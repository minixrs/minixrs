//! Minimal GICv3 driver for QEMU `virt` (cortex-a72).
//!
//! Targets the GICv3 distributor + redistributor + CPU-interface (system
//! register) surface needed to deliver the ARM virtual timer's PPI 27 to
//! EL1. Scope is deliberately narrow:
//!
//! - Single CPU (CPU 0). No MPIDR routing, no SGI broadcasts.
//! - PPIs only. SPI configuration (IRQ ≥ 32) lands when device drivers
//!   arrive in Phase 6.
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
use core::sync::atomic::{AtomicUsize, Ordering};

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
    let hhdm = limine::hhdm_offset()
        .expect("GICv3 init: Limine did not provide HHDM offset")
        as usize;
    GICD_VBASE.store(hhdm + GICD_PHYS_BASE, Ordering::Release);
    GICR_VBASE.store(hhdm + GICR_PHYS_BASE, Ordering::Release);

    // ----- Distributor -----------------------------------------------------
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

/// Acknowledge the highest-priority pending Group-1 interrupt. Returns the
/// INTID; `INTID_SPURIOUS` (1023) means no interrupt was actually pending.
///
/// SAFETY: must only be called from IRQ context.
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

fn wait_gicd_rwp() {
    while unsafe { read32(gicd() + GICD_CTLR) } & GICD_CTLR_RWP_BIT != 0 {
        core::hint::spin_loop();
    }
}
