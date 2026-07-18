// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
// `Prot::RO_DATA` and `AddrSpace::{walk_pt, destroy}` are part of the API
// surface that later Phase 3 slices consume (region tracking, exec/exit
// teardown); 3.1a's smoke test and 3.3's `VMCTL_PT_MAP`/`PT_UNMAP` exercise
// `Prot::RW_DATA`, `map_page_in`, and `unmap_page_in`. Surfacing the rest now
// keeps the API stable across slices.
#![allow(dead_code)]

//! Per-process aarch64 address space.
//!
//! Slice 3.1a introduces this API but no proc consumes it yet — the kmain
//! smoke test exercises it standalone, and slice 3.1b will swap `userland.rs`
//! over so each EL0 stub gets its own [`AddrSpace`] instead of the shared
//! static L0/L1/L2/L3 arrays.
//!
//! The walk follows the same 4 KiB-granule, 4-level, 48-bit-VA layout that
//! [`super::mmu`] already documents and verifies on Limine handoff. Intermediate
//! tables (L1, L2, L3) are allocated on demand from the frame allocator —
//! [`AddrSpace::new`] only allocates the L0 root; each `map_page` walk
//! allocates whatever sub-tables are missing.
//!
//! All page-table writes go through HHDM — we never touch a table via its
//! own VA, because at slice-3.1a time the address space isn't activated.
//! Slice 3.1b's context switch will install [`AddrSpace::ttbr0_pa`] into
//! TTBR0_EL1; until then, AddrSpaces are passive data structures.

use crate::arch::aarch64::mmu::{
    ATTR_IDX_NORMAL, PAGE_SHIFT, PTE_AF, PTE_AP_RO_EL0, PTE_AP_RW_EL0, PTE_PXN, PTE_SH_INNER,
    PTE_TABLE, PTE_UXN, PTE_VALID, PTES_PER_LEVEL, pte_attr_idx,
};
use crate::mm::{FRAME_SIZE, Frame, alloc_frame, free_frame, phys_to_hhdm};

/// User-page permission. Maps onto the aarch64 stage-1 descriptor's AP +
/// PXN + UXN bits; kernel callers describe intent here and `prot_attrs()`
/// converts to the bit pattern.
#[derive(Copy, Clone, Debug)]
pub struct Prot {
    /// EL0 may write.
    pub writable: bool,
    /// EL0 may fetch from this page (code).
    pub executable: bool,
}

impl Prot {
    pub const RO_CODE: Self = Self {
        writable: false,
        executable: true,
    };
    pub const RW_DATA: Self = Self {
        writable: true,
        executable: false,
    };
    pub const RO_DATA: Self = Self {
        writable: false,
        executable: false,
    };
}

fn prot_attrs(prot: Prot) -> u64 {
    let ap = if prot.writable {
        PTE_AP_RW_EL0
    } else {
        PTE_AP_RO_EL0
    };
    let uxn = if prot.executable { 0 } else { PTE_UXN };
    PTE_AF | PTE_SH_INNER | ap | PTE_PXN | uxn | pte_attr_idx(ATTR_IDX_NORMAL)
}

/// Errors from [`AddrSpace`] operations.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MapError {
    /// Frame allocator returned `None`; no physical memory left for an
    /// intermediate page table or for the L0 root.
    OutOfMemory,
    /// `va` is not 4 KiB-aligned.
    Misaligned,
    /// `va` is outside the 48-bit user-VA window.
    OutOfRange,
    /// A leaf PTE is already present at `va` and the caller asked for a
    /// fresh mapping. Use [`AddrSpace::unmap_page`] to clear first.
    AlreadyMapped,
}

/// One per-process page-table tree, rooted at the L0 frame whose PA goes
/// into TTBR0_EL1 on context switch.
pub struct AddrSpace {
    /// PA of the L0 page-table root. Always frame-aligned.
    pub ttbr0_pa: u64,
}

impl AddrSpace {
    /// Build a fresh, empty address space. Allocates one frame for the L0
    /// root; intermediate L1/L2/L3 tables come later, on first `map_page`.
    pub fn new() -> Result<Self, MapError> {
        let l0 = alloc_frame().ok_or(MapError::OutOfMemory)?;
        // `alloc_frame` zeroes the frame, so the L0 starts all-invalid.
        Ok(Self {
            ttbr0_pa: l0.addr(),
        })
    }

