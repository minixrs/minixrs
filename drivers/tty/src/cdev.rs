// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `CDEV_WRITE` validation — the driver's pure logic.
//!
//! The four-field parse moved to `server-rt::cdev` in slice 5.11, when the
//! memory driver became the second decoder of this payload. What stays here is
//! everything TTY-specific: which minor exists (only the console), and the
//! clamp to [`CDEV_MAX_IO`] that a stack staging buffer forces.
//!
//! ## Two things this module is careful about
//!
//! **The granter is not here.** [`Request`] carries only the four payload
//! fields; the granter comes from the kernel-stamped `m_source`, which the caller
//! passes straight to `SYS_SAFECOPY`. A granter *field* would make any
//! grant-holding driver a confused deputy, aiming a privileged cross-address-space
//! copy wherever its client pointed. There is deliberately no way to express one
//! through this API.
//!
//! **An over-long request is a short write, not an error.** [`validate_write`]
//! clamps to [`CDEV_MAX_IO`] and reports the clamped count; the client re-sends
//! with `offset` advanced. That is POSIX `write()`'s contract, and it is what lets
//! the driver stage through a fixed buffer in its `main` frame with no allocator.

use minixrs_kernel_shared::callnr::{CDEV_MAX_IO, CDEV_MINOR_CONSOLE};
use minixrs_kernel_shared::error::{EINVAL, ENXIO};
use minixrs_kernel_shared::grant::grant_valid;
use minixrs_server_rt::cdev::Request;

/// Decide how many bytes to move for `req`, or which errno to reply.
///
/// `Ok(n)` means "copy `n` bytes and reply `n`" — including `Ok(0)`, which is a
/// legal zero-length write that needs no copy at all. `Err(e)` is a negative errno
/// for the reply.
///
/// The checks, in the order a driver should apply them:
///
/// 1. `minor != CDEV_MINOR_CONSOLE` → `ENXIO`. `/dev/null` and `/dev/zero` are
///    minors of the *memory* driver, not of TTY, so this check stays an equality.
/// 2. `len < 0` → `EINVAL`. A negative length would otherwise widen into a huge
///    `u64` byte count for the kernel copy.
/// 3. An invalid grant id → `EINVAL`. Cheap local reject of the one value that can
///    never name a grant (`GRANT_INVALID`, and anything else negative); the kernel
///    still re-validates index, sequence, access, and grantee.
/// 4. `len == 0` → `Ok(0)`, checked *after* the gid so a zero-length write with a
///    garbage grant is still reported as the error it is.
/// 5. Otherwise clamp to [`CDEV_MAX_IO`].
pub fn validate_write(req: Request) -> Result<usize, i32> {
    if req.minor != CDEV_MINOR_CONSOLE {
        return Err(ENXIO);
    }
    if req.len < 0 {
        return Err(EINVAL);
    }
    if !grant_valid(req.gid) {
        return Err(EINVAL);
    }
    let len = req.len as usize;
    if len > CDEV_MAX_IO {
        return Ok(CDEV_MAX_IO);
    }
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::Message;
    use minixrs_kernel_shared::callnr::{
        CDEV_GRANT_OFF, CDEV_LEN_OFF, CDEV_MINOR_OFF, CDEV_OFFSET_OFF,
    };
    use minixrs_kernel_shared::grant::{GRANT_INVALID, grant_id};
    use minixrs_server_rt::cdev::parse;
    use minixrs_server_rt::{wr_i32, wr_u64};

    /// Build a well-formed `CDEV_WRITE` payload. The `granter` is deliberately not
    /// a parameter — there is no field for it.
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

    const GOOD_GID: i32 = grant_id(3, 1);

    #[test]
    fn parse_of_a_zeroed_payload_yields_a_rejectable_request() {
        // A client that sent nothing at all: minor 0 happens to be the console,
        // but grant id 0 has sequence 0 and index 0, and length 0 is a no-op. The
        // point is that nothing panics and nothing copies.
        let m = Message {
            m_source: 0,
            m_type: 0,
            payload: [0u8; 96],
        };
        let req = parse(&m);
        assert_eq!(req.len, 0);
        assert_eq!(validate_write(req), Ok(0));
    }

    #[test]
    fn a_normal_write_passes_its_length_through() {
        for len in [1i32, 12, 64, CDEV_MAX_IO as i32 - 1, CDEV_MAX_IO as i32] {
            let req = parse(&request(CDEV_MINOR_CONSOLE, GOOD_GID, len, 0));
            assert_eq!(validate_write(req), Ok(len as usize), "len {len}");
        }
    }

    #[test]
    fn an_over_long_write_is_clamped_not_refused() {
        // The short-write contract: the driver moves what it can and *reports* it,
        // so the client can advance `offset` and come back. A refusal here would
        // make every write longer than 256 bytes impossible.
        for len in [
            CDEV_MAX_IO as i32 + 1,
            CDEV_MAX_IO as i32 * 2,
            4096,
            i32::MAX,
        ] {
            assert_eq!(
                validate_write(parse(&request(CDEV_MINOR_CONSOLE, GOOD_GID, len, 0))),
                Ok(CDEV_MAX_IO),
                "len {len}"
            );
        }
    }

    #[test]
    fn a_zero_length_write_is_ok_not_an_error() {
        let req = parse(&request(CDEV_MINOR_CONSOLE, GOOD_GID, 0, 0));
        assert_eq!(validate_write(req), Ok(0));
    }

    #[test]
    fn an_unknown_minor_is_enxio() {
        for minor in [1i32, 7, -1, i32::MAX] {
            let req = parse(&request(minor, GOOD_GID, 16, 0));
            assert_eq!(validate_write(req), Err(ENXIO), "minor {minor}");
        }
    }

    #[test]
    fn a_negative_length_is_einval() {
        // Left unchecked this would widen into a ~16 EiB `u64` byte count for the
        // kernel copy call.
        for len in [-1i32, -256, i32::MIN] {
            let req = parse(&request(CDEV_MINOR_CONSOLE, GOOD_GID, len, 0));
            assert_eq!(validate_write(req), Err(EINVAL), "len {len}");
        }
    }

    #[test]
    fn an_invalid_grant_id_is_einval() {
        for gid in [GRANT_INVALID, -2, i32::MIN] {
            let req = parse(&request(CDEV_MINOR_CONSOLE, gid, 16, 0));
            assert_eq!(validate_write(req), Err(EINVAL), "gid {gid}");
        }
    }

    /// Check order matters: a request that is wrong in two ways must report the
    /// *first* problem, so a client fixing one error learns about the next.
    #[test]
    fn the_minor_check_precedes_the_length_and_grant_checks() {
        let req = parse(&request(9, GRANT_INVALID, -5, 0));
        assert_eq!(validate_write(req), Err(ENXIO));

        // ...and a bad grant is reported even on a zero-length write, rather than
        // being masked by the `Ok(0)` fast path.
        let req = parse(&request(CDEV_MINOR_CONSOLE, GRANT_INVALID, 0, 0));
        assert_eq!(validate_write(req), Err(EINVAL));
    }
}
