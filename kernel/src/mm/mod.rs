//! Kernel memory management — physical frame allocator and the address-space
//! API live here. Slice 3.1a introduces both; Phase 2.x code did not need
//! either (page tables were static `.bss` arrays shared across all stubs).
//!
//! The frame allocator owns every 4 KiB frame in regions Limine marks
//! `MEMMAP_USABLE`. Frames inside the kernel image, the embedded boot image,
//! and the static stub pages from slices 2.5/2.6 live in
//! `EXECUTABLE_AND_MODULES` and are therefore never visible to this
//! allocator — no explicit reservation needed.
//!
//! Slice 3.1a runs single-threaded with interrupts masked, like the rest of
//! the boot path; the allocator carries the same `unsafe impl Sync` boot
//! invariant the [`crate::proc::table`] tables document. Phase 3.2+ will
//! resolve page faults *outside* fault context (the handler enqueues, then
//! the scheduler runs the resolver), so the allocator never runs from
//! within a fault path.

pub mod frame;

pub use frame::{Frame, FRAME_SIZE, alloc_frame, free_frame, init_from_limine_memmap};

/// Translate a physical address to its kernel HHDM virtual address.
///
/// The HHDM offset is captured at `init` time from Limine's response;
/// all of `[0, 4 GiB)` is mapped under base revision 2 (see
/// `arch::aarch64::limine`). Out-of-range PAs will produce a kernel VA
/// that faults on access.
pub fn phys_to_hhdm(pa: u64) -> *mut u8 {
    (pa + hhdm_offset()) as *mut u8
}

static mut HHDM_OFFSET: u64 = 0;

/// Set the HHDM offset captured from Limine. Must be called once during
/// boot before any frame allocation.
///
/// SAFETY: must be called exactly once, single-threaded, before any
/// concurrent use of [`phys_to_hhdm`].
pub unsafe fn set_hhdm_offset(off: u64) {
    // SAFETY: caller's contract — single-threaded boot, single writer.
    unsafe { HHDM_OFFSET = off };
}

pub(crate) fn hhdm_offset() -> u64 {
    // SAFETY: written exactly once at boot before any reader; the value is
    // a plain `u64` so a torn read is impossible on aarch64.
    unsafe { HHDM_OFFSET }
}
