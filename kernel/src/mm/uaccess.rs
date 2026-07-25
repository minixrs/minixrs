// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Fault-safe access to user memory (slice 5.1, decision D5).
//!
//! Every byte the kernel moves in or out of a user address space goes through
//! here. The kernel **never dereferences a user VA directly** — not even the
//! active TTBR0's. Instead it walks the target's page tables
//! ([`walk_pt_in`]) and copies through the frame's HHDM alias.
//!
//! Two properties fall out of that, and both are the point:
//!
//! - **An unmapped page is a walk miss, not an exception.** It returns
//!   [`EFAULT`] to the caller instead of taking an EL1 data abort that the
//!   same-EL vector would turn into a kernel panic. No exception-fixup table
//!   (Linux's `extable`) is needed or wanted — see D5's rejected alternative.
//! - **The target need not be the running process.** The walk is
//!   address-space-independent, so the deferred `MF_DELIVERMSG` flush can write
//!   a *receiver's* buffer without that receiver's TTBR0 being installed.
//!
//! Copying through the HHDM alias is what makes both of those work, and it is
//! also why every page of every range goes through [`resolve_copyable`] rather
//! than a bare walk. The alias is a Normal-WB *kernel* mapping, so:
//!
//! - A read-only destination must be rejected explicitly — the MMU's EL0
//!   permission bits do not police these writes the way they policed the old
//!   `write_volatile`.
//! - A **device** leaf must be rejected on either side (slice 5.3). MMIO reached
//!   through a cacheable alias would be cached, gathered, and reordered; on the
//!   source side that means reading side-effecting registers, on the destination
//!   side losing writes.
//!
//! Slice 5.2's grant engine (`SYS_SAFECOPY` / `SYS_COPY`) is built directly on
//! these primitives: [`copy_between_as`] is two walks and a `memcpy` per chunk,
//! with no new mechanism of its own.

use minixrs_kernel_shared::error::EFAULT;
use minixrs_kernel_shared::message::{USER_PAGE_SIZE, dual_page_chunks, page_chunks};

use crate::arch::aarch64::addrspace::walk_pt_in;
use crate::mm::{FRAME_SIZE, phys_to_hhdm};

// `page_chunks` splits at `USER_PAGE_SIZE` boundaries and `walk_pt_in` resolves
// one L3 leaf per chunk, so the two granules must be the same number. They are
// declared in different crates — `USER_PAGE_SIZE` in `kernel-shared` (to keep
// the chunk arithmetic host-testable), `FRAME_SIZE` in the kernel's allocator —
// so nothing but this assert stops a future granule change from desyncing them
// into silent partial copies.
const _: () = assert!(USER_PAGE_SIZE == FRAME_SIZE as u64);

/// Copy `dst.len()` bytes out of address space `ttbr0_pa`, starting at user
/// VA `va`.
///
/// `Err(EFAULT)` if any page of the range is unmapped. `dst` may hold a
/// partial copy on error; every caller discards its staging buffer in that
/// case, so no rollback is done (MINIX 3's `data_copy` has the same contract).
pub fn copy_from_user_as(ttbr0_pa: u64, va: u64, dst: &mut [u8]) -> Result<(), i32> {
    let mut done = 0usize;
    for chunk in page_chunks(va, dst.len()) {
        let pa = resolve_copyable(ttbr0_pa, chunk.page_va, false)?;
        // SAFETY: `pa` is a frame this address space's L3 maps, so it came from
        // `alloc_frame` and is HHDM-mapped; `chunk.offset + chunk.len` is
        // within one 4 KiB page by `page_chunks`' contract, so the source range
        // lies inside that frame. The HHDM alias and the TTBR0 mapping are both
        // Normal-WB Inner-Shareable (`ATTR_IDX_NORMAL`) and ARMv8 data caches
        // are PIPT, so the two aliases are coherent with no maintenance — the
        // same assumption `addrspace::table_ref` already makes for live page
        // tables. Single-threaded EL1 with DAIF.I masked: no concurrent unmap
        // can race the walk. `dst` is a distinct kernel buffer, never a HHDM
        // alias of the same frame, so the ranges cannot overlap.
        unsafe {
            let src = phys_to_hhdm(pa).add(chunk.offset);
            core::ptr::copy_nonoverlapping(src, dst.as_mut_ptr().add(done), chunk.len);
        }
        done += chunk.len;
    }
    Ok(())
}

