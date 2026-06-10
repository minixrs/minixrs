// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Physical frame allocator — 4 KiB granule, intrusive free list + per-region
//! bump pointers seeded from Limine's memmap.
//!
//! The allocator's state is just two pieces:
//!  - a `[Region; MAX_REGIONS]` array of `(base_pa, next_free_pa, end_pa)`
//!    bump pointers, one per `MEMMAP_USABLE` region Limine reported. `base_pa`
//!    is kept after init so `free_frame` can bounds-check freed PAs against
//!    the *original* extent, not the post-bump remainder;
//!  - a single free-list head pointer (kernel virtual address, HHDM-relative)
//!    of recently-freed frames.
//!
//! Allocation prefers the free list; if empty, bumps the first region with
//! room. Freed frames are pushed onto the free list — each frame's first
//! 8 bytes hold a pointer to the next free frame (the classic intrusive
//! free-list trick). This means each frame must be at least 8 bytes — which
//! is trivially true at 4 KiB.
//!
//! Capacity: MAX_REGIONS = 16 is enough for QEMU virt aarch64 (typically
//! 2–3 USABLE regions) and Apple Silicon QEMU (similar). If a board ever
//! reports more, `init_from_limine_memmap` panics with a clear message
//! pointing at this constant.
//!
//! Concurrency: single-threaded boot. The allocator is wrapped in an
//! `UnsafeCell` newtype with `unsafe impl Sync`, matching the convention
//! documented on [`crate::proc::table`]. Phase 3.2+'s page-fault flow runs
//! the allocator outside fault context — the handler sets `RTS_PAGEFAULT`
//! and returns to the scheduler, which then runs VM (or the kernel
//! resolver), which calls the allocator. The allocator therefore never
//! re-enters from an IRQ or fault.

use core::cell::UnsafeCell;
use core::ptr;

use crate::arch::aarch64::limine::{MEMMAP_USABLE, memmap_entries};

/// 4 KiB page / frame size, in bytes.
pub const FRAME_SIZE: usize = 4096;

/// Maximum number of `MEMMAP_USABLE` regions the allocator can track.
const MAX_REGIONS: usize = 16;

/// Identifies one 4 KiB physical frame by its page-frame number (PA >> 12).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Frame(u64);

impl Frame {
    /// Construct from a physical address. The address must be 4 KiB-aligned
    /// — non-aligned values are rejected at construction so downstream
    /// callers can rely on `frame.addr()` returning an aligned PA.
    pub const fn from_addr(pa: u64) -> Self {
        assert!(
            pa & (FRAME_SIZE as u64 - 1) == 0,
            "Frame::from_addr: PA not 4 KiB aligned"
        );
        Self(pa >> 12)
    }

    /// Physical address of this frame.
    pub const fn addr(self) -> u64 {
        self.0 << 12
    }
}

#[derive(Copy, Clone)]
struct Region {
    /// First PA in this region, captured at init time. Stays fixed for the
    /// life of the allocator so `free_frame` can check `base_pa <= pa`
    /// against the region's *original* extent — `next_free_pa` advances on
    /// every bump, so it can't double as the lower bound.
    base_pa: u64,
    /// Next PA to hand out from this region; equals `base_pa` until the
    /// first allocation, then advances toward `end_pa`.
    next_free_pa: u64,
    /// One-past-the-last PA in this region.
    end_pa: u64,
}

const EMPTY_REGION: Region = Region {
    base_pa: 0,
    next_free_pa: 0,
    end_pa: 0,
};

struct Allocator {
    regions: [Region; MAX_REGIONS],
    region_count: usize,
    /// Head of the free list, as an HHDM virtual address (so we can deref
    /// it directly without redoing the HHDM math on each `alloc_frame`).
    /// `null` means the free list is empty.
    free_head: *mut FreeNode,
    /// Statistics for diagnostics — incremented on every alloc/free.
    allocs: u64,
    frees: u64,
}

#[repr(C)]
struct FreeNode {
    next: *mut FreeNode,
}

#[repr(transparent)]
struct AllocatorCell(UnsafeCell<Allocator>);
// SAFETY: single-threaded boot invariant; see module-level comment.
unsafe impl Sync for AllocatorCell {}

static ALLOC: AllocatorCell = AllocatorCell(UnsafeCell::new(Allocator {
    regions: [EMPTY_REGION; MAX_REGIONS],
    region_count: 0,
    free_head: ptr::null_mut(),
    allocs: 0,
    frees: 0,
}));

/// Populate the allocator from Limine's memmap. Must be called once during
/// boot, before any [`alloc_frame`] / [`free_frame`] call.
///
/// `hhdm_offset` is Limine's HHDM offset (also passed to
/// [`crate::mm::set_hhdm_offset`] separately so the allocator can dereference
/// freed-frame pointers into a kernel VA).
///
/// SAFETY: must be called exactly once, single-threaded, after
/// [`crate::mm::set_hhdm_offset`].
pub unsafe fn init_from_limine_memmap() {
    let entries = memmap_entries().expect("Limine did not populate the memmap response");

    // SAFETY: single-threaded boot, single writer.
    let a = unsafe { &mut *ALLOC.0.get() };

    for entry in entries {
        if entry.kind != MEMMAP_USABLE {
            continue;
        }
        // Limine guarantees USABLE entries are 4 KiB-aligned in both base
        // and length — verify rather than silently dropping bytes.
        let base = entry.base;
        let len = entry.length;
        assert!(
            base & (FRAME_SIZE as u64 - 1) == 0,
            "Limine USABLE base {base:#x} is not 4 KiB-aligned"
        );
        assert!(
            len & (FRAME_SIZE as u64 - 1) == 0,
            "Limine USABLE length {len:#x} is not 4 KiB-aligned"
        );
        if len == 0 {
            continue;
        }
        assert!(
            a.region_count < MAX_REGIONS,
            "Limine reported more than MAX_REGIONS={MAX_REGIONS} usable regions; \
             grow the constant in mm::frame"
        );
        a.regions[a.region_count] = Region {
            base_pa: base,
            next_free_pa: base,
            end_pa: base.checked_add(len).expect("USABLE region overflows u64"),
        };
        a.region_count += 1;
    }

    assert!(
        a.region_count > 0,
        "Limine memmap contains no MEMMAP_USABLE regions"
    );
}

