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

/// Read the current monotonic tick count.
///
/// Unused in slice 2.4 — slice 2.5's `clock_timers` / kernel-call alarms
/// and slice 2.6's `SYS_TIMES` handler are the first real consumers.
#[allow(dead_code)] // slice 2.5/2.6: kernel-call alarms + SYS_TIMES
pub fn uptime() -> u64 {
    UPTIME.load(Ordering::Relaxed)
}

/// Per-tick handler. Called from the arch IRQ dispatcher on every PPI 27
/// (virtual timer) interrupt.
///
/// SAFETY: must be called only from IRQ context, with no other mutable
/// references into `PROC_TABLE` or the run-queue static live. The IRQ
/// stub is the only async writer, so this is upheld by construction in
/// slice 2.4.
pub unsafe fn tick() {
    UPTIME.fetch_add(1, Ordering::Relaxed);

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
