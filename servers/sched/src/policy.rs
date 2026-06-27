// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! SCHED scheduling policy + per-proc state table (slice 4.3).
//!
//! SCHED is minix.rs's user-space scheduler. The kernel run queue is
//! *delegatable*: when a SCHED-scheduled proc exhausts its quantum the kernel
//! sends `SCHEDULING_NO_QUANTUM`, and SCHED re-admits the proc via
//! `SYS_SCHEDULE`, choosing its priority band and quantum. This module holds the
//! per-proc policy state — a fixed `[SchedProc; CAP]` table wrapped in an
//! `UnsafeCell` newtype, exactly like the DS registry and the VM region table —
//! plus the pure policy helpers; the IPC/kernel-call glue lives in `main.rs`.
//!
//! **Policy (4.3): round-robin quantum refresh at a fixed managed band**
//! ([`USER_Q`]). Each `SCHEDULING_NO_QUANTUM` re-admits the proc at its recorded
//! priority with a fresh quantum. The managed band equals the boot servers' band
//! (`SRV_Q = 8`) so a CPU-bound managed proc round-robins with the
//! kernel-scheduled procs instead of sinking behind them. MINIX's priority aging
//! (drop a band on full-quantum use, periodically re-raised by `balance_queues`)
//! needs the per-proc alarm from `SYS_SETALARM` (slice 4.4): without the
//! compensating boost, dropping a band would starve the managed proc behind the
//! kernel-scheduled band-8 stubs, so it is deferred.
//!
//! The pure `*_in` helpers operate on a borrowed array and carry the host unit
//! tests; the thin `schedule_proc`/`record`/`forget` wrappers reach the static
//! and are the only `unsafe` here — the same split as the DS registry.

use core::cell::UnsafeCell;

/// Managed priority band. Matches the kernel's `SRV_Q` so a CPU-bound managed
/// proc round-robins with the kernel-scheduled boot procs / stubs rather than
/// sinking behind them (see the module note on deferred priority aging).
pub const USER_Q: u8 = 8;

/// Quantum (kernel ticks) SCHED hands out. Small so a CPU-bound proc exhausts it
/// often, making the delegation round-trip visible in the boot trace.
pub const QUANTUM: i32 = 5;

/// Per-proc policy-state table capacity. Covers every boot proc plus the stubs;
/// no allocator, so this is a hard cap (a new proc past it is simply left
/// unmanaged — it stays blocked, the user-space equivalent of "can't schedule").
const CAP: usize = 16;

/// One managed proc's scheduling state. `in_use == false` marks a free slot.
#[derive(Copy, Clone)]
struct SchedProc {
    proc_nr: i32,
    priority: u8,
    quantum: i32,
    in_use: bool,
}

impl SchedProc {
    const EMPTY: Self = Self {
        proc_nr: 0,
        priority: 0,
        quantum: 0,
        in_use: false,
    };
}

/// Look up `proc_nr`'s `(priority, quantum)`, lazily registering it at the
/// default managed band + quantum if unseen (the `SCHEDULING_NO_QUANTUM` path —
/// the kernel pre-delegates a proc, so SCHED meets it for the first time on its
/// first no-quantum event). Returns `None` only if the table is full and
/// `proc_nr` is new.
fn schedule_in(t: &mut [SchedProc; CAP], proc_nr: i32) -> Option<(u8, i32)> {
    for e in t.iter() {
        if e.in_use && e.proc_nr == proc_nr {
            return Some((e.priority, e.quantum));
        }
    }
    for e in t.iter_mut() {
        if !e.in_use {
            *e = SchedProc {
                proc_nr,
                priority: USER_Q,
                quantum: QUANTUM,
                in_use: true,
            };
            return Some((USER_Q, QUANTUM));
        }
    }
    None
}

/// Record (or update) `proc_nr`'s managed priority + quantum — the
/// `SCHEDULING_START` / `SCHEDULING_SET_NICE` path. Returns false only if the
/// table is full and `proc_nr` is new.
fn record_in(t: &mut [SchedProc; CAP], proc_nr: i32, priority: u8, quantum: i32) -> bool {
    for e in t.iter_mut() {
        if e.in_use && e.proc_nr == proc_nr {
            e.priority = priority;
            e.quantum = quantum;
            return true;
        }
    }
    for e in t.iter_mut() {
        if !e.in_use {
            *e = SchedProc {
                proc_nr,
                priority,
                quantum,
                in_use: true,
            };
            return true;
        }
    }
    false
}

