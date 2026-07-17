// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_KILL` / `SYS_GETKSIG` / `SYS_ENDKSIG` — the kernel half of minimal
//! signals (slice 4.5).
//!
//! MINIX 3 splits signal work between the kernel and PM: a system process
//! *raises* a signal (`SYS_KILL` → `cause_sig`, kernel/system.c), the kernel
//! queues it on the target and notifies PM, and PM *disposes* of it by
//! draining `SYS_GETKSIG` and acknowledging `SYS_ENDKSIG` (servers/pm/signal.c
//! `ksig_pending`). minix.rs mirrors that shape with a per-proc pending bitmap
//! ([`Proc::sig_pending`]) and the reserved `RTS_SIGNALED` / `RTS_SIG_PENDING`
//! block flags, in real use for the first time here.
//!
//! The 4.5 subset: [`cause_sig`] always queues toward PM (MINIX's
//! non-PM-caller branch); PM's direct signal-as-message delivery to a system
//! proc (`send_sig`) is deferred until a consumer exists (RS restarts). The
//! ksig sink is hardcoded to PM rather than read from the target's `sig_mgr` —
//! RS (the boot-default `sig_mgr`) has no drain loop yet.
//!
//! ## Trust model
//!
//! Identical to `do_vmctl` / `do_schedule`: the single gate is
//! `Priv::k_call_mask`, checked in `kernel_call_dispatch` before we run. A
//! caller that reaches these handlers may name any target. VM (`SYS_KILL`) and
//! PM (`SYS_GETKSIG` / `SYS_ENDKSIG`) are the intended holders; per-target
//! authorization must be added first if a less-trusted process is ever
//! granted these calls.
//!
//! ## Message payload layout (offsets within `Message::payload`)
//!
//! | offset | field                 | direction | call          |
//! |--------|-----------------------|-----------|---------------|
//! |  0..4  | target endpoint (i32) | in        | `SYS_KILL`    |
//! |  4..8  | signal number (i32)   | in        | `SYS_KILL`    |
//! |  0..4  | target endpoint (i32) | out       | `SYS_GETKSIG` |
//! |  4..8  | pending bitmap (u32)  | out       | `SYS_GETKSIG` |
//! |  0..4  | target endpoint (i32) | in        | `SYS_ENDKSIG` |
//!
//! [`Proc::sig_pending`]: crate::proc::Proc::sig_pending

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::com::NR_SYS_PROCS;
use minixrs_kernel_shared::endpoint::{Endpoint, NONE, endpoint_proc};
use minixrs_kernel_shared::error::{EINVAL, OK};
use minixrs_kernel_shared::message::Message;
use minixrs_kernel_shared::signal::NSIG;

use crate::proc::flags::{RTS_SIG_PENDING, RTS_SIGNALED, RTS_SLOT_FREE};
use crate::proc::table::{N_PROC_SLOTS, proc_index};
use crate::proc::{Priv, Proc, sched};
use crate::uart::Uart;

/// Leading calls of each handler to trace explicitly. Signals are
/// once-per-boot events in the 4.5 demo (one SIGSEGV kill chain), far too rare
/// for the modulo-100 `[ksys]` sampler — same reasoning as
/// `do_schedule::SCHED_TRACE_HEAD`.
const KSIG_TRACE_HEAD: u64 = 6;
static KILL_COUNT: AtomicU64 = AtomicU64::new(0);
static GETKSIG_COUNT: AtomicU64 = AtomicU64::new(0);
static ENDKSIG_COUNT: AtomicU64 = AtomicU64::new(0);

/// `SYS_KILL` — raise `signo` on the target named in the payload.
///
/// Validates the signal number and the target slot, then queues the signal via
/// [`cause_sig`]. The reply carries no payload; the disposition happens later,
/// in PM.
pub(super) fn do_kill(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    priv_table: &mut [Priv; NR_SYS_PROCS],
    _caller_nr: ProcNr,
    msg: &mut Message,
) -> i32 {
    let target_e: Endpoint = read_i32(msg, 0);
    let signo = read_i32(msg, 4);

    if !(1..NSIG as i32).contains(&signo) {
        return EINVAL;
    }
    let Some(target_idx) = proc_index(endpoint_proc(target_e)) else {
        return EINVAL;
    };
    let (rts, name0, nr) = {
        let p = &proc_table[target_idx];
        (p.rts_flags.load(Ordering::Relaxed), p.name[0], p.nr)
    };
    if rts & RTS_SLOT_FREE != 0 {
        return EINVAL;
    }

    cause_sig(proc_table, priv_table, target_idx, signo);

    let n = KILL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= KSIG_TRACE_HEAD {
        let _ = writeln!(
            Uart::new(),
            "[ksys SYS_KILL] target={} nr={} sig={signo} -> pm",
            name0 as char,
            nr.get(),
        );
    }
    OK
}

