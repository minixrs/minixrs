// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_PRIVCTL` — set up a target process's privilege slot (slice 4.5).
//!
//! MINIX 3's RS drives service startup through `do_privctl`
//! (kernel/system/do_privctl.c): a new process is created frozen on
//! `RTS_NO_PRIV`, given a privilege structure, and released. The 4.5 subset
//! implements exactly one subcode, `PRIVCTL_SET_USER`: point a frozen target
//! at the shared USER priv slot ([`USER_PRIV_ID`]) and release it — MINIX's
//! `SYS_PRIV_USER` / `get_priv(rp, 0)` semantics, where every ordinary user
//! process shares one priv structure. The 4.6 fork path leans on this to
//! hand forked children a privilege; the system-proc variants (`SET_SYS`,
//! IPC/IO/IRQ grants) arrive with RS service starts.
//!
//! The `RTS_NO_PRIV` gate doubles as the authorization model: only a process
//! deliberately built frozen (stub E at boot; forked children in 4.6) can be
//! re-privileged, so the call can't strip or swap a live process's privilege.
//! Beyond that the trust model matches `do_vmctl`: `k_call_mask` is the only
//! caller check, and PM is the intended sole holder.
//!
//! ## Message payload layout (offsets within `Message::payload`)
//!
//! | offset | field                 | direction |
//! |--------|-----------------------|-----------|
//! |  0..4  | target endpoint (i32) | in        |
//! |  4..8  | subcode (i32)         | in        |
//!
//! [`USER_PRIV_ID`]: crate::proc::table::USER_PRIV_ID

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::callnr::PRIVCTL_SET_USER;
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{EINVAL, EPERM, OK};
use minixrs_kernel_shared::message::Message;

use crate::proc::flags::RTS_NO_PRIV;
use crate::proc::table::{N_PROC_SLOTS, USER_PRIV_ID};
use crate::proc::{Proc, sched};
use crate::uart::Uart;

/// Leading `SYS_PRIVCTL` calls traced explicitly, plus an every-100th steady
/// sample — 4.6's fork loop releases every child through here, so a head-only
/// trace (the 4.5 shape, when stub E's release was a once-per-boot event)
/// would go silent seconds into boot.
const PRIVCTL_TRACE_HEAD: u64 = 6;
const PRIVCTL_TRACE_EVERY: u64 = 100;
static PRIVCTL_COUNT: AtomicU64 = AtomicU64::new(0);

/// `SYS_PRIVCTL` — install the shared USER privilege on a frozen target and
/// release it.
pub(super) fn do_privctl(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    caller_nr: ProcNr,
    msg: &mut Message,
) -> i32 {
    let target_e: Endpoint = read_i32(msg, 0);
    let subcode = read_i32(msg, 4);

    if subcode != PRIVCTL_SET_USER {
        return EINVAL;
    }

    let target_idx = match super::resolve_target(proc_table, caller_nr, target_e) {
        Ok(idx) => idx,
        Err(e) => return e,
    };

    let (nr, name0) = {
        let p = &mut proc_table[target_idx];
        // Only a deliberately frozen process may be (re-)privileged; a live
        // one keeps the privilege it booted with. (A `SELF` target is always
        // running, so it lands here too.)
        if p.rts_flags.load(Ordering::Relaxed) & RTS_NO_PRIV == 0 {
            return EPERM;
        }
        p.priv_id = Some(USER_PRIV_ID);
        let out = (p.nr, p.name[0]);
        // SAFETY: single-threaded EL1 context; the exclusive `p` borrow ends
        // (NLL) as `rts_unset` captures `nr` internally — it enqueues the
        // target if `RTS_NO_PRIV` was its only block bit (stub E's case).
        unsafe { sched::rts_unset(p, RTS_NO_PRIV) };
        out
    };

    let n = PRIVCTL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= PRIVCTL_TRACE_HEAD || n.is_multiple_of(PRIVCTL_TRACE_EVERY) {
        let _ = writeln!(
            Uart::new(),
            "[ksys SYS_PRIVCTL] target={} nr={} subcode={subcode} result=0",
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
