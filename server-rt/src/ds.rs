// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! DS (Data Store) client glue used by the SEF init path.
//!
//! Every server publishes its own endpoint to DS at startup (slice 4.2) so the
//! others can look it up by name. The marshaling is trivial and identical for
//! all servers, so it lives here rather than being copied into each server's
//! `init_fresh` callback. DS itself is the one exception: it seeds its own entry
//! in-process (a SENDREC to self would deadlock), so it must *not* call this.
//!
//! [`sef_retrieve_from_ds`] is the other half — the lookup a client does instead
//! of hard-coding a peer's boot endpoint. Read its ordering note before relying
//! on it: publish-before-retrieve is *not* guaranteed by construction.
//!
//! Like [`crate::sef`], this issues an IPC trap, so it is excluded from host
//! coverage and verified by the QEMU boot trace.

use minixrs_ipc::ipc_sendrec;
use minixrs_kernel_shared::callnr::{DS_PUBLISH, DS_RETRIEVE, SYS_GETINFO_NAME_LEN};
use minixrs_kernel_shared::com::{DS_PROC_NR, boot_endpoint};
use minixrs_kernel_shared::error::OK;
use minixrs_kernel_shared::{Endpoint, Message};

use crate::payload::rd_i32;

/// Publish the caller's own endpoint to DS under `name` (the server's NUL-padded
/// name, as learned from `GET_WHOAMI` and passed to the SEF init callback).
///
/// Builds a key-only `DS_PUBLISH` request — key in payload
/// `0..SYS_GETINFO_NAME_LEN` — and SENDRECs it to DS. The endpoint to register
/// is *not* sent in the payload: DS records the kernel-stamped `m_source`, so a
/// server can only ever publish itself and cannot spoof another's endpoint.
/// Returns `OK` on success, the IPC trap's error if the SENDREC failed, or DS's
/// negative reply `m_type`. Intended to be the body of a server's `init_fresh`
/// callback.
///
/// Ordering note: the SENDREC blocks the caller until DS is in its receive loop
/// and replies. That is safe at boot — DS's own init does no IPC, so DS reaches
/// its loop and never SENDRECs back to a publisher during its own init (no
/// cycle).
pub fn sef_publish_to_ds(name: &[u8; SYS_GETINFO_NAME_LEN]) -> i32 {
    let mut msg = Message {
        m_source: 0,
        m_type: DS_PUBLISH,
        payload: [0u8; 96],
    };
    msg.payload[0..SYS_GETINFO_NAME_LEN].copy_from_slice(name);

    let trap_rc = ipc_sendrec(boot_endpoint(DS_PROC_NR), &mut msg);
    if trap_rc != OK {
        return trap_rc;
    }
    msg.m_type
}

/// Look up the endpoint another server published under `name`.
///
/// Sends `DS_RETRIEVE` with the key in payload `0..SYS_GETINFO_NAME_LEN` and reads
/// the endpoint back out of payload `16..20`. `Err` carries DS's negative reply
/// (`ESRCH` when the key is not registered) or the IPC trap's own error.
///
/// **Ordering is not guaranteed.** DS serves requests in the order they reach its
/// FIFO, and a server's `DS_PUBLISH` happens inside its own `sef_startup` — so
/// whether a lookup succeeds depends on whether the *target* was packed earlier in
/// the MXBI archive than the *caller*. Today it always is for the one pairing that
/// matters (TTY is packed before VFS), but a caller must not treat that as load-
/// bearing: **fall back to `boot_endpoint(…)` on `Err`** and make the fallback
/// visible in a diag line, so a boot where the ordering shifted still works and
/// still says so.
pub fn sef_retrieve_from_ds(name: &[u8; SYS_GETINFO_NAME_LEN]) -> Result<Endpoint, i32> {
    let mut msg = Message {
        m_source: 0,
        m_type: DS_RETRIEVE,
        payload: [0u8; 96],
    };
    msg.payload[0..SYS_GETINFO_NAME_LEN].copy_from_slice(name);

    let trap_rc = ipc_sendrec(boot_endpoint(DS_PROC_NR), &mut msg);
    if trap_rc != OK {
        return Err(trap_rc);
    }
    if msg.m_type != OK {
        return Err(msg.m_type);
    }
    Ok(rd_i32(&msg, 16))
}
