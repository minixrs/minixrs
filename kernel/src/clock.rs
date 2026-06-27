// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Architecture-independent clock tick handler.
//!
//! Slice 2.4 surface: a monotonic uptime counter and a `tick()` entry point
//! that the arch IRQ dispatcher calls on every timer interrupt. Each tick
//! prints the running process's identifying character to the early-console
//! UART (visible proof of preemption), decrements `quantum_left`, and
//! triggers a reschedule when the quantum hits zero.
//!
//! Slice 2.5+ will grow this into the MINIX 3 `kernel/clock.c` surface:
//! `clock_timers`, `tmrs_exptimers`, virtual timers (`MF_VIRT_TIMER` /
//! `MF_PROF_TIMER`), CPU-time accounting split across `user_time` /
//! `sys_time`, and the per-process kernel-call alarm.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::proc::flags::RTS_NO_QUANTUM;
use crate::proc::sched;
use crate::uart::Uart;

/// Wall-clock tick counter. Slice 2.4 ticks at `TICK_HZ` (100 Hz default),
/// so 1 second of uptime = 100 ticks.
static UPTIME: AtomicU64 = AtomicU64::new(0);

/// Earliest absolute uptime tick at which any armed per-proc `SYS_SETALARM`
/// timer is due, or 0 if no alarm is armed (slice 4.4). This is an O(1)
/// fast-path gate: [`tick`] consults it every tick but only pays the O(N)
/// proc-table scan in [`crate::ipc::fire_expired_alarms`] when an alarm is
/// actually due. The source of truth is each [`Proc::alarm_at`]; this is a
/// cached minimum kept coherent by [`arm_alarm`] (on a new arm) and
/// [`set_earliest_alarm`] (on a fire that recomputes the remainder).
///
/// [`Proc::alarm_at`]: crate::proc::Proc::alarm_at
static EARLIEST_ALARM: AtomicU64 = AtomicU64::new(0);

/// Read the current monotonic tick count.
///
/// Slice 4.4's `SYS_SETALARM` handler (`system::do_setalarm`) is the first
/// real consumer; slice 4.x's `SYS_TIMES` will be the next.
pub fn uptime() -> u64 {
    UPTIME.load(Ordering::Relaxed)
}

/// Record that an alarm is due at absolute tick `at`, folding it into the
/// [`EARLIEST_ALARM`] fast-path gate (`min`, treating 0 as "none"). Called by
/// `system::do_setalarm` when a proc arms its timer.
///
/// Single-threaded invariant: `do_setalarm` runs in SVC (EL1) context and
/// `tick` in IRQ context — both DAIF-masked and mutually exclusive — so a
/// plain load + conditional store needs no compare-exchange.
pub fn arm_alarm(at: u64) {
    if at == 0 {
        return;
    }
    let cur = EARLIEST_ALARM.load(Ordering::Relaxed);
    if cur == 0 || at < cur {
        EARLIEST_ALARM.store(at, Ordering::Relaxed);
    }
}

/// Overwrite the [`EARLIEST_ALARM`] gate with `at` (0 = no alarm armed).
/// Called by [`crate::ipc::fire_expired_alarms`] after a fire, with the
/// recomputed minimum of the still-armed timers.
pub fn set_earliest_alarm(at: u64) {
    EARLIEST_ALARM.store(at, Ordering::Relaxed);
}

/// Per-tick handler. Called from the arch IRQ dispatcher on every PPI 27
/// (virtual timer) interrupt.
///
/// SAFETY: must be called only from IRQ context, with no other mutable
/// references into `PROC_TABLE` or the run-queue static live. The IRQ
/// stub is the only async writer, so this is upheld by construction in
/// slice 2.4.
pub unsafe fn tick() {
    let now = UPTIME.fetch_add(1, Ordering::Relaxed) + 1;

    // Per-proc one-shot alarm (slice 4.4). The fast-path gate keeps the common
    // tick O(1); only when an alarm is actually due do we pay the proc-table
    // scan + delivery. `fire_expired_alarms` materializes (and drops, on return)
    // its own PROC_TABLE borrow, so it must run *before* `current_proc_mut`
    // below — NLL then keeps the two borrows from aliasing.
    let earliest = EARLIEST_ALARM.load(Ordering::Relaxed);
    if earliest != 0 && now >= earliest {
        // Materializes (and drops, on return) its own PROC_TABLE/PRIV_TABLE
        // borrow; safe to call here because no such borrow is live yet (we have
        // not taken `current_proc_mut`).
        crate::ipc::fire_expired_alarms(now);
    }

    // SAFETY: forwarded — IRQ context with no overlapping borrows.
    let Some(cur) = (unsafe { sched::current_proc_mut() }) else {
        return;
    };

    // Print one identifying byte per tick. Falls back to '?' if name[0] is
    // NUL (shouldn't happen for boot-image / userland_bootstrap procs).
    let id = if cur.name[0] != 0 { cur.name[0] } else { b'?' };
    Uart::new().putc(id);

    if cur.quantum_left > 0 {
        cur.quantum_left -= 1;
    }
    let quantum_expired = cur.quantum_left == 0;
    if quantum_expired {
        cur.rts_flags.fetch_or(RTS_NO_QUANTUM, Ordering::Relaxed);
    }
    // NLL drops the `cur` borrow at the end of its last use above, so the
    // subsequent reschedule() call's PROC_TABLE accesses don't alias.
    if quantum_expired {
        // SAFETY: IRQ-context invariant — same as our preconditions.
        unsafe { sched::reschedule() };
    }
}
