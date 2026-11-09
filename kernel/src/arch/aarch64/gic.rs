// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Minimal GICv3 driver for QEMU `virt` (cortex-a72).
//!
//! Targets the GICv3 distributor + redistributor + CPU-interface (system
//! register) surface needed to deliver the ARM virtual timer's PPI 27 to
//! EL1. Scope is deliberately narrow:
//!
//! - Single CPU (CPU 0). No MPIDR routing, no SGI broadcasts.
//! - PPIs for the kernel's own timer, plus SPIs (INTID ≥ 32) on behalf of
//!   `SYS_IRQCTL` (slice 6.1, spec I9): [`enable_spi`], [`mask_spi`],
//!   [`unmask_spi`], with [`nr_intids`] bounding which lines exist.
//! - Group 1 non-secure interrupts only.
//!
//! Register layout reference: ARM IHI 0069H, §12 (Distributor) and §13
//! (Redistributor). System-register names match the ARMv8-A spec.
//!
//! QEMU `-M virt -cpu cortex-a72` memory map:
//!
//! | Region                | Phys base        | Size  |
//! |-----------------------|------------------|-------|
//! | GICD                  | `0x0800_0000`    | 64 KB |
//! | GICR (CPU 0 RD page)  | `0x080A_0000`    | 64 KB |
//! | GICR (CPU 0 SGI page) | `0x080A_0000+0x10000` | 64 KB |
//!
//! Both bases fall inside Limine base-revision-2's `[0, 4 GiB)` blanket
//! map, so we reach them through `HHDM_offset + phys`.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use minixrs_kernel_shared::irq::FIRST_SPI;

use super::limine;

// ---------------------------------------------------------------------------
// MMIO base addresses on QEMU virt (cortex-a72).
// ---------------------------------------------------------------------------

pub const GICD_PHYS_BASE: usize = 0x0800_0000;
pub const GICR_PHYS_BASE: usize = 0x080A_0000;
const GICR_SGI_OFFSET: usize = 0x1_0000;

// GICD register offsets.
const GICD_CTLR: usize = 0x0000;
const GICD_CTLR_RWP_BIT: u32 = 1 << 31;
const GICD_CTLR_ENABLE_GRP1_NS: u32 = 1 << 1;
const GICD_CTLR_ARE_NS: u32 = 1 << 4;
const GICD_TYPER: usize = 0x0004;
/// `GICD_TYPER.ITLinesNumber`, bits [4:0]: the distributor implements
/// `32 * (N + 1)` INTIDs.
const GICD_TYPER_ITLINES_MASK: u32 = 0x1F;
// SPI banks: one bit per INTID (32 per word) unless noted.
const GICD_IGROUPR: usize = 0x0080;
const GICD_ISENABLER: usize = 0x0100;
const GICD_ICENABLER: usize = 0x0180;
const GICD_IPRIORITYR: usize = 0x0400; // 8 bits per INTID.
const GICD_ICFGR: usize = 0x0C00; // 2 bits per INTID.
const GICD_IROUTER: usize = 0x6000; // 64 bits per INTID; SPIs only.
/// INTID 1020..1023 are special (1023 is spurious); no distributor implements
/// an SPI there, whatever `ITLinesNumber` says.
const MAX_INTIDS: u32 = 1020;

// GICR (RD page) register offsets.
const GICR_WAKER: usize = 0x0014;
const GICR_WAKER_PROCESSOR_SLEEP: u32 = 1 << 1;
const GICR_WAKER_CHILDREN_ASLEEP: u32 = 1 << 2;

// GICR (SGI page) register offsets.
const GICR_SGI_IGROUPR0: usize = 0x0080;
const GICR_SGI_ISENABLER0: usize = 0x0100;
const GICR_SGI_ICENABLER0: usize = 0x0180;
const GICR_SGI_IPRIORITYR0: usize = 0x0400; // 8 bits per intid; intids 0..31.
const GICR_SGI_ICFGR1: usize = 0x0C04; // 2 bits per intid; intids 16..31.

// Spurious / no-pending INTID returned by ICC_IAR1_EL1.
pub const INTID_SPURIOUS: u32 = 1023;

// ---------------------------------------------------------------------------
// Resolved virtual bases (set in `init` from the HHDM offset).
// ---------------------------------------------------------------------------

static GICD_VBASE: AtomicUsize = AtomicUsize::new(0);
static GICR_VBASE: AtomicUsize = AtomicUsize::new(0);
/// INTIDs the distributor implements, from `GICD_TYPER` in `init`. 0 until
/// then, so every SPI range check fails closed before the GIC is up.
static NR_INTIDS: AtomicU32 = AtomicU32::new(0);

