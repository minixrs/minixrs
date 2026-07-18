// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! 8-bit ARMv8 ASID allocator with recycling.
//!
//! TTBR0_EL1 carries an ASID in bits [55:48] when TCR_EL1.AS = 0 (Limine's
//! default; `mmu::assert_tcr_el1_ttbr0_ready` enforces). The TLB tags
//! entries by ASID, so context switches that flip TTBR0 don't need a full
//! TLB flush — only a `tlbi aside1, Xt` of stale entries for the activated
//! ASID.
//!
//! Boot allocates ~11 ASIDs (six archive servers + five EL0 stubs), but
//! slice 4.6's fork/exit churn would exhaust the 8-bit space (1..=255,
//! ASID 0 reserved for "uninitialized") within minutes without recycling.
//! `SYS_EXIT` therefore returns each dead process's ASID via [`free_asid`]
//! (after flushing its TLB entries), and [`alloc_asid`] serves from that
//! free list before minting fresh IDs. Exhaustion — 255 simultaneously
//! *live* address spaces — still panics; that is a real capacity invariant,
//! not a churn artifact.
//!
//! Concurrency: single-threaded boot, like the rest of the kernel. Wrapped
//! in `UnsafeCell` + `unsafe impl Sync` per the convention documented on
//! `crate::proc::table`.

use core::cell::UnsafeCell;

/// First real ASID handed out. 0 is reserved as the "uninitialized"
/// sentinel `Proc::asid` carries until `userland_bootstrap` assigns a real
/// value.
const FIRST_ASID: u8 = 1;

/// One past the highest ASID (the 8-bit hardware limit).
const ASID_LIMIT: u16 = 256;

struct AsidPool {
    /// Next never-yet-minted ASID; grows monotonically to [`ASID_LIMIT`].
    next: u16,
    /// LIFO stack of freed ASIDs available for reuse.
    free: [u8; (ASID_LIMIT - 1) as usize],
    free_len: usize,
}

#[repr(transparent)]
struct AsidPoolCell(UnsafeCell<AsidPool>);
// SAFETY: single-threaded EL1 (interrupts masked in every kernel entry
// path); `alloc_asid`/`free_asid` are the only accessors.
unsafe impl Sync for AsidPoolCell {}

static POOL: AsidPoolCell = AsidPoolCell(UnsafeCell::new(AsidPool {
    next: FIRST_ASID as u16,
    free: [0; (ASID_LIMIT - 1) as usize],
    free_len: 0,
}));

/// Hand out an ASID: recycled from the free list if one is available, else
/// freshly minted. Panics when 255 address spaces are simultaneously live.
///
/// SAFETY: caller must hold the single-threaded EL1 invariant; this and
/// [`free_asid`] are the sole accessors of the pool.
pub unsafe fn alloc_asid() -> u8 {
    // SAFETY: caller's invariant — no concurrent pool access.
    let pool = unsafe { &mut *POOL.0.get() };
    if pool.free_len > 0 {
        pool.free_len -= 1;
        return pool.free[pool.free_len];
    }
    assert!(
        pool.next < ASID_LIMIT,
        "ASID space exhausted: 255 live address spaces"
    );
    let id = pool.next as u8;
    pool.next += 1;
    id
}

/// Return a dead process's ASID to the pool for reuse.
///
/// Contract: the caller must already have invalidated the ASID's TLB entries
/// (`mmu::flush_tlb_asid`) — the next `alloc_asid` may hand this ID to a
/// brand-new address space, and a stale translation would alias it. A
/// double-free would eventually let two live procs share one ASID (same
/// aliasing corruption), so it is a hard assert, not a debug one — the O(255)
/// scan is noise next to the page-table teardown that precedes every call.
///
/// SAFETY: caller must hold the single-threaded EL1 invariant; this and
/// [`alloc_asid`] are the sole accessors of the pool.
pub unsafe fn free_asid(asid: u8) {
    assert!(asid != 0, "freeing the reserved ASID 0");
    // SAFETY: caller's invariant — no concurrent pool access.
    let pool = unsafe { &mut *POOL.0.get() };
    assert!(
        (asid as u16) < pool.next,
        "freeing ASID {asid} that was never allocated"
    );
    for i in 0..pool.free_len {
        assert!(pool.free[i] != asid, "double free of ASID {asid}");
    }
    pool.free[pool.free_len] = asid;
    pool.free_len += 1;
}
