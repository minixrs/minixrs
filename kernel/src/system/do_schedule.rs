// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_SCHEDULE` / `SYS_SCHEDCTL` — the kernel mechanism behind the
//! delegatable scheduler (slice 4.3).
//!
//! minix.rs keeps the priority-banded run queue in the kernel but makes it
//! *delegatable*: a user-space scheduler (SCHED) drives policy through these two
//! calls, exactly as VM drives paging policy through `SYS_VMCTL`. Both name a
//! *target* process by endpoint, so — like `do_vmctl` — they take the whole
//! `&mut [Proc]` slice + `caller_nr` and are routed specially in
//! [`kernel_call_dispatch`](super::kernel_call_dispatch).
//!
//! - [`do_schedule`] sets a target's priority + quantum and (re-)admits it to the
//!   run queue. SCHED issues it in response to `SCHEDULING_NO_QUANTUM` (re-admit
//!   a preempted proc) and at `SCHEDULING_START` (initial assignment).
//! - [`do_schedctl`] claims a target (`target.scheduler = caller`) or releases it
//!   back to the kernel scheduler (`SCHEDCTL_FLAG_KERNEL` → `scheduler = NONE`).
//!
//! ## Trust model
//!
//! Identical to `do_vmctl`: the single gate is `Priv::k_call_mask` granting the
//! call, checked in `kernel_call_dispatch` before we run. A caller that reaches
//! here may name any process. SCHED is the intended sole holder; if a
//! less-trusted process is later granted these calls, per-target authorization
//! must be added first.
//!
//! ## Message payload layout (offsets within `Message::payload`)
//!
//! | offset | field            | direction | call          |
//! |--------|------------------|-----------|---------------|
//! |  0..4  | target endpoint  | in        | `SYS_SCHEDULE`|
//! |  4..8  | priority (i32)   | in        | `SYS_SCHEDULE`|
//! |  8..12 | quantum_ms (i32) | in        | `SYS_SCHEDULE`|
//! |  0..4  | flags (i32)      | in        | `SYS_SCHEDCTL`|
//! |  4..8  | target endpoint  | in        | `SYS_SCHEDCTL`|

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::callnr::SCHEDCTL_FLAG_KERNEL;
use minixrs_kernel_shared::endpoint::{Endpoint, NONE};
use minixrs_kernel_shared::error::{EINVAL, OK};
use minixrs_kernel_shared::message::Message;

use crate::proc::Proc;
use crate::proc::flags::RTS_NO_QUANTUM;
use crate::proc::sched::{self, NR_SCHED_QUEUES};
use crate::proc::table::{N_PROC_SLOTS, proc_index};
use crate::uart::Uart;

/// Leading `SYS_SCHEDULE` calls to trace explicitly — the scheduler-side half of
/// the delegation round-trip (`[noq]` in `sched::reschedule` is the kernel-side
/// half). A head carve-out is needed because SCHED's re-admits are ~0.1% of the
/// kernel-call flood, so the modulo-100 `[ksys]` sampler rarely catches them
/// (same reasoning as `do_vmctl::VMCTL_TRACE_HEAD`).
const SCHED_TRACE_HEAD: u64 = 6;
static SCHED_COUNT: AtomicU64 = AtomicU64::new(0);

/// `SYS_SCHEDULE` — set a target's priority + quantum and (re-)admit it.
///
/// Reads the target endpoint (`SELF` allowed), priority, and quantum from the
/// payload; validates them; updates the proc slot; then re-admits the proc:
///
/// - If the target is **off** the run queue (the `SCHEDULING_NO_QUANTUM` case —
///   `reschedule` dequeued it and left `RTS_NO_QUANTUM` set),
///   [`sched::rts_unset`] clears the no-quantum block and enqueues it in the
///   (possibly new) priority band.
/// - If the target is **on** the run queue (runnable already), its band may have
///   changed, so move it with `dequeue` + `enqueue`.
pub(super) fn do_schedule(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    caller_nr: ProcNr,
    msg: &mut Message,
) -> i32 {
    let target_e: Endpoint = read_i32(msg, 0);
    let priority = read_i32(msg, 4);
    let quantum_ms = read_i32(msg, 8);

    let target_idx = match super::resolve_target(proc_table, caller_nr, target_e) {
        Ok(idx) => idx,
        Err(e) => return e,
    };

    // Priority must name a real band; quantum must be positive. An out-of-range
    // priority would otherwise trip `enqueue`'s `prio < NR_SCHED_QUEUES` assert.
    if !(0..NR_SCHED_QUEUES as i32).contains(&priority) || quantum_ms <= 0 {
        return EINVAL;
    }

    let (nr, was_runnable, name0) = {
        let p = &mut proc_table[target_idx];
        let was_runnable = p.rts_flags.load(Ordering::Relaxed) == 0;
        p.priority = priority as u8;
        p.quantum_ms = quantum_ms as u32;
        p.quantum_left = quantum_ms as u64;
        (p.nr, was_runnable, p.name[0])
    };

    // SAFETY: the `p` borrow above has ended (NLL); rts_unset / dequeue / enqueue
    // re-borrow `target_idx`'s slot internally. Single-threaded EL1 context, same
    // invariant as `do_vmctl`.
    unsafe {
        if was_runnable {
            // Already on the run queue; the new priority may belong to a
            // different band, so move it.
            sched::dequeue(nr);
            sched::enqueue(nr);
        } else {
            // Off the run queue (preempted, RTS_NO_QUANTUM set): clear the
            // no-quantum block and re-admit. rts_unset enqueues in the new band
            // if this makes the proc runnable.
            sched::rts_unset(&mut proc_table[target_idx], RTS_NO_QUANTUM);
        }
    }

    let n = SCHED_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= SCHED_TRACE_HEAD {
        let _ = writeln!(
            Uart::new(),
            "[ksys SYS_SCHEDULE] target={} nr={} prio={priority} quantum={quantum_ms} result=0",
            name0 as char,
            nr.get(),
        );
    }
    OK
}

/// `SYS_SCHEDCTL` — claim a target for the caller's scheduler, or release it
/// back to the kernel scheduler.
///
/// `SCHEDCTL_FLAG_KERNEL` set → `target.scheduler = NONE` (kernel-scheduled);
/// otherwise the caller claims the target (`target.scheduler = caller`). The
/// change takes effect at the target's next quantum exhaustion (see
/// `sched::reschedule`); no immediate run-queue transition is needed.
pub(super) fn do_schedctl(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    caller_nr: ProcNr,
    msg: &mut Message,
) -> i32 {
    let flags = read_i32(msg, 0);
    let target_e: Endpoint = read_i32(msg, 4);

    let target_idx = match super::resolve_target(proc_table, caller_nr, target_e) {
        Ok(idx) => idx,
        Err(e) => return e,
    };

    // Snapshot the caller's endpoint before borrowing the target mutably (the
    // two may be the same slot when a process claims itself).
    let caller_endpoint = {
        let caller_idx = proc_index(caller_nr).expect("caller in proc table");
        proc_table[caller_idx].endpoint
    };

    let p = &mut proc_table[target_idx];
    p.scheduler = if flags & SCHEDCTL_FLAG_KERNEL != 0 {
        NONE
    } else {
        caller_endpoint
    };
    OK
}

#[inline]
fn read_i32(msg: &Message, off: usize) -> i32 {
    i32::from_ne_bytes(
        msg.payload[off..off + 4]
            .try_into()
            .expect("payload in range"),
    )
}