    /// Install a single 4 KiB mapping `va → pa` with the given permissions.
    ///
    /// Intermediate L1/L2/L3 tables are allocated from the frame allocator
    /// as needed. Returns `Err(AlreadyMapped)` if `va` already has a leaf
    /// PTE (callers must `unmap_page` first to replace).
    pub fn map_page(&mut self, va: u64, pa: u64, prot: Prot) -> Result<(), MapError> {
        map_page_in(self.ttbr0_pa, va, pa, prot)
    }

    /// Clear the leaf PTE at `va`. Returns `Some(pa)` if a mapping was
    /// present, `None` otherwise. The freed PA is returned to the caller —
    /// it's the caller's job to `free_frame` if appropriate (the page
    /// might be a CoW share, a device PA, or stale; the address space
    /// itself doesn't know).
    ///
    /// Does not free intermediate L1/L2/L3 tables even if they become
    /// empty — that's left to [`Self::destroy`]. Avoiding incremental
    /// pruning keeps `unmap_page` O(walk) and matches what real MINIX 3
    /// does (lazy pruning at exec/exit time).
    pub fn unmap_page(&mut self, va: u64) -> Option<u64> {
        unmap_page_in(self.ttbr0_pa, va)
    }

    /// Walk the page table to find the PA backing `va`, or `None` if `va`
    /// is unmapped.
    pub fn walk_pt(&self, va: u64) -> Option<u64> {
        check_va(va).ok()?;
        let l0 = self.ttbr0_pa;
        let l1 = next_table(l0, pte_index(va, 0))?;
        let l2 = next_table(l1, pte_index(va, 1))?;
        let l3 = next_table(l2, pte_index(va, 2))?;
        let pte = table_ref(l3)[pte_index(va, 3)];
        if pte & PTE_VALID == 0 {
            None
        } else {
            Some(pte & PA_MASK)
        }
    }

    /// Free every intermediate L1/L2/L3 table and the L0 root.
    ///
    /// Leaf-page frames (the PAs that `map_page` installed) are *not*
    /// freed — the address space owns the page-table tree, not the pages
    /// it points at. The caller is responsible for tracking and freeing
    /// leaf frames before calling this; otherwise they leak. Phase 3.5's
    /// VM region tracker is what eventually owns leaf-frame lifetimes.
    pub fn destroy(self) {
        // Walk the L0 root, recursing into table descriptors. Page
        // descriptors (PTE_VALID & PTE_TABLE both set at L3) are leaves
        // and left alone; intermediate tables (PTE_VALID & PTE_TABLE at
        // L0/L1/L2) get recursed-into then freed.
        free_subtree(self.ttbr0_pa, 0);
        free_frame(Frame::from_addr(self.ttbr0_pa));
    }
}

// ----- helpers --------------------------------------------------------------

/// Install a single 4 KiB mapping `va → pa` into the page-table tree rooted
/// at `ttbr0_pa`, allocating intermediate L1/L2/L3 tables on demand.
///
/// This is the body of [`AddrSpace::map_page`], exposed as a free function
/// so the kernel can map into a proc's *live* address space given only the
/// `ttbr0_pa` stored on its [`Proc`](crate::proc::Proc) slot — without
/// reconstructing an owning [`AddrSpace`] value (whose future `destroy`
/// semantics would risk freeing a tree it does not own). The slice-3.2
/// page-fault handler and slice-3.3's `VMCTL_PT_MAP` both go through here.
///
/// All table access is via HHDM; the target AS need not be the active one.
pub fn map_page_in(ttbr0_pa: u64, va: u64, pa: u64, prot: Prot) -> Result<(), MapError> {
    check_va(va)?;
    if pa & (FRAME_SIZE as u64 - 1) != 0 {
        return Err(MapError::Misaligned);
    }

    let l1 = ensure_next_table(ttbr0_pa, pte_index(va, 0))?;
    let l2 = ensure_next_table(l1, pte_index(va, 1))?;
    let l3 = ensure_next_table(l2, pte_index(va, 2))?;
    let l3_table = table_mut(l3);
    let idx = pte_index(va, 3);
    if l3_table[idx] & PTE_VALID != 0 {
        return Err(MapError::AlreadyMapped);
    }
    l3_table[idx] = make_page_desc(pa, prot_attrs(prot));
    Ok(())
}