/// Change `proc_nr`'s managed priority, preserving its recorded quantum — the
/// `SCHEDULING_SET_NICE` path. A renice changes the band only; the time slice is
/// a per-proc property set at `SCHEDULING_START`, not a function of the nice
/// value. If `proc_nr` is unseen it is lazily registered at the given priority +
/// default [`QUANTUM`] (so a renice that precedes a start still takes effect).
/// Returns the effective quantum to hand `SYS_SCHEDULE`, or `None` only if the
/// table is full and `proc_nr` is new.
fn renice_in(t: &mut [SchedProc; CAP], proc_nr: i32, priority: u8) -> Option<i32> {
    for e in t.iter_mut() {
        if e.in_use && e.proc_nr == proc_nr {
            e.priority = priority;
            return Some(e.quantum);
        }
    }
    for e in t.iter_mut() {
        if !e.in_use {
            *e = SchedProc {
                proc_nr,
                priority,
                quantum: QUANTUM,
                in_use: true,
            };
            return Some(QUANTUM);
        }
    }
    None
}

/// Stop managing `proc_nr` (the `SCHEDULING_STOP` path). No-op if absent.
fn forget_in(t: &mut [SchedProc; CAP], proc_nr: i32) {
    for e in t.iter_mut() {
        if e.in_use && e.proc_nr == proc_nr {
            *e = SchedProc::EMPTY;
            return;
        }
    }
}

/// `UnsafeCell`-wrapped static policy table. See the module note for the
/// single-mutator invariant that makes the `Sync` impl sound.
#[repr(transparent)]
struct Table(UnsafeCell<[SchedProc; CAP]>);

// SAFETY: SCHED is a single-threaded EL0 process with no interrupt handlers of
// its own; the table is only ever accessed from SCHED's straight-line receive
// loop, so there is never concurrent access.
unsafe impl Sync for Table {}

static TABLE: Table = Table(UnsafeCell::new([SchedProc::EMPTY; CAP]));

/// Look up (lazily registering at the default band) `proc_nr`'s scheduling
/// parameters. See [`schedule_in`].
#[cfg_attr(test, allow(dead_code))]
pub fn schedule_proc(proc_nr: i32) -> Option<(u8, i32)> {
    // SAFETY: single-mutator invariant (module note); no other reference into
    // the table is live during SCHED's straight-line loop.
    let t = unsafe { &mut *TABLE.0.get() };
    schedule_in(t, proc_nr)
}

/// Record (or update) `proc_nr`'s managed priority + quantum. See [`record_in`].
#[cfg_attr(test, allow(dead_code))]
pub fn record(proc_nr: i32, priority: u8, quantum: i32) -> bool {
    // SAFETY: single-mutator invariant (module note).
    let t = unsafe { &mut *TABLE.0.get() };
    record_in(t, proc_nr, priority, quantum)
}

/// Change `proc_nr`'s priority, preserving its recorded quantum. See
/// [`renice_in`].
#[cfg_attr(test, allow(dead_code))]
pub fn renice(proc_nr: i32, priority: u8) -> Option<i32> {
    // SAFETY: single-mutator invariant (module note).
    let t = unsafe { &mut *TABLE.0.get() };
    renice_in(t, proc_nr, priority)
}

