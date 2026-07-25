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
//! A read-only destination is rejected explicitly ([`Prot::writable`]): the
//! HHDM alias is a kernel mapping, so the MMU's EL0 permission bits do not
//! police these writes the way they policed the old `write_volatile`.
//!
//! Slice 5.2's grant engine (`SYS_SAFECOPY` / `SYS_COPY`) builds directly on
//! these primitives — a cross-address-space copy is two walks and a `memcpy`,
//! which is exactly [`copy_from_user_as`] followed by [`copy_to_user_as`].

use minixrs_kernel_shared::error::EFAULT;
use minixrs_kernel_shared::message::{USER_PAGE_SIZE, page_chunks};

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
        let (pa, _prot) = walk_pt_in(ttbr0_pa, chunk.page_va).ok_or(EFAULT)?;
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
        let (pa, _prot) = walk_pt_in(ttbr0_pa, chunk.page_va).ok_or(EFAULT)?;
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

/// Verify every page of `[va, va + len)` is mapped in `ttbr0_pa` — and
/// EL0-writable when `write`.
///
/// The pre-pass behind [`copy_to_user_as`]'s all-or-nothing contract, exposed
/// because slice 5.2's `verify_grant` wants exactly this check before it
/// commits to a grant copy.
pub fn probe_user_range(ttbr0_pa: u64, va: u64, len: usize, write: bool) -> Result<(), i32> {
    for chunk in page_chunks(va, len) {
        let (_pa, prot) = walk_pt_in(ttbr0_pa, chunk.page_va).ok_or(EFAULT)?;
        if write && !prot.writable {
            return Err(EFAULT);
        }
    }
    Ok(())
}
