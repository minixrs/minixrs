// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_EXIT` — terminate a target process (slice 4.5, "exit-lite").
//!
//! PM's kill path ends here: once PM decides a signal's disposition is
//! terminate (the no-handler default), it issues `SYS_EXIT` on the target —
//! exactly MINIX 3's `pm_exit → sys_exit → clear_proc` shape
//! (kernel/system/do_exit.c). The 4.5 subset stops the process permanently
//! and detaches it from the IPC machinery it can still be entangled in:
//!
//! - cancel its `SYS_SETALARM` timer (a stale `EARLIEST_ALARM` cached minimum
//!   is harmless — the next gated scan finds no due alarm and recomputes);
//! - set `RTS_PROC_STOP` (dequeues via `rts_set` if the target was runnable);
//! - if it was blocked `SENDING`, unlink it from the destination's `caller_q`
//!   so a terminated proc's queued message is never delivered.
//!
//! Deferred to 4.6 (alongside `SYS_FORK`'s slot reuse): unblocking receivers
//! blocked RECEIVE-from-target with `EDEADSRCDST`, address-space teardown via
//! `AddrSpace::destroy`, freeing the slot (`RTS_SLOT_FREE`), and the endpoint
//! generation bump. In 4.5 a terminated slot simply stays allocated and
//! permanently blocked.
//!
//! Target-taking (routed beside `SYS_VMCTL` in `kernel_call_dispatch`); trust
//! model identical to `do_vmctl` — the `k_call_mask` gate is the only check,
//! and PM is the intended sole holder.
//!
//! ## Message payload layout (offsets within `Message::payload`)
//!
//! | offset | field                 | direction |
//! |--------|-----------------------|-----------|
//! |  0..4  | target endpoint (i32) | in        |

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::endpoint::{Endpoint, SELF, endpoint_proc};
use minixrs_kernel_shared::error::{EINVAL, OK};
use minixrs_kernel_shared::message::Message;

use crate::proc::flags::{RTS_PROC_STOP, RTS_SENDING, RTS_SLOT_FREE};
use crate::proc::table::{N_PROC_SLOTS, proc_index};
use crate::proc::{Proc, sched};
use crate::uart::Uart;

/// Leading `SYS_EXIT` calls to trace explicitly — terminations are
/// once-per-boot events in the 4.5 demo, invisible to the modulo-100 `[ksys]`
/// sampler (same reasoning as `do_schedule::SCHED_TRACE_HEAD`).
const EXIT_TRACE_HEAD: u64 = 6;
static EXIT_COUNT: AtomicU64 = AtomicU64::new(0);

/// `SYS_EXIT` — permanently stop the target and detach it from IPC.
pub(super) fn do_exit(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    caller_nr: ProcNr,
    msg: &mut Message,
) -> i32 {
    let target_e: Endpoint = read_i32(msg, 0);

    let target_nr = if target_e == SELF {
        caller_nr
    } else {
        endpoint_proc(target_e)
    };
    let Some(target_idx) = proc_index(target_nr) else {
        return EINVAL;
    };

    let (rts, sendto_e, nr, name0) = {
        let p = &mut proc_table[target_idx];
        let rts = p.rts_flags.load(Ordering::Relaxed);
        if rts & RTS_SLOT_FREE != 0 {
            return EINVAL;
        }
        p.alarm_at = 0;
        let out = (rts, p.sendto_e, p.nr, p.name[0]);
        // SAFETY: single-threaded EL1 context; the exclusive `p` borrow ends
        // (NLL) as `rts_set` captures `nr` internally — it dequeues the target
        // if it was runnable.
        unsafe { sched::rts_set(p, RTS_PROC_STOP) };
        out
    };

    // A proc blocked SENDING sits on exactly one caller queue — the
    // destination named by its `sendto_e` (classic MINIX `clear_proc` walks
    // only that chain too). Unlink it so the dead proc's queued message is
    // never delivered.
    if rts & RTS_SENDING != 0 {
        unlink_from_caller_q(proc_table, target_idx, sendto_e);
    }

    let n = EXIT_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= EXIT_TRACE_HEAD {
        let _ = writeln!(
            Uart::new(),
            "[ksys SYS_EXIT] target={} nr={} result=0",
            name0 as char,
            nr.get(),
        );
    }
    OK
}

/// Splice `target_idx` out of the `caller_q` chain of the proc named by
/// `sendto_e`. No-op if the destination is invalid or the target isn't on the
/// chain (both indicate the flags and queue already diverged; nothing to fix
/// here). All borrows are sequential single-slot index accesses.
fn unlink_from_caller_q(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    target_idx: usize,
    sendto_e: Endpoint,
) {
    let Some(dst_idx) = proc_index(endpoint_proc(sendto_e)) else {
        return;
    };
    let target_nr = proc_table[target_idx].nr;
    let target_link = proc_table[target_idx].q_link;

    if proc_table[dst_idx].caller_q == Some(target_nr) {
        proc_table[dst_idx].caller_q = target_link;
        proc_table[target_idx].q_link = None;
        return;
    }

    // Walk for the predecessor. Chains are Option<ProcNr>-linked and acyclic
    // (each proc is on at most one caller queue), so this terminates.
    let mut cur = proc_table[dst_idx].caller_q;
    while let Some(cur_nr) = cur {
        let Some(cur_idx) = proc_index(cur_nr) else {
            return;
        };
        if proc_table[cur_idx].q_link == Some(target_nr) {
            proc_table[cur_idx].q_link = target_link;
            proc_table[target_idx].q_link = None;
            return;
        }
        cur = proc_table[cur_idx].q_link;
    }
}

#[inline]
fn read_i32(msg: &Message, off: usize) -> i32 {
    i32::from_ne_bytes(
        msg.payload[off..off + 4]
            .try_into()
            .expect("payload in range"),
    )
}