/// Copy `src` into address space `ttbr0_pa` at user VA `va`.
///
/// `Err(EFAULT)` if any page of the range is unmapped or not EL0-writable.
/// **All-or-nothing**: every page is probed before the first byte is written,
/// so a straddling buffer whose second page is bad leaves the user's memory
/// untouched rather than half-updated. A 104-byte `Message` is only 8-aligned
/// and really can straddle, so this matters in practice.
pub fn copy_to_user_as(ttbr0_pa: u64, va: u64, src: &[u8]) -> Result<(), i32> {
    probe_user_range(ttbr0_pa, va, src.len(), true)?;

    let mut done = 0usize;
    for chunk in page_chunks(va, src.len()) {
        let pa = resolve_copyable(ttbr0_pa, chunk.page_va, true)?;
        // SAFETY: as `copy_from_user_as`, with the direction reversed — and the
        // destination additionally proven EL0-writable by the probe above, so
        // this cannot smuggle a write past a read-only mapping via the HHDM.
        unsafe {
            let dst = phys_to_hhdm(pa).add(chunk.offset);
            core::ptr::copy_nonoverlapping(src.as_ptr().add(done), dst, chunk.len);
        }
        done += chunk.len;
    }
    Ok(())
}

/// Copy `len` bytes from `src_va` in address space `src_ttbr0` to `dst_va` in
/// address space `dst_ttbr0`.
///
/// The grant engine's whole mechanism (slice 5.2): two page-table walks and a
/// `memcpy` through the frames' HHDM aliases, per chunk. Neither address space
/// need be the active one, and the two may be the same.
///
/// `Err(EFAULT)` if any page of either range is unmapped, or any destination
/// page is not EL0-writable. **All-or-nothing on the destination**: both ranges
/// are probed before the first byte moves, the same contract (and for the same
/// reason) as [`copy_to_user_as`] — and the destination probe is load-bearing
/// for the same reason too, since the HHDM alias is a kernel mapping that the
/// MMU's EL0 permission bits do not police.
///
/// Overlapping ranges within one address space are copied chunk-by-chunk in
/// ascending order with `memmove` semantics per chunk. That is well-defined for
/// disjoint or forward-shifted ranges but is *not* a general overlap-safe copy;
/// no caller asks for one (a grant names one process's buffer and the caller's
/// own, and `SYS_COPY`'s two endpoints are distinct in every use).
pub fn copy_between_as(
    src_ttbr0: u64,
    src_va: u64,
    dst_ttbr0: u64,
    dst_va: u64,
    len: usize,
) -> Result<(), i32> {
    probe_user_range(src_ttbr0, src_va, len, false)?;
    probe_user_range(dst_ttbr0, dst_va, len, true)?;

    for chunk in dual_page_chunks(src_va, dst_va, len) {
        let src_pa = resolve_copyable(src_ttbr0, chunk.src_page_va, false)?;
        let dst_pa = resolve_copyable(dst_ttbr0, chunk.dst_page_va, true)?;
        // SAFETY: both PAs came from an L3 leaf of their respective address
        // space, so both are `alloc_frame` frames and HHDM-mapped; each
        // `offset + len` stays inside one 4 KiB page by `dual_page_chunks`'
        // contract, so both ranges lie inside their frame. Cache coherence and
        // the single-threaded-EL1 no-concurrent-unmap argument are as in
        // `copy_from_user_as`. `copy` (not `copy_nonoverlapping`) because the
        // two aliases *can* name the same frame when a caller copies within one
        // address space.
        unsafe {
            let src = phys_to_hhdm(src_pa).add(chunk.src_offset);
            let dst = phys_to_hhdm(dst_pa).add(chunk.dst_offset);
            core::ptr::copy(src, dst, chunk.len);
        }
    }
    Ok(())
}

/// Verify every page of `[va, va + len)` is mapped in `ttbr0_pa` — and
/// EL0-writable when `write`.
///
/// The pre-pass behind [`copy_to_user_as`]'s and [`copy_between_as`]'s
/// all-or-nothing contracts, exposed because a caller sometimes wants the check
/// without the copy.
pub fn probe_user_range(ttbr0_pa: u64, va: u64, len: usize, write: bool) -> Result<(), i32> {
    for chunk in page_chunks(va, len) {
        resolve_copyable(ttbr0_pa, chunk.page_va, write)?;
    }
    Ok(())
}

/// Resolve one page of a copy range: walk it, reject a device leaf outright, and
/// reject a non-writable one when the copy will write there.
///
/// The device rejection (slice 5.3) closes a hole the grant engine would
/// otherwise open. Every copy here goes through the frame's **HHDM alias**, which
/// is a Normal-WB kernel mapping — so a `memcpy` over a device leaf's PA would
/// touch MMIO cached, gathered, and reordered, reading side-effecting registers
/// on the source side and losing writes on the destination side. Nothing legitimate
/// asks for it: a server that granted its own UART window, or a `SYS_COPY` naming
/// TTY's device VA, is either confused or hostile. `EFAULT` is the right answer
/// either way — it is what an unmapped page returns, so a caller learns nothing
/// about whether the VA exists.
fn resolve_copyable(ttbr0_pa: u64, page_va: u64, write: bool) -> Result<u64, i32> {
    let (pa, prot) = walk_pt_in(ttbr0_pa, page_va).ok_or(EFAULT)?;
    if prot.device {
        return Err(EFAULT);
    }
    if write && !prot.writable {
        return Err(EFAULT);
    }
    Ok(pa)
}
