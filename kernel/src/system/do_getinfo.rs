// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_GETINFO` — kernel-state introspection.
//!
//! The request sub-type lives in the first 4 bytes of the message payload
//! (mirrors MINIX 3 `mess_lsys_krn_sys_getinfo.request`). Phase 2 implements
//! only `GET_WHOAMI`; every other sub-type returns `EINVAL` so a stub that
//! sends an unsupported request gets a recognizable error rather than a
//! silently-zeroed reply.

use minixrs_kernel_shared::callnr::{GET_WHOAMI, SYS_GETINFO_NAME_LEN};
use minixrs_kernel_shared::error::{EINVAL, OK};
use minixrs_kernel_shared::message::Message;

use crate::proc::proc_struct::PROC_NAME_LEN;
use crate::proc::{Priv, Proc};

// The GET_WHOAMI reply embeds `caller.name` verbatim, so the kernel's
// per-slot name field and the wire-format name field must stay the same
// size. If `PROC_NAME_LEN` ever changes, update `SYS_GETINFO_NAME_LEN`
// (and the layout-table comment below) to match.
const _: () = assert!(PROC_NAME_LEN == SYS_GETINFO_NAME_LEN);

/// `SYS_GETINFO` entry point. Dispatches by request sub-type.
pub(super) fn do_getinfo(caller: &mut Proc, caller_priv: &Priv, msg: &mut Message) -> i32 {
    let request = i32::from_ne_bytes(
        msg.payload[0..4]
            .try_into()
            .expect("payload is at least 4 bytes"),
    );
    match request {
        GET_WHOAMI => fill_whoami(caller, caller_priv, msg),
        _ => EINVAL,
    }
}

/// `GET_WHOAMI` reply — fills `msg.payload` in-place. Layout:
///
/// | offset  | type     | meaning                            |
/// |---------|----------|------------------------------------|
/// |   0..4  | i32      | caller endpoint                    |
/// |   4..8  | i32      | `Priv::flags`, zero-extended       |
/// |   8..12 | i32      | init flags (always 0 for Phase 2)  |
/// |  12..28 | [u8; 16] | `Proc::name`, NUL-padded           |
fn fill_whoami(caller: &Proc, caller_priv: &Priv, msg: &mut Message) -> i32 {
    msg.payload[0..4].copy_from_slice(&caller.endpoint.to_ne_bytes());
    msg.payload[4..8].copy_from_slice(&(caller_priv.flags as i32).to_ne_bytes());
    msg.payload[8..12].copy_from_slice(&0_i32.to_ne_bytes());
    msg.payload[12..12 + PROC_NAME_LEN].copy_from_slice(&caller.name);
    OK
}
