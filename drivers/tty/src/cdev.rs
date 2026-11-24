// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `CDEV_WRITE` and `CDEV_READ` validation — the driver's pure logic.
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
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{EINVAL, ENXIO};
use minixrs_kernel_shared::grant::grant_valid;
use minixrs_server_rt::cdev::Request;

/// Decide how many bytes a `CDEV_WRITE` moves for `req`, or which errno to reply.
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
    validate(req)
}

/// Decide the most a `CDEV_READ` may move for `req`, or which errno to reply
/// (slice 6.5, spec R1).
///
/// The rules and their order are [`validate_write`]'s, and both call the same
/// function, so the two directions cannot drift. Nothing in them depends on the
/// direction: the access bit a grant must carry (`CPF_WRITE` for a read) is the
/// kernel's check, never re-derived here.
///
/// `Ok(n)` is an upper bound, not the reply. The count actually moved is
/// [`read_len`] of it and what the input ring holds. `Ok(0)` is the exception:
/// the driver answers `0` for it *before* it looks at the ring, so a zero-length
/// read of an empty console is `0` and not `EAGAIN`.
pub fn validate_read(req: Request) -> Result<usize, i32> {
    validate(req)
}

/// The rule set `CDEV_WRITE` and `CDEV_READ` share; see [`validate_write`] for
/// the checks and why they come in this order.
fn validate(req: Request) -> Result<usize, i32> {
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

/// How many bytes one `CDEV_READ` moves: the least of what the client asked for
/// (`want`), what is `buffered`, and [`CDEV_MAX_IO`] (spec R1).
///
/// `want` normally comes from [`validate_read`] and is already clamped; the
/// clamp is repeated here so the result fits the staging buffer whatever the
/// caller passes. A result of `0` with a non-zero `want` means nothing is
/// buffered, which the driver answers with `EAGAIN`, never with a `0` reply
/// (that would be EOF).
pub fn read_len(want: usize, buffered: usize) -> usize {
    want.min(buffered).min(CDEV_MAX_IO)
}

/// What the driver does with one validated `CDEV_READ` (slice 6.5, spec R1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadPlan {
    /// A zero-length read: reply `0` and touch nothing.
    Zero,
    /// TTY holds no interrupt line, so no byte can ever arrive: reply `EIO`.
    NoLine,
    /// Nothing is buffered: record the caller as the waiter and reply `EAGAIN`.
    Empty,
    /// Copy this many of the oldest bytes to the caller, then consume them.
    /// Never `0`.
    Copy(usize),
}

/// Decide how a `CDEV_READ` that passed [`validate_read`] with `want` is
/// answered, given how many bytes are `buffered` and whether TTY claimed its
/// interrupt line at boot.
///
/// The order is the contract (spec R1, Review Focus 3):
///
/// 1. `want == 0` is [`ReadPlan::Zero`] **before the ring is looked at**, so a
///    zero-length read of an empty console is `0` and never `EAGAIN` — and it
///    records no waiter, so it cannot be used to steal the one `NOTIFY`.
/// 2. No line is [`ReadPlan::NoLine`]: `EAGAIN` promises a notification, and
///    without the line there is no interrupt to send one from. Parking a reader
///    behind it would be a hang with no marker.
/// 3. Nothing buffered is [`ReadPlan::Empty`].
/// 4. Otherwise [`ReadPlan::Copy`] of [`read_len`].
pub fn read_plan(want: usize, buffered: usize, line_claimed: bool) -> ReadPlan {
    if want == 0 {
        return ReadPlan::Zero;
    }
    if !line_claimed {
        return ReadPlan::NoLine;
    }
    match read_len(want, buffered) {
        0 => ReadPlan::Empty,
        n => ReadPlan::Copy(n),
    }
}

/// The one endpoint TTY owes a `NOTIFY` (spec R1).
///
/// A `CDEV_READ` that finds nothing buffered is answered `EAGAIN` and its
/// caller recorded here; when input next arrives that endpoint is notified
/// once and forgotten, and it asks again. One slot, not a queue: every reader
/// goes through VFS, and the notification means "ask again", not "this read is
/// ready". A second asker replaces the first — the last asker wins.
pub struct Waiter(Option<Endpoint>);

impl Waiter {
    /// Nobody is waiting.
    pub const fn new() -> Self {
        Self(None)
    }

    /// Record `who` as the endpoint to notify, replacing any earlier one.
    pub fn record(&mut self, who: Endpoint) {
        self.0 = Some(who);
    }

