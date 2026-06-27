// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! RS liveness-monitoring table (slice 4.4).
//!
//! RS heartbeats a fixed set of boot servers: on each `SYS_SETALARM` expiry it
//! pings every monitored peer (`ipc_notify`) and, between alarms, records each
//! peer's ack (a `NOTIFY` back). This module holds the per-peer liveness state —
//! a fixed `[Server; CAP]` table wrapped in an `UnsafeCell` newtype, exactly
//! like SCHED's policy table and the DS registry — plus the pure accounting
//! helpers; the IPC glue lives in `main.rs`.
//!
//! **Accounting:** a *round* is the interval between two alarm fires. During a
//! round, [`mark_alive`] flags each peer that acked. At the next fire,
//! [`sweep`] tallies the round: a peer that acked has its miss counter reset; a
//! peer that did not has it incremented. A peer whose consecutive misses reach
//! [`DEAD_THRESHOLD`] is *detected* dead. Phase 4 is detect-only — RS runs at
//! EL0 with no console and restart-on-crash is a later slice — so the live
//! consequence is just the returned count; the accounting itself is what these
//! host tests pin down.
//!
//! The pure `*_in` helpers operate on a borrowed array and carry the host unit
//! tests; the thin `init`/`mark_alive`/`sweep` wrappers reach the static and are
//! the only `unsafe` here — the same split as SCHED's policy table.

use core::cell::UnsafeCell;

use minixrs_kernel_shared::endpoint::Endpoint;

/// Monitored-peer table capacity. Covers the boot servers RS heartbeats; no
/// allocator, so this is a hard cap.
const CAP: usize = 8;

/// Consecutive missed heartbeat rounds before a peer is considered dead.
pub const DEAD_THRESHOLD: u32 = 3;

/// One monitored peer's liveness state. `in_use == false` marks a free slot.
#[derive(Copy, Clone)]
struct Server {
    endpoint: Endpoint,
    /// Acked at least once during the current round.
    seen: bool,
    /// Consecutive rounds with no ack.
    missed: u32,
    in_use: bool,
}

impl Server {
    const EMPTY: Self = Self {
        endpoint: 0,
        seen: false,
        missed: 0,
        in_use: false,
    };
}

/// Populate `t` with the monitored peers. Truncates silently at [`CAP`].
/// Returns the number of peers recorded.
fn init_in(t: &mut [Server; CAP], endpoints: &[Endpoint]) -> usize {
    *t = [Server::EMPTY; CAP];
    let mut n = 0;
    for &e in endpoints.iter() {
        if n >= CAP {
            break;
        }
        t[n] = Server {
            endpoint: e,
            seen: false,
            missed: 0,
            in_use: true,
        };
        n += 1;
    }
    n
}

/// Record an ack from `endpoint` for the current round. Returns true if
/// `endpoint` is a monitored peer (so the caller can tell a heartbeat ack apart
/// from unrelated `NOTIFY` traffic).
fn mark_alive_in(t: &mut [Server; CAP], endpoint: Endpoint) -> bool {
    for e in t.iter_mut() {
        if e.in_use && e.endpoint == endpoint {
            e.seen = true;
            return true;
        }
    }
    false
}

/// Close out the current round: reset the miss counter for peers that acked,
/// increment it for those that did not, and clear `seen` for the next round.
/// Returns the number of peers whose consecutive misses have reached
/// [`DEAD_THRESHOLD`] (the detect-only signal).
fn sweep_in(t: &mut [Server; CAP]) -> u32 {
    let mut dead = 0;
    for e in t.iter_mut() {
        if !e.in_use {
            continue;
        }
        if e.seen {
            e.missed = 0;
        } else {
            e.missed = e.missed.saturating_add(1);
        }
        e.seen = false;
        if e.missed >= DEAD_THRESHOLD {
            dead += 1;
        }
    }
    dead
}

/// `UnsafeCell`-wrapped static monitor table. RS is a single-threaded EL0
/// process with no interrupt handlers of its own, so the table is only ever
/// touched from RS's straight-line receive loop — no concurrent access.
#[repr(transparent)]
struct Table(UnsafeCell<[Server; CAP]>);

// SAFETY: single-mutator invariant (module note); RS never accesses the table
// from more than its one straight-line loop.
unsafe impl Sync for Table {}

static TABLE: Table = Table(UnsafeCell::new([Server::EMPTY; CAP]));