/// Allocate one 4 KiB frame. Returns `None` if no physical memory remains
/// (both the free list and every bump region are exhausted).
pub fn alloc_frame() -> Option<Frame> {
    // SAFETY: single-threaded boot invariant; see module-level comment.
    let a = unsafe { &mut *ALLOC.0.get() };

    // Free list takes priority — recently freed frames are still cache-hot.
    if !a.free_head.is_null() {
        // SAFETY: free_head is either null or an HHDM-VA pointer we set on a
        // previous `free_frame`. Reading the first 8 bytes of a free frame
        // through HHDM is sound because the frame is unmapped from all
        // address spaces (free means free) and HHDM is the only mapping.
        let node = a.free_head;
        let next = unsafe { (*node).next };
        a.free_head = next;
        a.allocs += 1;
        let pa = hhdm_vaddr_to_pa(node as u64);
        // Zero on the way out, same contract as the bump path below. A
        // free-list frame still holds the previous owner's bytes, and callers
        // depend on a clean frame: `addrspace::ensure_next_table` builds
        // intermediate page tables expecting all-invalid entries, and
        // `VMCTL_PT_MAP` hands the frame straight to EL0. Without this, a
        // reused frame would seed page tables with garbage PTEs and leak one
        // address space's data into another. `next` is already captured above,
        // so wiping the intrusive node here is fine.
        // SAFETY: HHDM covers this PA (Limine base revision 2 blanket-maps
        // [0, 4 GiB)), the frame is exclusively ours now, and HHDM is cacheable
        // normal memory — no MMIO side-channel that would require
        // `write_volatile`.
        unsafe {
            ptr::write_bytes(crate::mm::phys_to_hhdm(pa), 0, FRAME_SIZE);
        }
        return Some(Frame::from_addr(pa));
    }

    // Otherwise bump from the first region with room.
    for r in &mut a.regions[..a.region_count] {
        if r.next_free_pa < r.end_pa {
            let pa = r.next_free_pa;
            r.next_free_pa += FRAME_SIZE as u64;
            a.allocs += 1;
            // Zero before hand-out, same as the free-list path above, so every
            // frame `alloc_frame` returns is clean. Page-table walkers expect
            // intermediate tables to start all-invalid, and zero pages are
            // what user-mode brk/mmap expects too. Cost: one 4 KiB memset,
            // amortized over the frame's lifetime.
            // SAFETY: HHDM covers this PA (Limine base revision 2 blanket-maps
            // [0, 4 GiB)), the frame is exclusively ours, and HHDM is
            // cacheable normal memory — no MMIO side-channel that would
            // require `write_volatile`.
            unsafe {
                ptr::write_bytes(crate::mm::phys_to_hhdm(pa), 0, FRAME_SIZE);
            }
            return Some(Frame::from_addr(pa));
        }
    }
    None
}

/// Return one 4 KiB frame to the allocator. The frame is pushed onto the
/// free list; future `alloc_frame` calls will hand it back out (zeroed —
/// we re-zero on the alloc side, not the free side, so callers can free
/// without thinking about residual state).
///
/// Panics if `frame` lies outside the union of all tracked regions — that
/// would mean a caller forged a frame from a non-USABLE PA (e.g. a kernel-image
/// PA from `EXECUTABLE_AND_MODULES`, or a gap between two USABLE regions).
///
/// **Precondition (not checked):** the same frame must not be freed twice.
/// A double free silently corrupts the free list — the intrusive `next`
/// pointer on the freed frame gets overwritten with the previous head, and
/// the next two `alloc_frame` calls hand out the same PA. Detecting at
/// runtime would require an O(n) free-list walk per free (or extra
/// per-frame state); both are overkill for a single-threaded boot
/// allocator. The bounds check below catches forged-PA cases but cannot
/// distinguish a legitimate frame from one already on the list.
pub fn free_frame(frame: Frame) {
    let pa = frame.addr();

    // SAFETY: single-threaded boot invariant.
    let a = unsafe { &mut *ALLOC.0.get() };

    // Bounds-check against tracked regions. `base_pa` is captured at init
    // time and never advances, so `[base_pa, end_pa)` is the region's
    // original extent — exactly the set of legal frame PAs.
    let mut in_range = false;
    for r in &a.regions[..a.region_count] {
        if r.base_pa <= pa && pa < r.end_pa {
            in_range = true;
            break;
        }
    }
    assert!(
        in_range,
        "free_frame: PA {pa:#x} is outside all USABLE regions"
    );

    // Push onto the free list via HHDM.
    let vaddr = crate::mm::phys_to_hhdm(pa) as *mut FreeNode;
    // SAFETY: the frame is now owned by the allocator (caller relinquishes
    // it). HHDM mapping guarantees the VA is readable+writable.
    unsafe {
        (*vaddr).next = a.free_head;
    }
    a.free_head = vaddr;
    a.frees += 1;
}

fn hhdm_vaddr_to_pa(vaddr: u64) -> u64 {
    // HHDM is a flat shift; reversing is sub.
    vaddr - crate::mm::hhdm_offset()
}
