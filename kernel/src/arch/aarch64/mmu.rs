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

// ----- PageTable -----------------------------------------------------------

/// One 4 KiB page table — 512 × 8 B descriptors. The structure is its own
/// alignment guarantee so we can place it in static storage and hand a raw
/// physical address straight to TTBR0.
#[repr(C, align(4096))]
pub struct PageTable(pub [u64; PTES_PER_LEVEL]);

impl PageTable {
    /// All-zero (= all-invalid) table for static initialization.
    pub const EMPTY: Self = Self([0; PTES_PER_LEVEL]);

    #[inline]
    pub fn set(&mut self, idx: usize, desc: u64) {
        self.0[idx] = desc;
    }
}

// ----- VA index helpers ----------------------------------------------------

const fn pte_index(va: u64, level: u32) -> usize {
    // Each level consumes 9 VA bits (4 KiB granule). L0 indexes bits
    // [47:39], L1 [38:30], L2 [29:21], L3 [20:12].
    let shift = PAGE_SHIFT + 9 * (3 - level);
    ((va >> shift) & 0x1FF) as usize
}

/// Build a table descriptor pointing at `next_table_pa`.
const fn make_table_desc(next_table_pa: u64) -> u64 {
    (next_table_pa & 0x0000_FFFF_FFFF_F000) | PTE_VALID | PTE_TABLE
}

/// Build an L3 page descriptor for `pa` with the given attribute bits.
const fn make_page_desc(pa: u64, attrs: u64) -> u64 {
    (pa & 0x0000_FFFF_FFFF_F000) | PTE_VALID | PTE_TABLE | attrs
}

// ----- Walker -------------------------------------------------------------

/// Install a single 4 KiB mapping from `va` to `pa` with `attrs`.
///
/// `tables` must hold at least 4 page tables: `tables[0]` is the L0 root
/// (whose PA goes into TTBR0), and the walker writes the rest of the walk
/// into `tables[1..]` if they are not already linked. The simple slice-2.3
/// arrangement uses one fixed table at each level, so callers pre-allocate
/// `[L0, L1, L2, L3_for_va]`.
///
/// SAFETY: `tables[i]` must be live for the duration of the mapping; their
/// physical addresses must be reachable via [`super::limine::kernel_va_to_pa`].
pub unsafe fn map_4k(
    l0: &mut PageTable,
    l1: &mut PageTable,
    l2: &mut PageTable,
    l3: &mut PageTable,
    l1_pa: u64,
    l2_pa: u64,
    l3_pa: u64,
    va: u64,
    pa: u64,
    attrs: u64,
) {
    l0.set(pte_index(va, 0), make_table_desc(l1_pa));
    l1.set(pte_index(va, 1), make_table_desc(l2_pa));
    l2.set(pte_index(va, 2), make_table_desc(l3_pa));
    l3.set(pte_index(va, 3), make_page_desc(pa, attrs));
}

// ----- TTBR0 / TCR / MAIR ---------------------------------------------------

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

/// Install `root_pa` as TTBR0_EL1 and ensure TTBR0 walks are enabled.
///
/// Limine has already programmed TCR_EL1 with T0SZ=16, TG0=4 KiB,
/// IRGN0=ORGN0=WBWA, SH0=Inner (verified by the diagnostic print in kmain),
/// so we only need to clear EPD0 if set. We deliberately read-modify-write
/// just bit 7 — touching any other TCR field risks perturbing TTBR1, which
/// Limine owns and whose translations the kernel is actively using.
///
/// SAFETY: must be called with interrupts masked. `root_pa` must point at a
/// valid L0 page table.
pub unsafe fn activate_user_ttbr0(root_pa: u64) {
    const TCR_EPD0: u64 = 1 << 7;

    // SAFETY: TTBR0_EL1, TCR_EL1, and TLB-maintenance ops are EL1-only and
    // touch no normal memory. Bit 7 (EPD0) is the only TCR field we
    // perturb; the rest stays as Limine left it.
    unsafe {
        let tcr: u64;
        asm!("mrs {0}, tcr_el1", out(reg) tcr, options(nomem, nostack, preserves_flags));
        let new_tcr = tcr & !TCR_EPD0;

        asm!(
            "msr ttbr0_el1, {root}",
            "msr tcr_el1, {tcr}",
            "isb",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            root = in(reg) root_pa,
            tcr = in(reg) new_tcr,
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
    // Cache-line size: read CTR_EL0.DminLine to get the data-side line size
    // in words (4 B). 0 means "default", but we always set it conservatively
    // to 64 bytes on QEMU virt (cortex-a72 default).
    const LINE_BYTES: u64 = 64;

    let mut p = va & !(LINE_BYTES - 1);
    let end = va + len as u64;
    while p < end {
        // SAFETY: `dc cvau` / `ic ivau` are EL0-permissive cache ops on a
        // VA already known to be mapped; no memory access semantically.
        unsafe {
            asm!("dc cvau, {0}", in(reg) p, options(nostack, preserves_flags));
        }
        p += LINE_BYTES;
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
