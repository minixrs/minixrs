// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The park table: console reads VFS has accepted and not yet answered (slice
//! 6.5, decision R2).
//!
//! A character driver never withholds a `CDEV_READ` reply; with nothing buffered
//! it answers `EAGAIN` and owes its caller one `NOTIFY` (R1). VFS, in turn, must
//! not relay that `EAGAIN` and must not wait: it **parks** the read — records the
//! raw request here, sends no reply, and returns to its receive loop — and asks
//! the driver again when the notification comes. This module is the record and
//! the order of the asking, and nothing else: pure, total, no IPC and no
//! `unsafe`, with the grant, the `CDEV_READ` and the late reply in `main.rs`.
//! The `rw.rs` / `fd.rs` split.
//!
//! ## One entry per proc slot, so it cannot fill
//!
//! A process blocked in its own `SENDREC` to VFS has exactly one request
//! outstanding, so one entry per kernel proc number is every read that can be
//! parked at once. The table is sized from [`NR_SERVED_PROCS`] like `fd.rs`'s
//! rows, and there is no "too many readers" errno because there is no way to be
//! one too many.
//!
//! ## An entry stores the request, never a grant
//!
//! `{endpoint, fd, buf, len}` is what the caller sent, after `rw::validate`
//! passed it. The grant is issued per attempt and revoked before the attempt
//! ends, so a parked reader holds no slot of VFS's grant pool for as long as
//! nobody types.
//!
//! ## The stored endpoint is the whole endpoint
//!
//! Proc numbers are recycled; generations are what tell two occupants of one
//! slot apart. An entry keeps its caller's full endpoint, and three things use
//! it:
//!
//! - the late reply and the retry's magic grant are addressed to **it**, not to
//!   whatever now holds the slot;
//! - [`ParkTable::park`] **replaces** whatever the slot held — see there;
//! - [`ParkTable::evict_stale`] drops an entry the moment the slot is seen to
//!   have a different occupant, so [`ParkTable::oldest`] never hands a recycled
//!   slot's read to the retry walk.
//!
//! VFS keeps no process table, so "the slot's current occupant" is only ever
//! known from evidence: a `VFS_PM_EXIT` naming the slot ([`ParkTable::free`]),
//! or a request whose kernel-stamped `m_source` is in it (`evict_stale`).
//!
//! ## Ownership: a value, not a static
//!
//! `fd.rs` keeps its rows in an `UnsafeCell` static because they were once an
//! immutable `static` and stayed where they were. This table is new, small
//! (about a kilobyte, against a 64 KiB stack), and has no reason to be
//! reachable from anywhere but the receive loop, so it follows `GrantPool`
//! instead: a `main`-frame value handed down by `&mut`. That is what keeps this
//! module free of `unsafe`, and it makes every test below an ordinary
//! single-owner fixture rather than a contender for one shared static.

use minixrs_kernel_shared::com::NR_SERVED_PROCS;
use minixrs_kernel_shared::endpoint::{Endpoint, endpoint_proc};

/// Entries in the table — one per process, indexed by **kernel proc number**.
///
/// [`NR_SERVED_PROCS`] and never an independent literal, for `fd.rs`'s reason: a
/// process that has a descriptor row must have a place to park.
pub const NR_PARK_SLOTS: usize = NR_SERVED_PROCS;

const _: () = assert!(NR_PARK_SLOTS >= NR_SERVED_PROCS);

/// One parked `VFS_READ`: the request as it arrived, plus its place in the queue.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Parked {
    /// The caller's full endpoint — generation included — as the kernel stamped
    /// it. The late reply goes here, and the retry's magic grant names it as
    /// owner.
    pub endpoint: Endpoint,
    /// The descriptor the read was issued on. Re-resolved at every retry.
    pub fd: i32,
    /// The buffer's address in the caller's address space.
    pub buf: u64,
    /// Bytes asked for, already through `rw::validate` and non-zero.
    pub len: usize,
    /// Position in the oldest-first order: smaller is older. Unique among the
    /// entries of one table.
    pub seq: u64,
}

/// The table. See the module note.
pub struct ParkTable {
    slots: [Option<Parked>; NR_PARK_SLOTS],
    /// The sequence number the next [`ParkTable::park`] hands out. Greater than
    /// every `seq` in `slots`.
    next_seq: u64,
}

impl ParkTable {
    /// An empty table.
    pub const fn new() -> Self {
        Self {
            slots: [None; NR_PARK_SLOTS],
            next_seq: 0,
        }
    }

    /// An empty table whose next sequence number is `next_seq`, so a test can
    /// stand at the end of the counter without parking 2^64 reads to get there.
    #[cfg(test)]
    fn with_next_seq(next_seq: u64) -> Self {
        Self {
            slots: [None; NR_PARK_SLOTS],
            next_seq,
        }
    }