/// Populate the monitored-peer set. See [`init_in`].
#[cfg_attr(test, allow(dead_code))]
pub fn init(endpoints: &[Endpoint]) -> usize {
    // SAFETY: single-mutator invariant (module note).
    let t = unsafe { &mut *TABLE.0.get() };
    init_in(t, endpoints)
}

/// Record an ack from `endpoint`. See [`mark_alive_in`].
#[cfg_attr(test, allow(dead_code))]
pub fn mark_alive(endpoint: Endpoint) -> bool {
    // SAFETY: single-mutator invariant (module note).
    let t = unsafe { &mut *TABLE.0.get() };
    mark_alive_in(t, endpoint)
}

/// Close out the round and return the dead-peer count. See [`sweep_in`].
#[cfg_attr(test, allow(dead_code))]
pub fn sweep() -> u32 {
    // SAFETY: single-mutator invariant (module note).
    let t = unsafe { &mut *TABLE.0.get() };
    sweep_in(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_records_peers() {
        let mut t = [Server::EMPTY; CAP];
        assert_eq!(init_in(&mut t, &[10, 20, 30]), 3);
        assert_eq!(t.iter().filter(|e| e.in_use).count(), 3);
    }

    #[test]
    fn init_truncates_at_cap() {
        let mut t = [Server::EMPTY; CAP];
        let many: [Endpoint; 10] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(init_in(&mut t, &many), CAP);
        assert_eq!(t.iter().filter(|e| e.in_use).count(), CAP);
    }

    #[test]
    fn ack_then_sweep_keeps_peer_alive() {
        let mut t = [Server::EMPTY; CAP];
        init_in(&mut t, &[10, 20]);
        assert!(mark_alive_in(&mut t, 10));
        assert!(mark_alive_in(&mut t, 20));
        // Both acked → no deaths, miss counters stay at 0.
        assert_eq!(sweep_in(&mut t), 0);
    }

    #[test]
    fn mark_alive_reports_unknown_peer() {
        let mut t = [Server::EMPTY; CAP];
        init_in(&mut t, &[10, 20]);
        assert!(!mark_alive_in(&mut t, 999));
    }

    #[test]
    fn missed_rounds_accumulate_to_dead() {
        let mut t = [Server::EMPTY; CAP];
        init_in(&mut t, &[10]);
        // Never acks: misses climb, dead at the threshold.
        for _ in 0..DEAD_THRESHOLD - 1 {
            assert_eq!(sweep_in(&mut t), 0);
        }
        assert_eq!(sweep_in(&mut t), 1);
        // Still dead on subsequent sweeps.
        assert_eq!(sweep_in(&mut t), 1);
    }

    #[test]
    fn ack_resets_miss_counter() {
        let mut t = [Server::EMPTY; CAP];
        init_in(&mut t, &[10]);
        // Two missed rounds, then an ack — counter resets, no death.
        assert_eq!(sweep_in(&mut t), 0);
        assert_eq!(sweep_in(&mut t), 0);
        assert!(mark_alive_in(&mut t, 10));
        assert_eq!(sweep_in(&mut t), 0);
        // And it must climb from zero again, not from where it left off.
        assert_eq!(sweep_in(&mut t), 0);
    }

    #[test]
    fn seen_flag_is_per_round() {
        let mut t = [Server::EMPTY; CAP];
        init_in(&mut t, &[10]);
        // Ack in round 1 keeps it alive; silence in round 2 counts as a miss.
        assert!(mark_alive_in(&mut t, 10));
        assert_eq!(sweep_in(&mut t), 0);
        assert_eq!(sweep_in(&mut t), 0); // missed=1, below threshold
        // Confirm the miss actually registered by pushing to the threshold.
        for _ in 0..DEAD_THRESHOLD - 2 {
            assert_eq!(sweep_in(&mut t), 0);
        }
        assert_eq!(sweep_in(&mut t), 1);
    }

    #[test]
    fn independent_peers_tracked_separately() {
        let mut t = [Server::EMPTY; CAP];
        init_in(&mut t, &[10, 20]);
        // 10 keeps acking; 20 goes silent.
        for _ in 0..DEAD_THRESHOLD {
            assert!(mark_alive_in(&mut t, 10));
            let _ = sweep_in(&mut t);
        }
        // Only 20 should be dead.
        assert_eq!(sweep_in(&mut t), 1);
    }
}