    /// The endpoint to notify, if any — and forget it, so one arrival of
    /// input sends one notification.
    pub fn take(&mut self) -> Option<Endpoint> {
        self.0.take()
    }
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

    // ---- CDEV_READ (slice 6.5, spec R1) ----

    #[test]
    fn a_normal_read_passes_its_length_through() {
        for len in [1i32, 15, 64, CDEV_MAX_IO as i32 - 1, CDEV_MAX_IO as i32] {
            let req = parse(&request(CDEV_MINOR_CONSOLE, GOOD_GID, len, 0));
            assert_eq!(validate_read(req), Ok(len as usize), "len {len}");
        }
    }

    /// Review Focus 3: `Ok(0)` is what lets the driver answer `0` *before* it
    /// looks at the ring, so a zero-length read of an empty console is `0`
    /// and never `EAGAIN`.
    #[test]
    fn a_zero_length_read_is_ok_zero() {
        let req = parse(&request(CDEV_MINOR_CONSOLE, GOOD_GID, 0, 0));
        assert_eq!(validate_read(req), Ok(0));
    }

    #[test]
    fn a_negative_read_length_is_einval() {
        for len in [-1i32, -256, i32::MIN] {
            let req = parse(&request(CDEV_MINOR_CONSOLE, GOOD_GID, len, 0));
            assert_eq!(validate_read(req), Err(EINVAL), "len {len}");
        }
    }

    #[test]
    fn a_read_of_an_unknown_minor_is_enxio() {
        for minor in [1i32, 7, -1, i32::MAX] {
            let req = parse(&request(minor, GOOD_GID, 16, 0));
            assert_eq!(validate_read(req), Err(ENXIO), "minor {minor}");
        }
    }

    #[test]
    fn an_over_long_read_is_clamped_not_refused() {
        for len in [
            CDEV_MAX_IO as i32 + 1,
            CDEV_MAX_IO as i32 * 2,
            4096,
            i32::MAX,
        ] {
            assert_eq!(
                validate_read(parse(&request(CDEV_MINOR_CONSOLE, GOOD_GID, len, 0))),
                Ok(CDEV_MAX_IO),
                "len {len}"
            );
        }
    }

    #[test]
    fn a_read_with_an_invalid_grant_id_is_einval() {
        for gid in [GRANT_INVALID, -2, i32::MIN] {
            let req = parse(&request(CDEV_MINOR_CONSOLE, gid, 16, 0));
            assert_eq!(validate_read(req), Err(EINVAL), "gid {gid}");
        }
    }

    #[test]
    fn a_read_reports_the_first_problem_in_the_same_order_a_write_does() {
        let req = parse(&request(9, GRANT_INVALID, -5, 0));
        assert_eq!(validate_read(req), Err(ENXIO));

        // A bad grant is an error even at length zero, exactly as for a write.
        let req = parse(&request(CDEV_MINOR_CONSOLE, GRANT_INVALID, 0, 0));
        assert_eq!(validate_read(req), Err(EINVAL));
    }

    /// The two directions share one rule set; nothing may drift between them.
    #[test]
    fn validate_read_agrees_with_validate_write_on_every_case() {
        for minor in [CDEV_MINOR_CONSOLE, 1, -1] {
            for gid in [GOOD_GID, 0, GRANT_INVALID, i32::MIN] {
                for len in [i32::MIN, -1, 0, 1, 256, 257, i32::MAX] {
                    let req = parse(&request(minor, gid, len, 0));
                    assert_eq!(
                        validate_read(req),
                        validate_write(req),
                        "minor {minor} gid {gid} len {len}"
                    );
                }
            }
        }
    }

    #[test]
    fn read_len_is_the_least_of_want_buffered_and_the_staging_size() {
        // Limited by what was asked for.
        assert_eq!(read_len(1, 15), 1);
        assert_eq!(read_len(64, 200), 64);
        // Limited by what is buffered: a short read.
        assert_eq!(read_len(64, 15), 15);
        assert_eq!(read_len(CDEV_MAX_IO, 1), 1);
        // Equal on both sides.
        assert_eq!(read_len(15, 15), 15);
        assert_eq!(read_len(CDEV_MAX_IO, CDEV_MAX_IO), CDEV_MAX_IO);
    }

