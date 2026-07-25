// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Kernel-call clients for the cross-address-space copy engine (slice 5.2's
//! `SYS_SAFECOPY` / `SYS_COPY`).
//!
//! Lifted out of PM in slice 5.3, when the TTY driver became the second holder of
//! `SYS_SAFECOPY` and the marshaling would otherwise have been copied verbatim.
//! The payload layouts are documented on the two constants in
//! `kernel-shared::callnr`; this module is the single place they are written.
//!
//! Like [`crate::sef`], [`crate::ds`], and [`crate::diag`], these issue IPC traps,
//! so the module is excluded from host coverage and verified through the kernel's
//! `[ksys SYS_SAFECOPY]` / `[ksys SYS_COPY]` boot traces.
//!
//! ## The confused-deputy rule
//!
//! A server that holds these calls and serves clients that do not must take the
//! **granter from the kernel-stamped `m_source`**, never from a request payload. A
//! caller-supplied granter endpoint would let any client aim a privileged
//! cross-address-space copy at a third party's memory *through* the server. These
//! functions cannot enforce that — the granter is a parameter — so it is the
//! caller's obligation, and it binds every grant-id-carrying request in the CDEV,
//! BDEV, and FS bands.

use minixrs_ipc::ipc_sendrec;
use minixrs_kernel_shared::callnr::{SYS_COPY, SYS_SAFECOPY};
use minixrs_kernel_shared::com::{SYSTEM, boot_endpoint};
use minixrs_kernel_shared::error::OK;
use minixrs_kernel_shared::{Endpoint, Message};

use crate::payload::{wr_i32, wr_u64};

/// `SYS_SAFECOPY` — copy between the range grant `gid` describes and the
/// caller's own buffer at `addr`.
///
/// `direction` is `SAFECOPY_FROM` (grant → caller) or `SAFECOPY_TO` (caller →
/// grant); `offset` is measured within the granted range, so a client can drive a
/// short-write loop by advancing it. `granter` must be the endpoint the *kernel*
/// stamped on the request being served — see the module docs.
///
/// Returns the kernel-call result: `OK`, or a negative errno. Callers should relay
/// a negative result **verbatim** rather than collapsing it, because the two
/// failures mean different things to the client: `EPERM` is "your grant does not
/// authorize this" and `EFAULT` is "your buffer is not mapped".
pub fn sys_safecopy(
    direction: i32,
    granter: Endpoint,
    gid: i32,
    offset: u64,
    addr: u64,
    bytes: u64,
) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type: SYS_SAFECOPY,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, 0, direction);
    wr_i32(&mut m, 4, granter);
    wr_i32(&mut m, 8, gid);
    wr_u64(&mut m, 16, offset);
    wr_u64(&mut m, 24, addr);
    wr_u64(&mut m, 32, bytes);
    sendrec_to_system(&mut m)
}

/// `SYS_COPY` — raw privileged copy from `(src_e, src_addr)` to
/// `(dst_e, dst_addr)`, with no grant involved. `SELF` names the caller.
///
/// There is no per-target authorization here at all: the caller's `k_call_mask`
/// bit *is* the whole check, the `SYS_VMCTL` trust stance. Only for small
/// control-plane reads on behalf of a caller the server already trusts.
pub fn sys_copy(src_e: Endpoint, src_addr: u64, dst_e: Endpoint, dst_addr: u64, bytes: u64) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type: SYS_COPY,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, 0, src_e);
    wr_i32(&mut m, 4, dst_e);
    wr_u64(&mut m, 16, src_addr);
    wr_u64(&mut m, 24, dst_addr);
    wr_u64(&mut m, 32, bytes);
    sendrec_to_system(&mut m)
}

/// SENDREC a prepared kernel call to `SYSTEM` and flatten the two failure routes.
///
/// A trap-level failure (the SENDREC itself was refused — no `ipc_to` bit, bad
/// endpoint) and a call-level failure (the kernel ran the call and rejected it)
/// arrive by different paths, so both are checked; the same two-step every other
/// `server-rt` client uses.
fn sendrec_to_system(m: &mut Message) -> i32 {
    let trap_rc = ipc_sendrec(boot_endpoint(SYSTEM), m);
    if trap_rc != OK {
        return trap_rc;
    }
    m.m_type
}
