// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! 8-bit ARMv8 ASID allocator.
//!
//! TTBR0_EL1 carries an ASID in bits [55:48] when TCR_EL1.AS = 0 (Limine's
//! default; `mmu::assert_tcr_el1_ttbr0_ready` enforces). The TLB tags
//! entries by ASID, so context switches that flip TTBR0 don't need a full
//! TLB flush — only a `tlbi aside1, Xt` of stale entries for the activated
//! ASID.
//!
//! Slice 3.1b only ever hands out 3 ASIDs (one per EL0 stub). The 8-bit
//! space (1..=255, ASID 0 reserved for "uninitialized") is plenty; real
//! rollover lands in Phase 4 when fork actually churns ASIDs. Until then,
//! exhaustion panics.
//!
//! Concurrency: single-threaded boot, like the rest of the kernel. Wrapped
//! in `UnsafeCell` + `unsafe impl Sync` per the convention documented on
//! `crate::proc::table`.

use core::cell::UnsafeCell;

/// First real ASID handed out. 0 is reserved as the "uninitialized"
/// sentinel `Proc::asid` carries until `userland_bootstrap` assigns a real
/// value.
const FIRST_ASID: u8 = 1;

#[repr(transparent)]
struct AsidCounter(UnsafeCell<u8>);
// SAFETY: single-threaded boot, single writer (`alloc_asid`).
unsafe impl Sync for AsidCounter {}

static NEXT_ASID: AsidCounter = AsidCounter(UnsafeCell::new(FIRST_ASID));

/// Hand out the next free ASID. Panics if the 8-bit space is exhausted —
/// real rollover is deferred to Phase 4 (fork is what would actually churn
/// ASIDs; slice 3.1b only allocates 3).
///
/// SAFETY: caller must hold the single-threaded-boot invariant; this is
/// the sole writer of `NEXT_ASID`.
pub unsafe fn alloc_asid() -> u8 {
    // SAFETY: caller's invariant — only writer.
    let cell = unsafe { &mut *NEXT_ASID.0.get() };
    let id = *cell;
    // 0 means we wrapped past 255 (checked_add returned 254→255→panic on
    // the previous call), or someone reset the counter to 0. Either way
    // the 8-bit ASID space is exhausted.
    assert!(id != 0, "ASID space exhausted (8-bit wrap)");
    *cell = id
        .checked_add(1)
        .expect("ASID space exhausted (next allocation would overflow u8)");
    id
}