fn gicd() -> usize {
    GICD_VBASE.load(Ordering::Acquire)
}

fn gicr_rd() -> usize {
    GICR_VBASE.load(Ordering::Acquire)
}

fn gicr_sgi() -> usize {
    gicr_rd() + GICR_SGI_OFFSET
}

// ---------------------------------------------------------------------------
// Public API.
// ---------------------------------------------------------------------------

/// Initialize the GICv3 distributor, this CPU's redistributor, and the
/// CPU interface (system registers). Idempotent only in the sense that
/// re-running it would clobber the same state; callers should only invoke
/// this once during boot.
///
/// SAFETY: callable only on the boot CPU during single-threaded boot, with
/// IRQs masked at PSTATE.DAIF. `limine::hhdm_offset` must return `Some`.
pub unsafe fn init() {
    let hhdm =
        limine::hhdm_offset().expect("GICv3 init: Limine did not provide HHDM offset") as usize;
    GICD_VBASE.store(hhdm + GICD_PHYS_BASE, Ordering::Release);
    GICR_VBASE.store(hhdm + GICR_PHYS_BASE, Ordering::Release);

    // ----- Distributor -----------------------------------------------------
    // How many INTIDs exist (spec I9): `SYS_IRQCTL`'s range check uses this
    // rather than a constant, so a claim on a line the distributor does not
    // implement is `EINVAL`, never a write to a reserved register.
    // SAFETY: `gicd()` was resolved to the distributor's HHDM alias just above;
    // `GICD_TYPER` is a read-only ID register with no side effects.
    let itlines = unsafe { read32(gicd() + GICD_TYPER) } & GICD_TYPER_ITLINES_MASK;
    NR_INTIDS.store((32 * (itlines + 1)).min(MAX_INTIDS), Ordering::Release);

    // Disable while configuring, wait for RWP to clear.
    unsafe { write32(gicd() + GICD_CTLR, 0) };
    wait_gicd_rwp();
    // Enable Group 1 non-secure with Affinity Routing (ARE) on.
    unsafe {
        write32(
            gicd() + GICD_CTLR,
            GICD_CTLR_ENABLE_GRP1_NS | GICD_CTLR_ARE_NS,
        )
    };
    wait_gicd_rwp();

    // ----- Redistributor (this CPU) ---------------------------------------
    // Clear ProcessorSleep so the redistributor wakes up; poll
    // ChildrenAsleep until 0.
    let waker = unsafe { read32(gicr_rd() + GICR_WAKER) };
    unsafe { write32(gicr_rd() + GICR_WAKER, waker & !GICR_WAKER_PROCESSOR_SLEEP) };
    while unsafe { read32(gicr_rd() + GICR_WAKER) } & GICR_WAKER_CHILDREN_ASLEEP != 0 {
        core::hint::spin_loop();
    }

    // ----- CPU interface (system registers) -------------------------------
    // Enable system-register access (ICC_SRE_EL1.SRE = 1). Some GICs
    // require this *before* IGRPEN1_EL1 writes will stick.
    unsafe {
        let mut sre: u64;
        core::arch::asm!(
            "mrs {0}, ICC_SRE_EL1",
            out(reg) sre,
            options(nomem, nostack, preserves_flags),
        );
        sre |= 1; // SRE bit
        core::arch::asm!(
            "msr ICC_SRE_EL1, {0}",
            "isb",
            in(reg) sre,
            options(nomem, nostack, preserves_flags),
        );

        // Priority mask: allow all priorities (0xFF = lowest acceptable).
        core::arch::asm!(
            "msr ICC_PMR_EL1, {0}",
            in(reg) 0xFFu64,
            options(nomem, nostack, preserves_flags),
        );

        // Enable Group 1 interrupts at the CPU interface.
        core::arch::asm!(
            "msr ICC_IGRPEN1_EL1, {0}",
            "isb",
            in(reg) 1u64,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Configure and enable a Private Peripheral Interrupt (PPI; 16 ≤ intid < 32).
///
/// SAFETY: `init` must have completed; called single-threaded with IRQs
/// masked.
pub unsafe fn enable_ppi(intid: u32, priority: u8) {
    assert!(
        (16..32).contains(&intid),
        "enable_ppi: intid {intid} is not a PPI",
    );
    let bit = 1u32 << intid;

    // Disable while configuring.
    unsafe { write32(gicr_sgi() + GICR_SGI_ICENABLER0, bit) };

    // Priority: 8-bit slot at IPRIORITYR0 + intid.
    // We read-modify-write a 32-bit word to set just the target byte.
    let byte_off = intid as usize;
    let word_off = (byte_off / 4) * 4;
    let shift = (byte_off % 4) * 8;
    let mut pri = unsafe { read32(gicr_sgi() + GICR_SGI_IPRIORITYR0 + word_off) };
    pri = (pri & !(0xFFu32 << shift)) | ((priority as u32) << shift);
    unsafe { write32(gicr_sgi() + GICR_SGI_IPRIORITYR0 + word_off, pri) };

    // Configuration: level-triggered (00 in the 2-bit field). ICFGR1 covers
    // intids 16..31 with 2 bits each, indexed by (intid - 16).
    let cfg_shift = (intid - 16) * 2;
    let mut cfg = unsafe { read32(gicr_sgi() + GICR_SGI_ICFGR1) };
    cfg &= !(0b11u32 << cfg_shift); // 00 = level-triggered.
    unsafe { write32(gicr_sgi() + GICR_SGI_ICFGR1, cfg) };

    // Group 1 non-secure.
    let mut grp = unsafe { read32(gicr_sgi() + GICR_SGI_IGROUPR0) };
    grp |= bit;
    unsafe { write32(gicr_sgi() + GICR_SGI_IGROUPR0, grp) };

    // Enable.
    unsafe { write32(gicr_sgi() + GICR_SGI_ISENABLER0, bit) };
}

/// Number of INTIDs the distributor implements:
/// `min(32 * (GICD_TYPER.ITLinesNumber + 1), 1020)`, read once in [`init`].
/// The claimable SPIs are `FIRST_SPI..nr_intids()` (spec I3).
pub fn nr_intids() -> u32 {
    NR_INTIDS.load(Ordering::Acquire)
}

/// Panic unless `intid` is an implemented SPI. Every SPI register write goes
/// through this, so a bad INTID reaching here is a kernel bug (the handler's
/// `decode` already answered `EINVAL`), never a write past the SPI banks.
fn assert_spi(intid: u32) {
    assert!(
        intid >= FIRST_SPI && intid < nr_intids(),
        "gic: intid {intid} is not an implemented SPI",
    );
}

/// Configure and enable a Shared Peripheral Interrupt (spec I9): group 1 NS,
/// `priority`, level-triggered, routed to CPU 0, then unmasked.
///
/// The line is disabled first and only re-enabled after configuration:
/// `GICD_ICFGR` is UNPREDICTABLE to change while the line is enabled, and an
/// `IRQ_SETPOLICY` replacement re-runs this on a line that already is.
///
/// SAFETY: `init` must have completed; single-threaded EL1 with IRQs masked
/// (a kernel call or boot). `intid` must be an implemented SPI (asserted).
pub unsafe fn enable_spi(intid: u32, priority: u8) {
    assert_spi(intid);
    let i = intid as usize;
    let word = (i / 32) * 4;
    let bit = 1u32 << (i % 32);

    // SAFETY: `init` resolved `gicd()` to the HHDM alias of the distributor,
    // and `assert_spi` bounds every offset computed below inside the
    // implemented SPI banks; the caller's single-threaded, IRQs-masked context
    // means nothing else touches these registers meanwhile.
    unsafe {
        // Disable while configuring; RWP covers the ICENABLER write.
        write32(gicd() + GICD_ICENABLER + word, bit);
        wait_gicd_rwp();

        // Group 1 non-secure.
        let grp = read32(gicd() + GICD_IGROUPR + word);
        write32(gicd() + GICD_IGROUPR + word, grp | bit);

        // Priority: one byte per INTID; read-modify-write the containing word,
        // as `enable_ppi` does for the redistributor's copy.
        let pri_word = (i / 4) * 4;
        let pri_shift = (i % 4) * 8;
        let mut pri = read32(gicd() + GICD_IPRIORITYR + pri_word);
        pri = (pri & !(0xFFu32 << pri_shift)) | ((priority as u32) << pri_shift);
        write32(gicd() + GICD_IPRIORITYR + pri_word, pri);

        // Configuration: level-triggered (0b00 in the 2-bit field). Every
        // `virt` device line is level-triggered — the reason I7 masks before
        // EOI.
        let cfg_word = (i / 16) * 4;
        let cfg_shift = (i % 16) * 2;
        let cfg = read32(gicd() + GICD_ICFGR + cfg_word);
        write32(
            gicd() + GICD_ICFGR + cfg_word,
            cfg & !(0b11u32 << cfg_shift),
        );

        // Route to affinity 0.0.0.0 (CPU 0), IRM = 0. Required, not optional:
        // with `ARE_NS` on, an SPI's target comes only from `GICD_IROUTER<n>`.
        write64(gicd() + GICD_IROUTER + 8 * i, 0);

        // Enable.
        write32(gicd() + GICD_ISENABLER + word, bit);
    }
}

/// Mask an SPI at the distributor (`GICD_ICENABLER`) and wait for
/// `GICD_CTLR.RWP` to clear, so the line is provably disabled on return —
/// the property I7's mask-before-EOI depends on.
///
/// SAFETY: as [`enable_spi`].
pub unsafe fn mask_spi(intid: u32) {
    assert_spi(intid);
    let i = intid as usize;
    // SAFETY: `init` resolved `gicd()`; `assert_spi` bounds the offset.
    unsafe { write32(gicd() + GICD_ICENABLER + (i / 32) * 4, 1u32 << (i % 32)) };
    wait_gicd_rwp();
}

/// Unmask an SPI at the distributor (`GICD_ISENABLER`). The line keeps the
/// configuration [`enable_spi`] gave it.
///
/// SAFETY: as [`enable_spi`].
pub unsafe fn unmask_spi(intid: u32) {
    assert_spi(intid);
    let i = intid as usize;
    // SAFETY: `init` resolved `gicd()`; `assert_spi` bounds the offset.
    unsafe { write32(gicd() + GICD_ISENABLER + (i / 32) * 4, 1u32 << (i % 32)) };
}

/// Acknowledge the highest-priority pending Group-1 interrupt. Returns the
/// INTID; `INTID_SPURIOUS` (1023) means no interrupt was actually pending.
///
/// SAFETY: exactly two callers are sound, both through
/// `arch::aarch64::irq::irq_dispatch`, and both exclusive:
///
/// - **IRQ context** — the `el0_64_irq_entry` stub's `do_irq`, which runs with
///   `PSTATE.DAIF.I` set until `eret`;
/// - **the idle poll** — `proc::sched::schedule_next`'s empty-run-queue loop,
///   at EL1 after a `wfi` with `DAIF.I` still set (slice 6.1, spec I11). No
///   exception is taken, but nothing can preempt it either, so it is as
///   exclusive as IRQ context.
///
/// Any other caller could take an acknowledge that one of those two paths then
/// never sees, leaving the INTID active with no matching `eoi`.
pub unsafe fn ack() -> u32 {
    let intid: u64;
    // SAFETY: ICC_IAR1_EL1 is the documented EL1 IRQ acknowledge register
    // for Group-1 interrupts. Reads are side-effecting (mark the interrupt
    // active) and must be paired with an `eoi` of the same INTID.
    unsafe {
        core::arch::asm!(
            "mrs {0}, ICC_IAR1_EL1",
            out(reg) intid,
            options(nomem, nostack, preserves_flags),
        );
    }
    intid as u32
}

/// Signal end-of-interrupt for `intid`, deactivating the GIC's active state.
///
/// SAFETY: must be called exactly once per `ack`, with the value `ack`
/// returned (the GIC enforces matching).
pub unsafe fn eoi(intid: u32) {
    // SAFETY: ICC_EOIR1_EL1 is the matching EOI for ICC_IAR1_EL1.
    unsafe {
        core::arch::asm!(
            "msr ICC_EOIR1_EL1, {0}",
            in(reg) intid as u64,
            options(nomem, nostack, preserves_flags),
        );
    }
}

// ---------------------------------------------------------------------------
// MMIO helpers.
// ---------------------------------------------------------------------------

unsafe fn read32(addr: usize) -> u32 {
    // SAFETY: caller proves `addr` is the post-HHDM virtual address of a
    // GICv3 MMIO register reachable from EL1.
    unsafe { read_volatile(addr as *const u32) }
}

unsafe fn write32(addr: usize, value: u32) {
    // SAFETY: as above; caller establishes the addr/register validity.
    unsafe { write_volatile(addr as *mut u32, value) }
}

unsafe fn write64(addr: usize, value: u64) {
    // SAFETY: as above. `GICD_IROUTER<n>` is a 64-bit register and supports
    // 64-bit single-copy-atomic access (IHI 0069H §12.1.3).
    unsafe { write_volatile(addr as *mut u64, value) }
}

fn wait_gicd_rwp() {
    while unsafe { read32(gicd() + GICD_CTLR) } & GICD_CTLR_RWP_BIT != 0 {
        core::hint::spin_loop();
    }
}