    /// Park a read for `endpoint`, as the **newest** entry. Returns `false`, and
    /// parks nothing, only when `endpoint`'s proc number has no slot here (a
    /// kernel task, or a number past [`NR_PARK_SLOTS`]) — the caller must then
    /// answer the request itself, because nothing will ever retry it.
    ///
    /// **A slot that is already occupied is replaced, not refused.** `endpoint`
    /// is a kernel-stamped `m_source` whose request VFS is holding right now, so
    /// that process is demonstrably alive and demonstrably not blocked on an
    /// earlier read: whatever the slot holds is a leftover (an occupant whose
    /// exit never reached VFS), and it is the one to lose. Refusing would answer
    /// a legitimate `read()` with an error on account of a dead process, and
    /// keep the dead one's entry in the walk. The replacement joins the back of
    /// the queue — it is a new read.
    pub fn park(&mut self, endpoint: Endpoint, fd: i32, buf: u64, len: usize) -> bool {
        let Some(slot) = slot_index(endpoint_proc(endpoint).get()) else {
            return false;
        };
        // Emptied first, so a replaced entry is not counted — or renumbered —
        // by `take_seq`.
        self.slots[slot] = None;
        let seq = self.take_seq();
        self.slots[slot] = Some(Parked {
            endpoint,
            fd,
            buf,
            len,
            seq,
        });
        true
    }

    /// Hand out the next sequence number.
    ///
    /// `checked_add`, not `+`: servers ship with `overflow-checks = false`, so a
    /// bare increment would wrap silently in the shipped binary — where the
    /// newest read would become the oldest — while panicking under `cargo test`.
    /// At the end of the counter the live entries are **renumbered** `0..n` in
    /// their existing order and counting resumes from `n`, so the order is
    /// exact across the wrap rather than merely unlikely to matter. (2^64 parks
    /// is not a reachable number; the point is that no input makes this wrong.)
    fn take_seq(&mut self) -> u64 {
        if let Some(next) = self.next_seq.checked_add(1) {
            let seq = self.next_seq;
            self.next_seq = next;
            return seq;
        }
        // An entry's new number is its rank: how many entries are older. Ranks
        // are computed against the old numbers, then stored.
        let old = self.slots;
        let mut live: u64 = 0;
        for cell in self.slots.iter_mut() {
            let Some(p) = cell else { continue };
            let older = old.iter().flatten().filter(|q| q.seq < p.seq).count();
            p.seq = older as u64;
            live = live.saturating_add(1);
        }
        // `live <= NR_PARK_SLOTS`, so this cannot saturate.
        self.next_seq = live.saturating_add(1);
        live
    }

    /// Free `proc_slot`'s entry, whoever parked it. `true` if there was one.
    ///
    /// Keyed by slot alone, deliberately: this serves `VFS_PM_EXIT`, where the
    /// slot's occupant is gone, and an entry left there by an *earlier*
    /// occupant is no more deserving of a reply than the exiting one's. An empty
    /// slot, a negative number and one past the table are all `false` and touch
    /// nothing, so a stray exit report cannot disturb a neighbour.
    pub fn free(&mut self, proc_slot: i32) -> bool {
        slot_index(proc_slot)
            .and_then(|slot| self.slots[slot].take())
            .is_some()
    }

    /// Record that `current`'s slot is occupied by `current`: an entry parked
    /// there under any **other** endpoint is a previous occupant's, and is
    /// dropped. `true` if one was.
    ///
    /// An entry under `current` itself is left alone. (It cannot exist while
    /// `current` is sending — a parked process is blocked — but it is not
    /// *stale*, and this function's only business is staleness.)
    pub fn evict_stale(&mut self, current: Endpoint) -> bool {
        let Some(slot) = slot_index(endpoint_proc(current).get()) else {
            return false;
        };
        match self.slots[slot] {
            Some(p) if p.endpoint != current => {
                self.slots[slot] = None;
                true
            }
            _ => false,
        }
    }

    /// The oldest parked read, or `None` when nothing is parked.
    ///
    /// A copy, not a borrow: the retry walk issues a SENDREC between looking an
    /// entry up and freeing it, and nothing of this table may be held across
    /// that (`fd.rs`'s rule, kept here by construction rather than by care).
    pub fn oldest(&self) -> Option<Parked> {
        self.slots.iter().flatten().min_by_key(|p| p.seq).copied()
    }

    /// Whether any read is parked.
    pub fn any_parked(&self) -> bool {
        self.slots.iter().any(Option::is_some)
    }
}