/// Clear the leaf PTE at `va` in the page-table tree rooted at `ttbr0_pa`,
/// returning `Some(pa)` of the page that was mapped or `None` if `va` had no
/// leaf PTE.
///
/// This is the body of [`AddrSpace::unmap_page`], exposed as a free function
/// for the same reason as [`map_page_in`]: slice-3.3's `VMCTL_PT_UNMAP` clears
/// a PTE in a proc's *live* tree given only the `ttbr0_pa` from its
/// [`Proc`](crate::proc::Proc) slot. Intermediate L1/L2/L3 tables are left in
/// place (lazy pruning happens at [`AddrSpace::destroy`]); the freed PA is the
/// caller's to `free_frame` if appropriate.
pub fn unmap_page_in(ttbr0_pa: u64, va: u64) -> Option<u64> {
    check_va(va).ok()?;
    let l1 = next_table(ttbr0_pa, pte_index(va, 0))?;
    let l2 = next_table(l1, pte_index(va, 1))?;
    let l3 = next_table(l2, pte_index(va, 2))?;
    let l3_table = table_mut(l3);
    let idx = pte_index(va, 3);
    let pte = l3_table[idx];
    if pte & PTE_VALID == 0 {
        return None;
    }
    l3_table[idx] = 0;
    Some(pte & PA_MASK)
}

/// Visit every valid L3 leaf mapping in the tree rooted at `ttbr0_pa`,
/// calling `f(va, pa, prot)` for each in ascending-VA order.
///
/// This is the enumeration primitive slice 4.6 needs twice: `SYS_FORK` copies
/// every parent page into a fresh child tree, and `SYS_EXIT` frees every leaf
/// frame before [`AddrSpace::destroy`] (which only frees the tables). `f` may
/// allocate frames and write *other* trees, but must not modify this one —
/// each table is snapshotted before its entries are walked (the
/// [`free_subtree`] pattern), so mid-walk edits to `ttbr0_pa`'s tree would be
/// silently ignored, not honored. Returning `Err` aborts the walk (fork's
/// out-of-memory unwind).
pub fn walk_leaves(
    ttbr0_pa: u64,
    f: &mut dyn FnMut(u64, u64, Prot) -> Result<(), MapError>,
) -> Result<(), MapError> {
    let l0: [u64; PTES_PER_LEVEL] = *table_ref(ttbr0_pa);
    for (i0, d0) in l0.iter().enumerate() {
        if !is_table_desc(*d0) {
            continue;
        }
        let l1: [u64; PTES_PER_LEVEL] = *table_ref(d0 & PA_MASK);
        for (i1, d1) in l1.iter().enumerate() {
            if !is_table_desc(*d1) {
                continue;
            }
            let l2: [u64; PTES_PER_LEVEL] = *table_ref(d1 & PA_MASK);
            for (i2, d2) in l2.iter().enumerate() {
                if !is_table_desc(*d2) {
                    continue;
                }
                let l3: [u64; PTES_PER_LEVEL] = *table_ref(d2 & PA_MASK);
                for (i3, d3) in l3.iter().enumerate() {
                    if *d3 & PTE_VALID == 0 {
                        continue;
                    }
                    let va = ((i0 as u64) << 39)
                        | ((i1 as u64) << 30)
                        | ((i2 as u64) << 21)
                        | ((i3 as u64) << PAGE_SHIFT);
                    f(va, *d3 & PA_MASK, pte_prot(*d3))?;
                }
            }
        }
    }
    Ok(())
}

/// The AP bit that distinguishes read-only from read-write at EL0: set in
/// `PTE_AP_RO_EL0` (0b11 << 6), clear in `PTE_AP_RW_EL0` (0b01 << 6).
const PTE_AP_READONLY_BIT: u64 = PTE_AP_RO_EL0 & !PTE_AP_RW_EL0;

/// Recover the mapping intent from a leaf page descriptor — the inverse of
/// [`prot_attrs`]. Writable ⇔ the AP read-only bit is clear; executable ⇔
/// UXN is clear. Fork uses this to re-install a copied page with the same
/// permissions the parent had.
fn pte_prot(pte: u64) -> Prot {
    Prot {
        writable: pte & PTE_AP_READONLY_BIT == 0,
        executable: pte & PTE_UXN == 0,
    }
}

/// True when `desc` is a valid table descriptor (at L0–L2 — the only kind of
/// valid entry [`ensure_next_table`] writes at those levels; a valid entry
/// with `PTE_TABLE` clear would be a block descriptor, which this kernel
/// never maps).
fn is_table_desc(desc: u64) -> bool {
    desc & (PTE_VALID | PTE_TABLE) == (PTE_VALID | PTE_TABLE)
}

/// Mask selecting the PA bits of a descriptor (47:12).
const PA_MASK: u64 = 0x0000_FFFF_FFFF_F000;

/// One 48-bit user VA past the last legal address: `1 << 48`.
const USER_VA_TOP: u64 = 1 << 48;

