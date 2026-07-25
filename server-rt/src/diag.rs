// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Kernel debug-channel client — `SYS_DIAGCTL` (slice 5.1).
//!
//! Servers run at EL0 with no console. [`diag_print`] is how they get text onto
//! the serial line: a `SYS_DIAGCTL` kernel call carrying the text inline in the
//! message payload, which the kernel prints prefixed with the caller's own name.
//!
//! This is the *debug* channel, deliberately independent of the eventual stdio
//! path (VFS → TTY, slices 5.3/5.4) — it has to keep working while those, and
//! the grant machinery they depend on, are still being built.
//!
//! Like [`crate::sef`] and [`crate::ds`], this issues an IPC trap, so it is
//! excluded from host coverage and verified by the QEMU boot trace. The chunk
//! arithmetic is factored out as a pure helper and unit-tested.

use minixrs_ipc::ipc_sendrec;
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{DIAG_TEXT_MAX, DIAG_TEXT_OFF, DIAGCTL_CODE_DIAG, SYS_DIAGCTL};
use minixrs_kernel_shared::com::{SYSTEM, boot_endpoint};
use minixrs_kernel_shared::error::OK;

/// Print `s` to the kernel console via `SYS_DIAGCTL`.
///
/// The kernel prefixes each line with the caller's own name as it knows it, so
/// a server needs no identifying prefix of its own — `diag_print("sef ready")`
/// from VFS emits `[diag vfs] sef ready`.
///
/// Strings longer than [`DIAG_TEXT_MAX`] are split across successive calls, one
/// console line per chunk. Splits happen on **byte** boundaries, not UTF-8
/// character boundaries; the kernel replaces every non-printable-ASCII byte
/// with `.`, so a split can garble a multi-byte character but can never emit a
/// broken line. Keep diag text ASCII.
///
/// Returns `OK`, or the first non-`OK` result — aborting any remaining chunks
/// rather than printing a partial message under a misleading success. An empty
/// string sends nothing and returns `OK`.
pub fn diag_print(s: &str) -> i32 {
    let bytes = s.as_bytes();
    let mut sent = 0usize;
    while sent < bytes.len() {
        let n = chunk_len(bytes.len() - sent);
        let rc = diag_send(&bytes[sent..sent + n]);
        if rc != OK {
            return rc;
        }
        sent += n;
    }
    OK
}

/// Bytes to place in the next `SYS_DIAGCTL` call given `remaining` still to
/// send: the whole remainder, capped at the payload's inline text budget.
///
/// Trivial, but it is the one piece of [`diag_print`] that can be host-tested,
/// and an off-by-one here would silently truncate every long diag line.
fn chunk_len(remaining: usize) -> usize {
    if remaining < DIAG_TEXT_MAX {
        remaining
    } else {
        DIAG_TEXT_MAX
    }
}

/// Issue one `SYS_DIAGCTL(DIAGCTL_CODE_DIAG)` carrying `bytes` inline.
///
/// Returns the IPC trap's error if the SENDREC itself failed, else the kernel's
/// reply `m_type` — the same two-step check `sef_startup` and
/// `sef_publish_to_ds` use, since a trap-level failure and a call-level failure
/// arrive by different routes.
fn diag_send(bytes: &[u8]) -> i32 {
    let mut msg = Message {
        m_source: 0,
        m_type: SYS_DIAGCTL,
        payload: [0u8; 96],
    };
    msg.payload[0..4].copy_from_slice(&DIAGCTL_CODE_DIAG.to_ne_bytes());
    msg.payload[4..8].copy_from_slice(&(bytes.len() as i32).to_ne_bytes());
    msg.payload[DIAG_TEXT_OFF..DIAG_TEXT_OFF + bytes.len()].copy_from_slice(bytes);

    let trap_rc = ipc_sendrec(boot_endpoint(SYSTEM), &mut msg);
    if trap_rc != OK {
        return trap_rc;
    }
    msg.m_type
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_len_passes_short_strings_whole() {
        assert_eq!(chunk_len(0), 0);
        assert_eq!(chunk_len(1), 1);
        assert_eq!(chunk_len(DIAG_TEXT_MAX - 1), DIAG_TEXT_MAX - 1);
    }

    #[test]
    fn chunk_len_caps_at_the_payload_budget() {
        assert_eq!(chunk_len(DIAG_TEXT_MAX), DIAG_TEXT_MAX);
        assert_eq!(chunk_len(DIAG_TEXT_MAX + 1), DIAG_TEXT_MAX);
        assert_eq!(chunk_len(10 * DIAG_TEXT_MAX), DIAG_TEXT_MAX);
    }

    #[test]
    fn chunking_covers_every_byte_exactly_once() {
        // Mirror `diag_print`'s loop without the IPC trap: the chunk lengths
        // must tile the input with no gap, overlap, or zero-length step.
        for total in [
            0,
            1,
            DIAG_TEXT_MAX - 1,
            DIAG_TEXT_MAX,
            DIAG_TEXT_MAX + 1,
            500,
        ] {
            let mut sent = 0usize;
            let mut steps = 0usize;
            while sent < total {
                let n = chunk_len(total - sent);
                assert!(n > 0, "a zero-length chunk would loop forever");
                assert!(n <= DIAG_TEXT_MAX);
                sent += n;
                steps += 1;
                assert!(steps <= total + 1, "chunking failed to converge");
            }
            assert_eq!(sent, total);
        }
    }
}