    #[test]
    fn read_len_is_zero_when_either_side_is_zero() {
        assert_eq!(read_len(0, 0), 0);
        assert_eq!(read_len(0, 100), 0);
        assert_eq!(read_len(100, 0), 0);
        assert_eq!(read_len(usize::MAX, 0), 0);
        assert_eq!(read_len(0, usize::MAX), 0);
    }

    /// The staging buffer is `CDEV_MAX_IO` bytes whatever the ring holds and
    /// whatever a caller that skipped `validate_read` passes.
    #[test]
    fn read_len_never_exceeds_cdev_max_io() {
        assert_eq!(read_len(CDEV_MAX_IO + 1, CDEV_MAX_IO + 1), CDEV_MAX_IO);
        assert_eq!(read_len(usize::MAX, usize::MAX), CDEV_MAX_IO);
        assert_eq!(read_len(usize::MAX, CDEV_MAX_IO + 44), CDEV_MAX_IO);
        assert_eq!(read_len(CDEV_MAX_IO + 44, usize::MAX), CDEV_MAX_IO);
    }

    // ---- the read plan and the waiter (slice 6.5, spec R1) ----

    /// Review Focus 3: the zero-length answer comes before the empty check
    /// and before the no-line check, whatever the ring holds.
    #[test]
    fn a_zero_length_read_is_planned_before_anything_else() {
        for buffered in [0usize, 1, CDEV_MAX_IO] {
            for line_claimed in [true, false] {
                assert_eq!(
                    read_plan(0, buffered, line_claimed),
                    ReadPlan::Zero,
                    "buffered {buffered} line {line_claimed}"
                );
            }
        }
    }

    #[test]
    fn an_empty_ring_is_empty_not_a_zero_byte_copy() {
        for want in [1usize, 15, CDEV_MAX_IO] {
            assert_eq!(read_plan(want, 0, true), ReadPlan::Empty, "want {want}");
        }
    }

    /// `EAGAIN` owes a `NOTIFY`; with no interrupt line none can be sent.
    #[test]
    fn a_read_with_no_line_is_refused_rather_than_parked() {
        for want in [1usize, 64, CDEV_MAX_IO] {
            for buffered in [0usize, 5] {
                assert_eq!(
                    read_plan(want, buffered, false),
                    ReadPlan::NoLine,
                    "want {want} buffered {buffered}"
                );
            }
        }
    }

    #[test]
    fn buffered_input_is_copied_up_to_what_was_asked_for() {
        assert_eq!(read_plan(64, 15, true), ReadPlan::Copy(15));
        assert_eq!(read_plan(4, 15, true), ReadPlan::Copy(4));
        assert_eq!(read_plan(1, 1, true), ReadPlan::Copy(1));
        assert_eq!(
            read_plan(CDEV_MAX_IO, CDEV_MAX_IO, true),
            ReadPlan::Copy(CDEV_MAX_IO)
        );
        // Whatever the caller passes, the copy fits the staging buffer.
        assert_eq!(
            read_plan(usize::MAX, usize::MAX, true),
            ReadPlan::Copy(CDEV_MAX_IO)
        );
    }

    #[test]
    fn a_new_waiter_holds_nobody() {
        let mut w = Waiter::new();
        assert_eq!(w.take(), None);
    }

    #[test]
    fn a_recorded_waiter_is_taken_once_and_then_forgotten() {
        let mut w = Waiter::new();
        w.record(0x1234);
        assert_eq!(w.take(), Some(0x1234));
        // One arrival of input, one notification.
        assert_eq!(w.take(), None);
    }

    /// Spec R1: a second, different endpoint asking while a waiter is recorded
    /// replaces it. Unreachable at boot (only VFS sends TTY a `CDEV_READ`), so
    /// this test is what pins it.
    #[test]
    fn the_last_asker_wins() {
        let mut w = Waiter::new();
        w.record(0x1111);
        w.record(0x2222);
        assert_eq!(w.take(), Some(0x2222));
        assert_eq!(w.take(), None);
    }

    #[test]
    fn the_same_asker_twice_is_one_waiter() {
        let mut w = Waiter::new();
        w.record(0x1111);
        w.record(0x1111);
        assert_eq!(w.take(), Some(0x1111));
        assert_eq!(w.take(), None);
    }

    #[test]
    fn a_waiter_can_be_recorded_again_after_it_was_taken() {
        let mut w = Waiter::new();
        w.record(0x1111);
        assert_eq!(w.take(), Some(0x1111));
        w.record(0x1111);
        assert_eq!(w.take(), Some(0x1111));
    }
}
