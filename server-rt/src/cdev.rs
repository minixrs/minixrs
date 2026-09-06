// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The CDEV request codec — the four payload fields `CDEV_WRITE` and `CDEV_READ`
//! share (slice 5.11, lifted out of `drivers/tty`).
//!
//! Two drivers decode this payload now: TTY for the console, `memory` for
//! `/dev/null` and `/dev/zero`. So the parse lives here, the way slice 5.3
//! lifted `rd_i32` and friends the moment a second server needed them.
//! **Validation stays in each driver.** Which minors exist and whether a request
//! is clamped are driver facts, not band facts — TTY clamps to `CDEV_MAX_IO`
//! because it stages through a stack buffer, the memory driver clamps nothing
//! because it stages nothing.
//!
//! **There is no granter field, and no way to express one.** A driver takes the
//! granter from the kernel-stamped `m_source`; a payload field would make every
//! grant-holding driver a confused deputy, aiming a privileged cross-address-space
//! copy wherever its client pointed.

use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    CDEV_GRANT_OFF, CDEV_LEN_OFF, CDEV_MINOR_OFF, CDEV_OFFSET_OFF,
};

use crate::payload::{rd_i32, rd_u64};

/// A parsed `CDEV_WRITE` or `CDEV_READ` request. Field-for-field the payload,
/// with no interpretation applied — the driver's validator does that.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Request {
    /// Device minor. A per-driver namespace: `CDEV_MINOR_CONSOLE` on TTY,
    /// `CDEV_MINOR_NULL` / `CDEV_MINOR_ZERO` on the memory driver.
    pub minor: i32,
    /// Grant id naming the client's buffer. The access bit it must carry is the
    /// one the *direction* needs — `CPF_READ` for a write, `CPF_WRITE` for a read
    /// — checked by the kernel's `verify_grant`, never re-derived by a driver.
    pub gid: i32,
    /// Bytes the client asked for. May be negative on the wire; the validator
    /// rejects that before it can widen into a huge `u64`.
    pub len: i32,
    /// Offset within the granted range to start at. Advanced by the client across
    /// a short-write loop; range-checked against the grant by the kernel, so it
    /// passes through here unvalidated.
    pub offset: u64,
}

/// Read a CDEV request out of a message payload.
///
/// Total: every field is a fixed-offset scalar read that cannot fail (the payload
/// accessors return `0` for an out-of-range offset), so a malformed request
/// becomes an invalid *value* the driver's validator rejects, never a panic.
pub fn parse(msg: &Message) -> Request {
    Request {
        minor: rd_i32(msg, CDEV_MINOR_OFF),
        gid: rd_i32(msg, CDEV_GRANT_OFF),
        len: rd_i32(msg, CDEV_LEN_OFF),
        offset: rd_u64(msg, CDEV_OFFSET_OFF),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::{wr_i32, wr_u64};

    /// Build a well-formed CDEV payload. The granter is deliberately not a
    /// parameter — there is no field for it.
    fn request(minor: i32, gid: i32, len: i32, offset: u64) -> Message {
        let mut m = Message {
            m_source: 0,
            m_type: 0,
            payload: [0u8; 96],
        };
        wr_i32(&mut m, CDEV_MINOR_OFF, minor);
        wr_i32(&mut m, CDEV_GRANT_OFF, gid);
        wr_i32(&mut m, CDEV_LEN_OFF, len);
        wr_u64(&mut m, CDEV_OFFSET_OFF, offset);
        m
    }

    #[test]
    fn parse_reads_every_field_from_its_own_offset() {
        // Four distinct values, so a swapped pair of offsets would fail.
        let m = request(5, 0x0030_0001, 64, 4096);
        assert_eq!(
            parse(&m),
            Request {
                minor: 5,
                gid: 0x0030_0001,
                len: 64,
                offset: 4096,
            }
        );
    }

    #[test]
    fn parse_of_a_zeroed_payload_is_all_zeroes_and_does_not_panic() {
        let m = Message {
            m_source: 0,
            m_type: 0,
            payload: [0u8; 96],
        };
        assert_eq!(
            parse(&m),
            Request {
                minor: 0,
                gid: 0,
                len: 0,
                offset: 0,
            }
        );
    }

    #[test]
    fn the_offset_is_passed_through_unvalidated() {
        // Range-checking it is the kernel's job (`verify_grant` tests
        // `offset + bytes <= grant.len`); clamping it here would break the
        // short-write loop a client drives with it.
        for offset in [0u64, 1, 256, u64::MAX] {
            assert_eq!(parse(&request(0, 1, 16, offset)).offset, offset);
        }
    }
}
