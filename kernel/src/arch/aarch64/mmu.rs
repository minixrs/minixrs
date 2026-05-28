//! Minimal MMU helpers for the slice-2.3 EL0 stub task.
//!
//! Slice 2.3 needs exactly two user mappings — one code page and one stack
//! page — installed via TTBR0_EL1. Phase 3's VM server takes this over and
//! replaces it with proper per-process address spaces; for now we hand-roll
//! a tiny 4 KiB-granule, 4-level page-table walk in static `.bss` storage.
//!
//! Layout decisions:
//! - 4 KiB granule, 4 levels, T0SZ = 16 (48-bit VA). Symmetric with the
//!   TTBR1 settings Limine programmed for the higher-half kernel; using the
//!   same granule for TTBR0 lets us reuse Limine's MAIR indices unchanged.
//! - We never write `MAIR_EL1` — Limine has already placed `Normal WB` at
//!   AttrIdx 0. We read once at boot and assert that invariant.
//! - We touch only the `*0` field-set bits of `TCR_EL1` (T0SZ / IRGN0 /
//!   ORGN0 / SH0 / TG0 / EPD0) — the `*1` fields govern TTBR1, which is the
//!   kernel's own translation regime and must not be perturbed.

use core::arch::asm;

/// 4 KiB page size, in bytes.
pub const PAGE_SIZE: usize = 4096;
/// log2 of [`PAGE_SIZE`].
pub const PAGE_SHIFT: u32 = 12;
/// 512 entries per 4 KiB page table.
pub const PTES_PER_LEVEL: usize = 512;

// ----- PTE bit positions ----------------------------------------------------
//
// ARMv8 stage-1 descriptor format (D5.3 in the ARM ARM). The bits we use:

/// Entry is valid. Cleared = "translation fault on access".
pub const PTE_VALID: u64 = 1 << 0;
/// At L0/L1/L2: entry points at a next-level table (vs. a block). At L3:
/// must be 1 to indicate a page descriptor (block descriptors don't exist
/// at L3).
pub const PTE_TABLE: u64 = 1 << 1;
/// Inner Shareable shareability attribute. Mandatory for cacheable mappings
/// on multi-core systems; harmless on single-core QEMU.
pub const PTE_SH_INNER: u64 = 0b11 << 8;
/// Access Flag — must be set, or the first access generates a fault.
pub const PTE_AF: u64 = 1 << 10;
/// Privileged eXecute Never — kernel cannot fetch instructions from this
/// page. Always set on user mappings.
pub const PTE_PXN: u64 = 1 << 53;
/// Unprivileged eXecute Never — EL0 cannot fetch instructions. Set on
/// data/stack pages; cleared on code pages.
pub const PTE_UXN: u64 = 1 << 54;

/// Access-permission field selecting EL1 RW + EL0 RW.
pub const PTE_AP_RW_EL0: u64 = 0b01 << 6;
/// Access-permission field selecting EL1 RO + EL0 RO.
pub const PTE_AP_RO_EL0: u64 = 0b11 << 6;

/// Pack a MAIR attribute index into the descriptor's AttrIndx field.
pub const fn pte_attr_idx(idx: u64) -> u64 {
    (idx & 0b111) << 2
}

/// MAIR_EL1 AttrIdx 0 — must encode Normal memory, Write-Back, RAWA. Limine
/// programs this; slice 2.3 only verifies.
pub const ATTR_IDX_NORMAL: u64 = 0;
/// Expected MAIR_EL1[7:0] for AttrIdx 0.
pub const MAIR_NORMAL_BYTE: u8 = 0xFF;

// ----- TTBR0 / TCR / MAIR ---------------------------------------------------
//
// Slice 3.1a's [`super::addrspace::AddrSpace`] owns the page-table walker
// now; it consumes the PTE bit constants above and the frame allocator to
// build per-proc trees in HHDM. Slice 2.5's static `PageTable` newtype,
// `map_4k` helper, and `pte_index`/`make_*_desc` const fns lived here as
// the only consumers — they're gone in slice 3.1b along with the static
// userland page-table arrays.

/// Verify Limine programmed AttrIdx 0 = Normal WB. We rely on this index
/// throughout slice 2.3.
pub fn assert_mair_normal_wb() {
    let mair: u64;
    // SAFETY: MAIR_EL1 is readable at EL1; no side effects.
    unsafe {
        asm!("mrs {0}, mair_el1", out(reg) mair, options(nomem, nostack, preserves_flags));
    }
    assert!(
        (mair & 0xFF) as u8 == MAIR_NORMAL_BYTE,
        "MAIR_EL1[7:0] = {:#04x}, expected Normal WB = {:#04x}",
        (mair & 0xFF) as u8,
        MAIR_NORMAL_BYTE,
    );
}

