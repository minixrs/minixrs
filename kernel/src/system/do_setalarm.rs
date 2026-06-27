// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_SETALARM` — arm a per-process one-shot timer (slice 4.4).
//!
//! Replaces the slice-2.6 `ENOSYS` placeholder. The caller asks for a timer a
//! relative number of clock ticks in the future; the kernel records an absolute
//! deadline on the caller's [`Proc::alarm_at`] and folds it into the
//! `clock::EARLIEST_ALARM` fast-path gate. On the tick where the deadline is
//! reached, `clock::tick` → [`crate::ipc::fire_expired_alarms`] delivers a
//! kernel-originated `NOTIFY` from `CLOCK` to the owner and disarms the timer.
//! A periodic alarm is just a re-arm on each fire (what RS does).
//!
//! This is a *caller-local* kernel call — it acts only on the caller, never a
//! target named in the message — so it takes a single `&mut Proc` and is
//! dispatched through [`dispatch_caller_local`](super::dispatch_caller_local),
//! unlike the target-taking `do_vmctl` / `do_schedule`.
//!
//! ## Message payload layout (offsets within `Message::payload`)
//!
//! | offset | field                         | direction |
//! |--------|-------------------------------|-----------|
//! |  0..8  | delta ticks (u64); 0 = cancel | in        |
//! |  0..8  | previous time-left (u64)      | out       |
//!
//! Returning the previous timer's remaining ticks mirrors MINIX 3's
//! `SYS_SETALARM` reply (`m_lsys_krn_sys_setalarm.time_left`). RS ignores it.
//!
//! [`Proc::alarm_at`]: crate::proc::Proc::alarm_at

use minixrs_kernel_shared::error::OK;
use minixrs_kernel_shared::message::Message;

use crate::clock;
use crate::proc::{Priv, Proc};

/// `SYS_SETALARM`. Arms (or cancels, when `delta == 0`) the caller's one-shot
/// alarm and replies with the previous timer's remaining ticks.
pub(super) fn do_setalarm(caller: &mut Proc, _caller_priv: &Priv, msg: &mut Message) -> i32 {
    let delta = read_u64(msg, 0);
    let now = clock::uptime();

    // Previous timer's remaining ticks (0 if it was disarmed or already due).
    let prev_remaining = caller.alarm_at.saturating_sub(now);

    if delta == 0 {
        // Cancel. The stale `alarm_at` is left in `EARLIEST_ALARM`'s cached
        // minimum, but `fire_expired_alarms` recomputes the gate from the live
        // `alarm_at` fields on the next fire, so a cancelled timer simply never
        // fires (its slot reads `alarm_at == 0`).
        caller.alarm_at = 0;
    } else {
        let at = now.saturating_add(delta);
        caller.alarm_at = at;
        clock::arm_alarm(at);
    }

    write_u64(msg, 0, prev_remaining);
    OK
}

/// Read a native-endian u64 from payload `off..off+8`.
fn read_u64(m: &Message, off: usize) -> u64 {
    u64::from_ne_bytes(
        m.payload[off..off + 8]
            .try_into()
            .expect("payload in range"),
    )
}

/// Write a native-endian u64 into payload `off..off+8`.
fn write_u64(m: &mut Message, off: usize, v: u64) {
    m.payload[off..off + 8].copy_from_slice(&v.to_ne_bytes());
}
