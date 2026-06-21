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
//! Like [`crate::sef`], this issues an IPC trap, so it is excluded from host
//! coverage and verified by the QEMU boot trace.

use minixrs_ipc::ipc_sendrec;
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{DS_PUBLISH, SYS_GETINFO_NAME_LEN};
use minixrs_kernel_shared::com::{DS_PROC_NR, boot_endpoint};
use minixrs_kernel_shared::error::OK;

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