/// Verify Limine programmed TCR_EL1's TTBR0-side walk structure the way our
/// page-table math assumes: 48-bit VA (T0SZ=16) so the walk has 4 levels,
/// and 4 KiB granule (TG0=0) so `pte_index` shifts by 12+9*… correctly.
/// IRGN0/ORGN0/SH0 are perf-only on single-core slice 2.3 and are reported
/// in the panic message but not enforced; `activate_user_ttbr0` leaves them
/// as Limine set them.
pub fn assert_tcr_el1_ttbr0_ready() {
    let tcr: u64;
    // SAFETY: TCR_EL1 is readable at EL1; no side effects.
    unsafe {
        asm!("mrs {0}, tcr_el1", out(reg) tcr, options(nomem, nostack, preserves_flags));
    }

    let t0sz = tcr & 0x3F;
    let tg0 = (tcr >> 14) & 0x3;
    let irgn0 = (tcr >> 8) & 0x3;
    let orgn0 = (tcr >> 10) & 0x3;
    let sh0 = (tcr >> 12) & 0x3;

    assert!(
        t0sz == 16 && tg0 == 0b00,
        "TCR_EL1 = {tcr:#018x} (T0SZ={t0sz} TG0={tg0} IRGN0={irgn0} ORGN0={orgn0} SH0={sh0}); \
         slice 2.3 requires T0SZ=16 (48-bit VA, 4-level walk) and TG0=0 (4 KiB granule)",
    );

    // Slice 3.1b: assert 8-bit ASIDs (TCR_EL1.AS = 0). Limine's aarch64
    // default leaves this bit clear, putting ASID at TTBR0_EL1[55:48].
    // If it ever flips to 16-bit, our `switch_ttbr0_with_asid` shift
    // would silently truncate.
    let as_bit = (tcr >> 36) & 1;
    assert!(
        as_bit == 0,
        "TCR_EL1.AS={as_bit}; slice 3.1b requires 8-bit ASIDs (AS=0). \
         Limine's aarch64 default is AS=0; if you see this, the bootloader changed.",
    );
}