fn check_va(va: u64) -> Result<(), MapError> {
    if va & (FRAME_SIZE as u64 - 1) != 0 {
        return Err(MapError::Misaligned);
    }
    if va >= USER_VA_TOP {
        return Err(MapError::OutOfRange);
    }
    Ok(())
}

fn pte_index(va: u64, level: u32) -> usize {
    let shift = PAGE_SHIFT + 9 * (3 - level);
    ((va >> shift) & 0x1FF) as usize
}

const fn make_table_desc(next_table_pa: u64) -> u64 {
    (next_table_pa & PA_MASK) | PTE_VALID | PTE_TABLE
}

const fn make_page_desc(pa: u64, attrs: u64) -> u64 {
    // At L3, both PTE_VALID and PTE_TABLE bits must be set to distinguish a
    // page descriptor from an invalid entry. `mmu::make_page_desc` documents
    // this; we mirror the same encoding.
    (pa & PA_MASK) | PTE_VALID | PTE_TABLE | attrs
}

/// Borrow the 512-entry table at `table_pa` via HHDM for read.
fn table_ref(table_pa: u64) -> &'static [u64; PTES_PER_LEVEL] {
    // SAFETY: `table_pa` came from `alloc_frame`, so it is HHDM-mapped and
    // 4 KiB-aligned. Single-threaded boot — no concurrent writer.
    unsafe { &*(phys_to_hhdm(table_pa) as *const [u64; PTES_PER_LEVEL]) }
}

/// Borrow the 512-entry table at `table_pa` via HHDM for mutation.
fn table_mut(table_pa: u64) -> &'static mut [u64; PTES_PER_LEVEL] {
    // SAFETY: `table_pa` came from `alloc_frame`, so it is HHDM-mapped and
    // 4 KiB-aligned. Single-threaded boot — no concurrent reader/writer.
    unsafe { &mut *(phys_to_hhdm(table_pa) as *mut [u64; PTES_PER_LEVEL]) }
}

/// Return the PA of the child table at `parent[idx]`, or `None` if the slot
/// is empty or holds a non-table descriptor.
fn next_table(parent_pa: u64, idx: usize) -> Option<u64> {
    let desc = table_ref(parent_pa)[idx];
    if desc & PTE_VALID == 0 {
        return None;
    }
    // At L0/L1/L2, a table descriptor has PTE_TABLE=1; a valid entry with
    // PTE_TABLE=0 is a *block* descriptor (2 MiB / 1 GiB block) and must
    // NOT be followed as a sub-table. Slice 3.1a only ever writes table
    // descriptors via `ensure_next_table`, so this branch never fires
    // today — the check is future-proofing for any later slice that maps
    // a block (large-page support, identity ranges, etc.). (L3 page
    // descriptors also have PTE_TABLE=1, but we never call `next_table`
    // at L3.)
    if desc & PTE_TABLE == 0 {
        return None;
    }
    Some(desc & PA_MASK)
}

/// Return the PA of the child table at `parent[idx]`, allocating and
/// linking it if absent.
fn ensure_next_table(parent_pa: u64, idx: usize) -> Result<u64, MapError> {
    let parent = table_mut(parent_pa);
    let desc = parent[idx];
    if desc & PTE_VALID != 0 {
        return Ok(desc & PA_MASK);
    }
    let child = alloc_frame().ok_or(MapError::OutOfMemory)?;
    parent[idx] = make_table_desc(child.addr());
    Ok(child.addr())
}

/// Recursively free every intermediate table under `table_pa` at the given
/// `level` (0 = L0 root, 3 = L3 leaves). Page descriptors at L3 are left
/// alone (caller owns leaf frames); the table itself at L3 is freed by
/// the L2 recursion call. The L0 root is freed by [`AddrSpace::destroy`].
fn free_subtree(table_pa: u64, level: u32) {
    if level >= 3 {
        // L3 contains page descriptors, not table descriptors — nothing to
        // recurse into. The L3 frame itself was already linked at L2 and
        // will be freed by the L2 caller.
        return;
    }
    // Snapshot the entries into a local so the HHDM borrow of `table_pa`
    // ends before the recursive call (which takes fresh borrows of sibling
    // tables — overlapping `&` is fine for shared reads, but copying makes
    // the intent unambiguous).
    let entries: [u64; PTES_PER_LEVEL] = *table_ref(table_pa);
    for desc in entries.iter() {
        if *desc & PTE_VALID == 0 {
            continue;
        }
        let child_pa = *desc & PA_MASK;
        // Recurse to free grandchildren first, then free this child table.
        free_subtree(child_pa, level + 1);
        free_frame(Frame::from_addr(child_pa));
    }
}