/// Queue `signo` on the target and wake PM (MINIX 3 `cause_sig`).
///
/// Records the signal in the target's pending bitmap, sets the
/// `RTS_SIGNALED | RTS_SIG_PENDING` block flags, and delivers the ksig
/// notification to PM. Setting the flags on an already-blocked target (the
/// SIGSEGV case: the faulter holds `RTS_PAGEFAULT`) just accumulates block
/// bits — `rts_set` sees the proc wasn't runnable and leaves the run queue
/// alone.
fn cause_sig(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    priv_table: &mut [Priv; NR_SYS_PROCS],
    target_idx: usize,
    signo: i32,
) {
    {
        let p = &mut proc_table[target_idx];
        p.sig_pending |= 1u32 << signo;
        // SAFETY: single-threaded EL1 context; the exclusive `p` borrow ends
        // (NLL) as `rts_set` captures `nr` internally, so no other PROC_TABLE
        // borrow aliases.
        unsafe { sched::rts_set(p, RTS_SIGNALED | RTS_SIG_PENDING) };
    }
    // Target borrow is over; deliver on the same passed-in slices (never
    // re-materialize the statics here — the dispatch already holds them
    // exclusively).
    crate::ipc::deliver_ksig(proc_table, priv_table);
}

/// `SYS_GETKSIG` — fetch the next proc with pending kernel signals.
///
/// Replies with the proc's endpoint and pending bitmap, handing the bitmap
/// off (clearing [`Proc::sig_pending`]) but leaving the RTS signal state set
/// until PM acknowledges with `SYS_ENDKSIG`. Replies `NONE` when no proc is
/// pending — PM's drain loop terminates on that.
///
/// [`Proc::sig_pending`]: crate::proc::Proc::sig_pending
pub(super) fn do_getksig(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    _caller_nr: ProcNr,
    msg: &mut Message,
) -> i32 {
    // O(N_PROC_SLOTS) scan, paid once per iteration of PM's drain loop — a
    // once-per-signal event, not a hot path. A MINIX-style resume cursor can
    // come later if it ever shows in traces.
    //
    // Both conditions matter: the RTS bit alone would re-return a proc whose
    // bitmap was already handed off but not yet ENDKSIG-acknowledged.
    for idx in 0..N_PROC_SLOTS {
        let p = &mut proc_table[idx];
        if p.rts_flags.load(Ordering::Relaxed) & RTS_SIG_PENDING != 0 && p.sig_pending != 0 {
            let map = p.sig_pending;
            p.sig_pending = 0; // handed off; RTS state stays until ENDKSIG
            let (endpoint, name0, nr) = (p.endpoint, p.name[0], p.nr);
            write_i32(msg, 0, endpoint);
            write_u32(msg, 4, map);

            let n = GETKSIG_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            if n <= KSIG_TRACE_HEAD {
                let _ = writeln!(
                    Uart::new(),
                    "[ksys SYS_GETKSIG] target={} nr={} map={map:#x}",
                    name0 as char,
                    nr.get(),
                );
            }
            return OK;
        }
    }
    write_i32(msg, 0, NONE);
    write_u32(msg, 4, 0);
    OK
}

/// `SYS_ENDKSIG` — PM finished disposing of the target's signals; clear its
/// signal-pending RTS state.
///
/// The target normally stays blocked via its other flags (`RTS_PAGEFAULT`
/// from the fault, `RTS_PROC_STOP` after a `SYS_EXIT` terminate); a target
/// with no other block bits becomes runnable again — the MINIX "signal
/// handled, resume" case.
pub(super) fn do_endksig(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    _caller_nr: ProcNr,
    msg: &mut Message,
) -> i32 {
    let target_e: Endpoint = read_i32(msg, 0);
    let Some(target_idx) = proc_index(endpoint_proc(target_e)) else {
        return EINVAL;
    };
    let (rts, name0, nr) = {
        let p = &proc_table[target_idx];
        (p.rts_flags.load(Ordering::Relaxed), p.name[0], p.nr)
    };
    if rts & RTS_SIG_PENDING == 0 {
        return EINVAL;
    }

    // SAFETY: single-threaded EL1 context; no other PROC_TABLE borrow is live
    // (the snapshot borrow above ended), and `rts_unset` captures `nr` as its
    // own borrow ends.
    unsafe { sched::rts_unset(&mut proc_table[target_idx], RTS_SIGNALED | RTS_SIG_PENDING) };

    let n = ENDKSIG_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= KSIG_TRACE_HEAD {
        let _ = writeln!(
            Uart::new(),
            "[ksys SYS_ENDKSIG] target={} nr={} result=0",
            name0 as char,
            nr.get(),
        );
    }
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

#[inline]
fn write_i32(msg: &mut Message, off: usize, v: i32) {
    msg.payload[off..off + 4].copy_from_slice(&v.to_ne_bytes());
}

#[inline]
fn write_u32(msg: &mut Message, off: usize, v: u32) {
    msg.payload[off..off + 4].copy_from_slice(&v.to_ne_bytes());
}