/// Clear `TCR_EL1.EPD0` so the MMU starts walking TTBR0 on EL0 accesses.
///
/// Limine leaves EPD0 set by default; until this runs, every EL0 address
/// access translation-faults. Must be called exactly once during
/// `userland_bootstrap`, before any proc is enqueued onto the scheduler.
///
/// Bit 7 (EPD0) is the only TCR_EL1 field we perturb; the TTBR1-side
/// fields (which the kernel's own translation regime relies on, and which
/// Limine programmed) stay as Limine left them. TTBR1_EL1 is never written
/// from the kernel — Phase 3+ continues to own only TTBR0_EL1.
///
/// SAFETY: must be called with DAIF masked at EL1, single-threaded boot.
pub unsafe fn enable_ttbr0_walks_once() {
    const TCR_EPD0: u64 = 1 << 7;
    // SAFETY: caller's invariants. Read-modify-write of TCR_EL1 touches
    // only bit 7; ISB serializes the new TCR view before any subsequent
    // EL0 access can fault.
    unsafe {
        let tcr: u64;
        asm!(
            "mrs {0}, tcr_el1",
            out(reg) tcr,
            options(nomem, nostack, preserves_flags),
        );
        let new_tcr = tcr & !TCR_EPD0;
        asm!(
            "msr tcr_el1, {0}",
            "isb",
            in(reg) new_tcr,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Install a per-proc TTBR0 + ASID and invalidate stale TLB entries for
/// that ASID. Called from `proc::sched::schedule_next` on every EL1 → EL0
/// transition that picks a proc with `ttbr0_pa != 0`.
///
/// `ttbr0_pa` is the L0-root PA, frame-aligned. `asid` is in [1, 255];
/// caller is responsible for sourcing it from
/// [`super::asid::alloc_asid`].
///
/// The TLBI is unconditional. With three ASIDs in slice 3.1b, the cost is
/// negligible (~tens of cycles per switch on QEMU) and the simpler
/// control flow is worth more than micro-optimization. A later slice can
/// elide it once an "AS already activated" flag is added per AddrSpace.
///
/// Writes only TTBR0_EL1 — TTBR1 stays owned by Limine + the kernel's
/// translation regime. The kernel never installs a custom TTBR1.
///
/// SAFETY: must be called with DAIF masked at EL1, single-threaded boot.
/// `ttbr0_pa` must point at a valid L0 page table the caller owns.
pub unsafe fn switch_ttbr0_with_asid(ttbr0_pa: u64, asid: u8) {
    // Hard guards (not debug_assert!): the kernel only ever builds --release,
    // where debug_assert! is compiled out. A null/zero ASID here means a proc
    // with no address space reached the scheduler; installing TTBR0_EL1 = 0
    // would point the EL0 walk at PA 0. Fail loudly at the choke point.
    assert!(asid != 0, "switch_ttbr0_with_asid: ASID 0 is reserved");
    assert!(
        ttbr0_pa != 0 && ttbr0_pa & (PAGE_SIZE as u64 - 1) == 0,
        "switch_ttbr0_with_asid: ttbr0_pa {ttbr0_pa:#x} null or not page-aligned",
    );
    let tagged = ttbr0_pa | ((asid as u64) << 48);
    // `tlbi aside1, Xt` consumes the ASID from bits [63:48] of Xt; the
    // remaining bits are RES0 / SBZ. We pass the same `asid << 48` we
    // used for TTBR0 — keeps the encoding self-documenting.
    //
    // SAFETY: TTBR0_EL1 and TLBI ASIDE1 are EL1 ops with no normal-memory
    // access. The ISB after MSR ensures the new TTBR0 is observed before
    // the TLBI; the DSB ISH after the TLBI completes the invalidate; the
    // trailing ISB context-synchronizes before the subsequent eret.
    unsafe {
        asm!(
            "msr ttbr0_el1, {ttbr}",
            "isb",
            "tlbi aside1, {asid_x}",
            "dsb ish",
            "isb",
            ttbr = in(reg) tagged,
            asid_x = in(reg) (asid as u64) << 48,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Invalidate all TLB entries tagged with `asid`, without touching TTBR0.
///
/// Used after mutating a *currently-active* address space's page tables
/// (e.g. the slice-3.2 page-fault handler installing a heap page, or slice
/// 3.3's `VMCTL_PT_MAP`/`VMCTL_PT_UNMAP`). For a fresh translation-fault
/// resolve (invalid → valid) the TLBI is strictly redundant — ARMv8 does
/// not cache invalid entries — but it is required for permission-fault
/// resolves (valid → valid with new AP bits) and is cheap, so we always
/// issue it for uniformity.
///
/// SAFETY: EL1 op with no normal-memory access; must run with DAIF masked
/// in single-threaded boot/exception context. `asid` must be the live ASID
/// of the address space whose PTEs were just changed.
pub unsafe fn flush_tlb_asid(asid: u8) {
    // `tlbi aside1, Xt` reads the ASID from bits [63:48] of Xt; the rest is
    // RES0. `dsb ishst` orders the prior PTE store before the invalidate;
    // `dsb ish` completes it; the trailing `isb` context-synchronizes.
    //
    // SAFETY: see fn doc.
    unsafe {
        asm!(
            "dsb ishst",
            "tlbi aside1, {asid_x}",
            "dsb ish",
            "isb",
            asid_x = in(reg) (asid as u64) << 48,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Make instruction memory coherent with a recent data write at `va..va+len`.
///
/// AArch64 caches are PIPT for data but VIPT-flavored for instructions; after
/// writing into a code page we must clean the data side to PoU, then
/// invalidate the i-cache. Sequence is straight out of the ARM ARM §B2.4
/// example.
///
/// SAFETY: `va..va+len` must be a valid mapped range readable at EL1.
pub unsafe fn flush_icache_range(va: u64, len: usize) {
    // CTR_EL0.DminLine (bits 19:16) = log2 of the minimum D-cache line
    // size in words; multiply by the 4 B word size to get bytes. QEMU virt
    // reports 64 here; Apple Silicon reports 128.
    let ctr: u64;
    // SAFETY: CTR_EL0 is readable at EL1; no side effects.
    unsafe {
        asm!("mrs {0}, ctr_el0", out(reg) ctr, options(nomem, nostack, preserves_flags));
    }
    let line_bytes = 4_u64 << ((ctr >> 16) & 0xF);

    let mut p = va & !(line_bytes - 1);
    let end = va + len as u64;
    while p < end {
        // SAFETY: `dc cvau` / `ic ivau` are EL0-permissive cache ops on a
        // VA already known to be mapped; no memory access semantically.
        unsafe {
            asm!("dc cvau, {0}", in(reg) p, options(nostack, preserves_flags));
        }
        p += line_bytes;
    }
    // SAFETY: barrier + i-cache invalidation; no memory access.
    unsafe {
        asm!(
            "dsb ish",
            "ic iallu",
            "dsb ish",
            "isb",
            options(nomem, nostack, preserves_flags),
        );
    }
}