/// A proc number as an index into the table, or `None` for one that has no slot
/// — negative (a kernel task) or past the end. Every entry point goes through
/// this, so nothing here can index wild.
fn slot_index(proc_slot: i32) -> Option<usize> {
    usize::try_from(proc_slot)
        .ok()
        .filter(|&slot| slot < NR_PARK_SLOTS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::com::{NR_BOOT_PROCS, SYSTEM, boot_endpoint};
    use minixrs_kernel_shared::endpoint::{ProcNr, bump_generation, make_endpoint};

    /// A user-range proc slot the fixtures park in, and two neighbours.
    const SLOT_A: i32 = NR_BOOT_PROCS as i32 + 1;
    const SLOT_B: i32 = SLOT_A + 1;
    const SLOT_C: i32 = SLOT_A + 2;

    /// `slot`'s endpoint at generation 1 — a forked process, not a boot one.
    fn ep(slot: i32) -> Endpoint {
        make_endpoint(1, ProcNr::new(slot))
    }

    /// Drain the table oldest-first, returning the endpoints in walk order. The
    /// retry walk's own shape: look at the oldest, free it, look again.
    fn drain(t: &mut ParkTable) -> Vec<Endpoint> {
        let mut order = Vec::new();
        while let Some(p) = t.oldest() {
            order.push(p.endpoint);
            assert!(t.free(endpoint_proc(p.endpoint).get()));
        }
        order
    }

    #[test]
    fn a_new_table_is_empty() {
        let t = ParkTable::new();
        assert!(!t.any_parked());
        assert_eq!(t.oldest(), None);
    }

    #[test]
    fn park_stores_the_request_verbatim() {
        let mut t = ParkTable::new();
        assert!(t.park(ep(SLOT_A), 0, 0x4000_1000, 64));
        assert!(t.any_parked());
        let p = t.oldest().expect("one entry");
        assert_eq!(
            (p.endpoint, p.fd, p.buf, p.len),
            (ep(SLOT_A), 0, 0x4000_1000, 64)
        );
    }

    #[test]
    fn one_entry_per_slot_a_repark_replaces_and_goes_to_the_back() {
        let mut t = ParkTable::new();
        assert!(t.park(ep(SLOT_A), 0, 0x1000, 8));
        assert!(t.park(ep(SLOT_B), 0, 0x2000, 8));
        // The same slot parks again: the old request is gone, not queued beside
        // the new one, and the new one is the newest read in the table.
        assert!(t.park(ep(SLOT_A), 5, 0x3000, 16));

        let first = t.oldest().expect("two entries");
        assert_eq!(first.endpoint, ep(SLOT_B));
        assert!(t.free(SLOT_B));
        let second = t.oldest().expect("one entry");
        assert_eq!(
            (second.endpoint, second.fd, second.buf, second.len),
            (ep(SLOT_A), 5, 0x3000, 16)
        );
        assert!(t.free(SLOT_A));
        // One entry per slot: nothing of the replaced request is left behind.
        assert!(!t.any_parked());
        assert_eq!(t.oldest(), None);
    }

    #[test]
    fn a_repark_under_a_new_generation_replaces_the_old_occupants_read() {
        let mut t = ParkTable::new();
        let old = ep(SLOT_A);
        let new = bump_generation(old);
        assert_ne!(old, new);
        assert!(t.park(old, 0, 0x1000, 8));
        assert!(t.park(new, 0, 0x2000, 8));
        assert_eq!(drain(&mut t), vec![new]);
    }

    #[test]
    fn the_walk_is_oldest_first_across_slots_whatever_their_numbers() {
        let mut t = ParkTable::new();
        // Parked in an order that is neither ascending nor descending by slot,
        // so a walk by slot index cannot pass.
        assert!(t.park(ep(SLOT_B), 0, 0, 1));
        assert!(t.park(ep(SLOT_C), 0, 0, 1));
        assert!(t.park(ep(SLOT_A), 0, 0, 1));
        assert_eq!(drain(&mut t), vec![ep(SLOT_B), ep(SLOT_C), ep(SLOT_A)]);
    }

    #[test]
    fn freeing_from_the_middle_keeps_the_rest_in_order() {
        let mut t = ParkTable::new();
        assert!(t.park(ep(SLOT_A), 0, 0, 1));
        assert!(t.park(ep(SLOT_B), 0, 0, 1));
        assert!(t.park(ep(SLOT_C), 0, 0, 1));
        assert!(t.free(SLOT_B));
        assert_eq!(drain(&mut t), vec![ep(SLOT_A), ep(SLOT_C)]);
    }

    #[test]
    fn sequence_numbers_are_unique_and_increase_with_age() {
        let mut t = ParkTable::new();
        assert!(t.park(ep(SLOT_A), 0, 0, 1));
        let a = t.oldest().expect("a").seq;
        assert!(t.park(ep(SLOT_B), 0, 0, 1));
        assert!(t.free(SLOT_A));
        let b = t.oldest().expect("b").seq;
        assert!(a < b, "{a} < {b}");
    }

    #[test]
    fn exit_frees_the_slots_entry_and_only_that_one() {
        let mut t = ParkTable::new();
        assert!(t.park(ep(SLOT_A), 0, 0, 1));
        assert!(t.park(ep(SLOT_B), 0, 0, 1));
        // `VFS_PM_EXIT` for A: its read must never be retried or replied to.
        assert!(t.free(SLOT_A));
        assert_eq!(drain(&mut t), vec![ep(SLOT_B)]);
    }

    #[test]
    fn freeing_an_empty_slot_is_false_and_touches_nothing() {
        let mut t = ParkTable::new();
        assert!(!t.free(SLOT_A));
        assert!(t.park(ep(SLOT_B), 0, 0, 1));
        assert!(!t.free(SLOT_A));
        // Twice is once: the second free finds the slot empty.
        assert!(t.free(SLOT_B));
        assert!(!t.free(SLOT_B));
        assert!(!t.any_parked());
    }

    #[test]
    fn slots_outside_the_table_neither_park_nor_free() {
        let mut t = ParkTable::new();
        assert!(t.park(ep(SLOT_A), 0, 0, 1));
        // A kernel task (negative proc number) and one past the last slot.
        for outside in [
            boot_endpoint(SYSTEM),
            make_endpoint(1, ProcNr::new(NR_PARK_SLOTS as i32)),
        ] {
            assert!(!t.park(outside, 0, 0, 1), "{outside}");
            assert!(!t.evict_stale(outside), "{outside}");
        }
        for slot in [-1, i32::MIN, NR_PARK_SLOTS as i32, i32::MAX] {
            assert!(!t.free(slot), "{slot}");
        }
        assert_eq!(drain(&mut t), vec![ep(SLOT_A)]);
    }

    #[test]
    fn the_last_slot_parks() {
        let mut t = ParkTable::new();
        let last = make_endpoint(1, ProcNr::new(NR_PARK_SLOTS as i32 - 1));
        assert!(t.park(last, 0, 0, 1));
        assert_eq!(drain(&mut t), vec![last]);
    }

    #[test]
    fn a_stale_endpoint_is_not_returned_for_retry() {
        let mut t = ParkTable::new();
        let old = ep(SLOT_A);
        let new = bump_generation(old);
        assert_eq!(endpoint_proc(old).get(), endpoint_proc(new).get());
        assert!(t.park(old, 0, 0x1000, 8));
        assert!(t.park(ep(SLOT_B), 0, 0x2000, 8));

        // The slot is seen under a new generation: the old occupant's read is
        // not the new one's to inherit, and must not reach the walk.
        assert!(t.evict_stale(new));
        assert_eq!(drain(&mut t), vec![ep(SLOT_B)]);
    }

    #[test]
    fn evicting_under_the_same_endpoint_keeps_the_entry() {
        let mut t = ParkTable::new();
        assert!(t.park(ep(SLOT_A), 0, 0, 1));
        assert!(!t.evict_stale(ep(SLOT_A)));
        // And an empty slot has nothing to evict.
        assert!(!t.evict_stale(ep(SLOT_B)));
        assert_eq!(drain(&mut t), vec![ep(SLOT_A)]);
    }

    #[test]
    fn the_sequence_counter_does_not_overflow_and_order_survives_its_end() {
        // Two parks from the end of the counter. `overflow-checks` is on under
        // `cargo test`, so a bare `+ 1` panics here — and wraps in the shipped
        // binary, where the newest read would become the oldest.
        let mut t = ParkTable::with_next_seq(u64::MAX - 1);
        assert!(t.park(ep(SLOT_B), 0, 0, 1));
        assert!(t.park(ep(SLOT_C), 0, 0, 1));
        assert!(t.park(ep(SLOT_A), 0, 0, 1));
        // A fourth, replacing the first, after the counter has been through its
        // end: still the newest.
        assert!(t.park(ep(SLOT_B), 0, 0, 1));
        assert_eq!(drain(&mut t), vec![ep(SLOT_C), ep(SLOT_A), ep(SLOT_B)]);
    }

    #[test]
    fn a_full_table_at_the_end_of_the_counter_keeps_its_order() {
        let mut t = ParkTable::with_next_seq(u64::MAX - (NR_PARK_SLOTS as u64 / 2));
        // Descending by slot, so age and slot number disagree everywhere.
        let want: Vec<Endpoint> = (0..NR_PARK_SLOTS as i32).rev().map(ep).collect();
        for &e in &want {
            assert!(t.park(e, 0, 0, 1));
        }
        assert_eq!(drain(&mut t), want);
    }
}