/// Stop managing `proc_nr`. See [`forget_in`].
#[cfg_attr(test, allow(dead_code))]
pub fn forget(proc_nr: i32) {
    // SAFETY: single-mutator invariant (module note).
    let t = unsafe { &mut *TABLE.0.get() };
    forget_in(t, proc_nr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sight_registers_at_default_band() {
        let mut t = [SchedProc::EMPTY; CAP];
        assert_eq!(schedule_in(&mut t, 13), Some((USER_Q, QUANTUM)));
        // The slot is now in use and stable on the next lookup.
        assert_eq!(schedule_in(&mut t, 13), Some((USER_Q, QUANTUM)));
        let used = t.iter().filter(|e| e.in_use).count();
        assert_eq!(
            used, 1,
            "a repeat no-quantum must not consume a second slot"
        );
    }

    #[test]
    fn record_then_schedule_returns_recorded_values() {
        let mut t = [SchedProc::EMPTY; CAP];
        assert!(record_in(&mut t, 9, 6, 20));
        assert_eq!(schedule_in(&mut t, 9), Some((6, 20)));
    }

    #[test]
    fn record_updates_in_place() {
        let mut t = [SchedProc::EMPTY; CAP];
        assert!(record_in(&mut t, 9, 6, 20));
        assert!(record_in(&mut t, 9, 10, 5));
        assert_eq!(schedule_in(&mut t, 9), Some((10, 5)));
        let used = t.iter().filter(|e| e.in_use).count();
        assert_eq!(used, 1);
    }

    #[test]
    fn forget_frees_the_slot() {
        let mut t = [SchedProc::EMPTY; CAP];
        schedule_in(&mut t, 13);
        forget_in(&mut t, 13);
        assert_eq!(t.iter().filter(|e| e.in_use).count(), 0);
        // forgetting an absent proc is a no-op
        forget_in(&mut t, 99);
        assert_eq!(t.iter().filter(|e| e.in_use).count(), 0);
    }

    #[test]
    fn distinct_procs_coexist() {
        let mut t = [SchedProc::EMPTY; CAP];
        schedule_in(&mut t, 11);
        schedule_in(&mut t, 12);
        record_in(&mut t, 13, 4, 30);
        assert_eq!(schedule_in(&mut t, 11), Some((USER_Q, QUANTUM)));
        assert_eq!(schedule_in(&mut t, 12), Some((USER_Q, QUANTUM)));
        assert_eq!(schedule_in(&mut t, 13), Some((4, 30)));
    }

    #[test]
    fn full_table_leaves_new_proc_unmanaged() {
        let mut t = [SchedProc::EMPTY; CAP];
        for i in 0..CAP as i32 {
            assert!(record_in(&mut t, i, USER_Q, QUANTUM));
        }
        // A new proc has nowhere to go.
        assert_eq!(schedule_in(&mut t, 100), None);
        assert!(!record_in(&mut t, 100, USER_Q, QUANTUM));
        // But an already-managed proc still resolves.
        assert_eq!(schedule_in(&mut t, 0), Some((USER_Q, QUANTUM)));
    }

    #[test]
    fn renice_preserves_quantum() {
        // A proc started with a non-default quantum keeps it across a renice;
        // only the band changes.
        let mut t = [SchedProc::EMPTY; CAP];
        assert!(record_in(&mut t, 9, USER_Q, 20));
        assert_eq!(renice_in(&mut t, 9, 4), Some(20));
        assert_eq!(schedule_in(&mut t, 9), Some((4, 20)));
        let used = t.iter().filter(|e| e.in_use).count();
        assert_eq!(used, 1, "renice must not consume a second slot");
    }

    #[test]
    fn renice_unseen_registers_at_default_quantum() {
        // A renice before any start lazily registers at the given band + default
        // quantum.
        let mut t = [SchedProc::EMPTY; CAP];
        assert_eq!(renice_in(&mut t, 13, 6), Some(QUANTUM));
        assert_eq!(schedule_in(&mut t, 13), Some((6, QUANTUM)));
    }

    #[test]
    fn renice_full_table_leaves_new_proc_unmanaged() {
        let mut t = [SchedProc::EMPTY; CAP];
        for i in 0..CAP as i32 {
            assert!(record_in(&mut t, i, USER_Q, QUANTUM));
        }
        // A new proc has nowhere to go, even via renice.
        assert_eq!(renice_in(&mut t, 100, 4), None);
        // An already-managed proc reprioritizes in place, quantum intact.
        assert_eq!(renice_in(&mut t, 0, 4), Some(QUANTUM));
        assert_eq!(schedule_in(&mut t, 0), Some((4, QUANTUM)));
    }
}
