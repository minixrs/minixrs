// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `/dev/null` and `/dev/zero` — the memory driver's character minors, and the
//! pure logic behind them (slice 5.11).
//!
//! MINIX 3's memory driver owns these two devices beside its ramdisks, and so
//! does this one, under MINIX's minor numbers (`NULL_DEV` 3, `ZERO_DEV` 5). They
//! share the driver with the BDEV ramdisk but not a namespace: a minor is
//! per-request-band, so `BDEV_MINOR_RAMDISK` 0 and these two never meet.
//!
//! ## Two things this module is careful about
//!
//! **Nothing is clamped.** `CDEV_MAX_IO` exists because TTY stages through a
//! 256-byte stack buffer. A `/dev/null` write moves nothing at all and a
//! `/dev/zero` read copies from a constant, so both answer the *whole* request in
//! one round; the zero read merely walks the grant in `CDEV_MAX_IO`-sized
//! `SYS_SAFECOPY` calls ([`zero_chunk`]) because that is the constant that
//! already exists.
//!
//! **The check order is TTY's.** Minor, then length, then grant id — so a request
//! that is wrong in two ways hears the same first error from either driver.

use minixrs_kernel_shared::callnr::{CDEV_MAX_IO, CDEV_MINOR_NULL, CDEV_MINOR_ZERO};
use minixrs_kernel_shared::error::{EINVAL, ENXIO};
use minixrs_kernel_shared::grant::grant_valid;
use minixrs_server_rt::cdev::Request;

/// The character minors this driver serves.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Minor {
    /// `/dev/null`: reads are EOF, writes discard.
    Null,
    /// `/dev/zero`: reads fill with zeroes, writes discard.
    Zero,
}

/// Map a minor number to a device, or `ENXIO`.
///
/// `CDEV_MINOR_CONSOLE` (0) is `ENXIO` *here*: it is TTY's minor, and minors are
/// a per-driver namespace.
pub fn classify(minor: i32) -> Result<Minor, i32> {
    match minor {
        CDEV_MINOR_NULL => Ok(Minor::Null),
        CDEV_MINOR_ZERO => Ok(Minor::Zero),
        _ => Err(ENXIO),
    }
}

/// Decide which device `req` names and how many bytes it covers, or which errno
/// to reply.
///
/// 1. Unknown minor → `ENXIO`.
/// 2. `len < 0` → `EINVAL`. Unchecked it would widen into a ~16 EiB `u64`.
/// 3. An invalid grant id → `EINVAL`. Checked even for `/dev/null`, whose write
///    never touches the grant: a client that sent garbage should hear so, and the
///    kernel re-validates everything real on the copies that do happen.
/// 4. Otherwise the full length. **No clamp** — see the module note.
pub fn validate(req: Request) -> Result<(Minor, usize), i32> {
    let minor = classify(req.minor)?;
    if req.len < 0 {
        return Err(EINVAL);
    }
    if !grant_valid(req.gid) {
        return Err(EINVAL);
    }
    Ok((minor, req.len as usize))
}

/// Bytes the next `SYS_SAFECOPY` of a `/dev/zero` read moves, given the request's
/// total and how much has already landed.
///
/// `min(len - done, CDEV_MAX_IO)`, saturating so a caller that has somehow
/// overshot gets `0` (a loop terminator) rather than a wrapped huge count.
pub fn zero_chunk(len: usize, done: usize) -> usize {
    len.saturating_sub(done).min(CDEV_MAX_IO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::callnr::CDEV_MINOR_CONSOLE;
    use minixrs_kernel_shared::grant::{GRANT_INVALID, grant_id};

    const GOOD_GID: i32 = grant_id(3, 1);

    fn req(minor: i32, gid: i32, len: i32) -> Request {
        Request {
            minor,
            gid,
            len,
            offset: 0,
        }
    }

    #[test]
    fn the_two_minors_classify_and_everything_else_is_enxio() {
        assert_eq!(classify(CDEV_MINOR_NULL), Ok(Minor::Null));
        assert_eq!(classify(CDEV_MINOR_ZERO), Ok(Minor::Zero));
        // The console is TTY's minor, not this driver's.
        for minor in [CDEV_MINOR_CONSOLE, 1, 2, 4, 6, 7, -1, i32::MAX] {
            assert_eq!(classify(minor), Err(ENXIO), "minor {minor}");
        }
    }

    #[test]
    fn a_normal_request_passes_its_whole_length_through() {
        // No clamp: lengths past CDEV_MAX_IO come back whole (Z4).
        for len in [
            0i32,
            1,
            64,
            CDEV_MAX_IO as i32,
            CDEV_MAX_IO as i32 + 1,
            4096,
            i32::MAX,
        ] {
            assert_eq!(
                validate(req(CDEV_MINOR_ZERO, GOOD_GID, len)),
                Ok((Minor::Zero, len as usize)),
                "len {len}"
            );
            assert_eq!(
                validate(req(CDEV_MINOR_NULL, GOOD_GID, len)),
                Ok((Minor::Null, len as usize)),
                "len {len}"
            );
        }
    }

    #[test]
    fn a_negative_length_is_einval() {
        for len in [-1i32, -256, i32::MIN] {
            assert_eq!(
                validate(req(CDEV_MINOR_ZERO, GOOD_GID, len)),
                Err(EINVAL),
                "len {len}"
            );
        }
    }

    #[test]
    fn an_invalid_grant_id_is_einval_even_for_null() {
        for gid in [GRANT_INVALID, -2, i32::MIN] {
            assert_eq!(
                validate(req(CDEV_MINOR_NULL, gid, 16)),
                Err(EINVAL),
                "gid {gid}"
            );
            assert_eq!(
                validate(req(CDEV_MINOR_ZERO, gid, 16)),
                Err(EINVAL),
                "gid {gid}"
            );
        }
    }

    #[test]
    fn the_minor_check_precedes_the_length_and_grant_checks() {
        // TTY's order, so a doubly-wrong request hears the same first error from
        // either driver.
        assert_eq!(validate(req(9, GRANT_INVALID, -5)), Err(ENXIO));
        // ...and a bad grant is reported on a zero-length request, not masked.
        assert_eq!(
            validate(req(CDEV_MINOR_NULL, GRANT_INVALID, 0)),
            Err(EINVAL)
        );
    }

    #[test]
    fn zero_chunk_walks_the_request_in_cdev_max_io_steps() {
        assert_eq!(zero_chunk(64, 0), 64);
        assert_eq!(zero_chunk(CDEV_MAX_IO, 0), CDEV_MAX_IO);
        assert_eq!(zero_chunk(CDEV_MAX_IO + 1, 0), CDEV_MAX_IO);
        assert_eq!(zero_chunk(CDEV_MAX_IO + 1, CDEV_MAX_IO), 1);
        assert_eq!(zero_chunk(4096, 4096), 0);
        // Overshoot saturates to a loop terminator rather than wrapping.
        assert_eq!(zero_chunk(10, 11), 0);
    }
}
