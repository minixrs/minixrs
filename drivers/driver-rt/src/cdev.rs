// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The driver side of the `CDEV_READ` / `CDEV_WRITE` contract — its pure half
//! (slice 6.5, spec R1; moved here from `drivers/tty` in slice 6.6, spec C6).
//!
//! A character driver that can run out of input answers every `CDEV_READ` at
//! once: the count copied, or `EAGAIN` with the caller recorded as its
//! [`Waiter`] and owed one `NOTIFY`. TTY was the first to implement that,
//! `virtio-console` is the second and slice 6.8 adds a third; three copies of
//! "record the waiter before answering `EAGAIN`" would drift, so the decisions
//! live here and each driver's `main` does what they say. The memory driver
//! stays off this module: it never says `EAGAIN`.
//!
//! What is here is plain data and arithmetic: the request validator
//! ([`validate_write`], [`validate_read`]), the byte count of one read
//! ([`read_len`]), the order a read is answered in ([`read_plan`]) and the one
//! endpoint owed a notification ([`Waiter`]). No IPC, no device, no `unsafe`.
//! The four-field parse is `server-rt::cdev`'s (slice 5.11); what a driver adds
//! to it is which minor exists — one per driver, passed in — and the clamp to
//! [`CDEV_MAX_IO`].
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
//! a driver stage through a fixed buffer with no allocator.

use minixrs_kernel_shared::callnr::CDEV_MAX_IO;
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{EINVAL, ENXIO};
use minixrs_kernel_shared::grant::grant_valid;
use minixrs_server_rt::cdev::Request;

/// Decide how many bytes a `CDEV_WRITE` moves for `req` on a driver whose one
/// device is `minor`, or which errno to reply.
///
/// `Ok(n)` means "copy `n` bytes and reply `n`" — including `Ok(0)`, which is a
/// legal zero-length write that needs no copy at all. `Err(e)` is a negative errno
/// for the reply.
///
/// The checks, in the order a driver should apply them:
///
/// 1. `req.minor != minor` → `ENXIO`. Minors are a per-driver namespace and each
///    driver here serves exactly one (TTY passes `CDEV_MINOR_CONSOLE`), so this
///    check is an equality: `/dev/null` and `/dev/zero` are minors of the
///    *memory* driver, not of TTY.
/// 2. `len < 0` → `EINVAL`. A negative length would otherwise widen into a huge
///    `u64` byte count for the kernel copy.
/// 3. An invalid grant id → `EINVAL`. Cheap local reject of the one value that can
///    never name a grant (`GRANT_INVALID`, and anything else negative); the kernel
///    still re-validates index, sequence, access, and grantee.
/// 4. `len == 0` → `Ok(0)`, checked *after* the gid so a zero-length write with a
///    garbage grant is still reported as the error it is.
/// 5. Otherwise clamp to [`CDEV_MAX_IO`].
pub fn validate_write(req: Request, minor: i32) -> Result<usize, i32> {
    validate(req, minor)
}

/// Decide the most a `CDEV_READ` may move for `req` on a driver whose one
/// device is `minor`, or which errno to reply (slice 6.5, spec R1).
///
/// The rules and their order are [`validate_write`]'s, and both call the same
/// function, so the two directions cannot drift. Nothing in them depends on the
/// direction: the access bit a grant must carry (`CPF_WRITE` for a read) is the
/// kernel's check, never re-derived here.
///
/// `Ok(n)` is an upper bound, not the reply. The count actually moved is
/// [`read_len`] of it and what the driver has buffered. `Ok(0)` is the
/// exception: the driver answers `0` for it *before* it looks at its buffer, so a
/// zero-length read of an empty console is `0` and not `EAGAIN`.
pub fn validate_read(req: Request, minor: i32) -> Result<usize, i32> {
    validate(req, minor)
}

/// The rule set `CDEV_WRITE` and `CDEV_READ` share; see [`validate_write`] for
/// the checks and why they come in this order.
fn validate(req: Request, minor: i32) -> Result<usize, i32> {
    if req.minor != minor {
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

/// Whether the recorded waiter is owed its `NOTIFY` now (slice 6.6, C6 and C9).
///
/// `input` is whether the event just handled made bytes readable. `was_stopped`
/// and `stopped` are whether the device could no longer receive before and
/// after it: the transition is what counts, so a reader parked on a device that
/// stops is woken once — its retry then hears `EIO` instead of waiting for an
/// interrupt that cannot come — and nothing is sent for a device that had
/// already stopped. Both at once is still one `NOTIFY`.
///
/// Both character drivers decide through this one function. The call sites
/// themselves are not host-testable, and no boot stops a device, so a call
/// site that ignores the answer is caught by reading only (6.6's mutation 12).
pub const fn wake_due(input: bool, was_stopped: bool, stopped: bool) -> bool {
    input || (stopped && !was_stopped)
}

/// What the driver does with one validated `CDEV_READ` (slice 6.5, spec R1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadPlan {
    /// A zero-length read: reply `0` and touch nothing.
    Zero,
    /// The device has stopped — its interrupt line was never claimed, or it
    /// failed since — so no input can ever arrive: reply `EIO` (slice 6.6,
    /// spec C9).
    Stopped,
    /// Nothing is buffered: record the caller as the waiter and reply `EAGAIN`.
    Empty,
    /// Copy this many of the oldest bytes to the caller, then consume them.
    /// Never `0`.
    Copy(usize),
}

/// Decide how a `CDEV_READ` that passed [`validate_read`] with `want` is
/// answered, given how many bytes are `buffered` and whether the driver
/// `can_receive` — whether input can still arrive.
///
/// `can_receive` is `false` for a device that will never deliver another
/// byte: TTY when its interrupt claim was refused or its line has stopped, a
/// virtio driver once it has marked its device `FAILED`. It is `true` for one
/// that is merely not receiving *yet* (TTY during its delivery proof): input
/// will come, so `EAGAIN` and its notification are still a promise the driver
/// can keep.
///
/// The order is the contract (spec R1, Review Focus 3; slice 6.6, Review
/// Focus 4):
///
/// 1. `want == 0` is [`ReadPlan::Zero`] **before the buffer is looked at**, so
///    a zero-length read of an empty console is `0` and never `EAGAIN` — and it
///    records no waiter, so it cannot be used to steal the one `NOTIFY`.
/// 2. A driver that cannot receive is [`ReadPlan::Stopped`]: `EAGAIN` promises
///    a notification, and with no interrupt to come there is nothing to send
///    one from. Parking a reader behind it would be a hang with no marker.
///    This comes before the buffer too, so bytes a device delivered before it
///    stopped are not handed out afterwards.
/// 3. Nothing buffered is [`ReadPlan::Empty`].
/// 4. Otherwise [`ReadPlan::Copy`] of [`read_len`].
pub fn read_plan(want: usize, buffered: usize, can_receive: bool) -> ReadPlan {
    if want == 0 {
        return ReadPlan::Zero;
    }
    if !can_receive {
        return ReadPlan::Stopped;
    }
    match read_len(want, buffered) {
        0 => ReadPlan::Empty,
        n => ReadPlan::Copy(n),
    }
}

/// The one endpoint a driver owes a `NOTIFY` (spec R1).
///
/// A `CDEV_READ` that finds nothing buffered is answered `EAGAIN` and its
/// caller recorded here; when input next arrives — or the device stops, so
/// that none ever will (slice 6.6, spec C9) — that endpoint is notified once
/// and forgotten, and it asks again. One slot, not a queue: every reader
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

impl Default for Waiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::Message;
    use minixrs_kernel_shared::callnr::{
        CDEV_GRANT_OFF, CDEV_LEN_OFF, CDEV_MINOR_CONSOLE, CDEV_MINOR_OFF, CDEV_OFFSET_OFF,
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

    /// The minor the moved tests run the validator for: TTY's, as before the
    /// move. `the_one_minor_is_the_drivers_not_a_constant` covers another.
    const MINOR: i32 = CDEV_MINOR_CONSOLE;

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
        assert_eq!(validate_write(req, MINOR), Ok(0));
    }

    #[test]
    fn a_normal_write_passes_its_length_through() {
        for len in [1i32, 12, 64, CDEV_MAX_IO as i32 - 1, CDEV_MAX_IO as i32] {
            let req = parse(&request(MINOR, GOOD_GID, len, 0));
            assert_eq!(validate_write(req, MINOR), Ok(len as usize), "len {len}");
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
                validate_write(parse(&request(MINOR, GOOD_GID, len, 0)), MINOR),
                Ok(CDEV_MAX_IO),
                "len {len}"
            );
        }
    }

    #[test]
    fn a_zero_length_write_is_ok_not_an_error() {
        let req = parse(&request(MINOR, GOOD_GID, 0, 0));
        assert_eq!(validate_write(req, MINOR), Ok(0));
    }

    #[test]
    fn an_unknown_minor_is_enxio() {
        for minor in [1i32, 7, -1, i32::MAX] {
            let req = parse(&request(minor, GOOD_GID, 16, 0));
            assert_eq!(validate_write(req, MINOR), Err(ENXIO), "minor {minor}");
        }
    }

    #[test]
    fn a_negative_length_is_einval() {
        // Left unchecked this would widen into a ~16 EiB `u64` byte count for the
        // kernel copy call.
        for len in [-1i32, -256, i32::MIN] {
            let req = parse(&request(MINOR, GOOD_GID, len, 0));
            assert_eq!(validate_write(req, MINOR), Err(EINVAL), "len {len}");
        }
    }

    #[test]
    fn an_invalid_grant_id_is_einval() {
        for gid in [GRANT_INVALID, -2, i32::MIN] {
            let req = parse(&request(MINOR, gid, 16, 0));
            assert_eq!(validate_write(req, MINOR), Err(EINVAL), "gid {gid}");
        }
    }

    /// Check order matters: a request that is wrong in two ways must report the
    /// *first* problem, so a client fixing one error learns about the next.
    #[test]
    fn the_minor_check_precedes_the_length_and_grant_checks() {
        let req = parse(&request(9, GRANT_INVALID, -5, 0));
        assert_eq!(validate_write(req, MINOR), Err(ENXIO));

        // ...and a bad grant is reported even on a zero-length write, rather than
        // being masked by the `Ok(0)` fast path.
        let req = parse(&request(MINOR, GRANT_INVALID, 0, 0));
        assert_eq!(validate_write(req, MINOR), Err(EINVAL));
    }

    /// Slice 6.6, spec C6: which minor exists is the driver's fact, passed in.
    /// A driver whose device is not minor 0 accepts its own and refuses the
    /// console's, in both directions.
    #[test]
    fn the_one_minor_is_the_drivers_not_a_constant() {
        for minor in [1i32, 7, i32::MAX] {
            let own = parse(&request(minor, GOOD_GID, 16, 0));
            assert_eq!(validate_write(own, minor), Ok(16), "minor {minor}");
            assert_eq!(validate_read(own, minor), Ok(16), "minor {minor}");

            let other = parse(&request(CDEV_MINOR_CONSOLE, GOOD_GID, 16, 0));
            assert_eq!(validate_write(other, minor), Err(ENXIO), "minor {minor}");
            assert_eq!(validate_read(other, minor), Err(ENXIO), "minor {minor}");
        }
    }

    // ---- CDEV_READ (slice 6.5, spec R1) ----

    #[test]
    fn a_normal_read_passes_its_length_through() {
        for len in [1i32, 15, 64, CDEV_MAX_IO as i32 - 1, CDEV_MAX_IO as i32] {
            let req = parse(&request(MINOR, GOOD_GID, len, 0));
            assert_eq!(validate_read(req, MINOR), Ok(len as usize), "len {len}");
        }
    }

    /// Review Focus 3: `Ok(0)` is what lets the driver answer `0` *before* it
    /// looks at the ring, so a zero-length read of an empty console is `0`
    /// and never `EAGAIN`.
    #[test]
    fn a_zero_length_read_is_ok_zero() {
        let req = parse(&request(MINOR, GOOD_GID, 0, 0));
        assert_eq!(validate_read(req, MINOR), Ok(0));
    }

    #[test]
    fn a_negative_read_length_is_einval() {
        for len in [-1i32, -256, i32::MIN] {
            let req = parse(&request(MINOR, GOOD_GID, len, 0));
            assert_eq!(validate_read(req, MINOR), Err(EINVAL), "len {len}");
        }
    }

    #[test]
    fn a_read_of_an_unknown_minor_is_enxio() {
        for minor in [1i32, 7, -1, i32::MAX] {
            let req = parse(&request(minor, GOOD_GID, 16, 0));
            assert_eq!(validate_read(req, MINOR), Err(ENXIO), "minor {minor}");
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
                validate_read(parse(&request(MINOR, GOOD_GID, len, 0)), MINOR),
                Ok(CDEV_MAX_IO),
                "len {len}"
            );
        }
    }

    #[test]
    fn a_read_with_an_invalid_grant_id_is_einval() {
        for gid in [GRANT_INVALID, -2, i32::MIN] {
            let req = parse(&request(MINOR, gid, 16, 0));
            assert_eq!(validate_read(req, MINOR), Err(EINVAL), "gid {gid}");
        }
    }

    #[test]
    fn a_read_reports_the_first_problem_in_the_same_order_a_write_does() {
        let req = parse(&request(9, GRANT_INVALID, -5, 0));
        assert_eq!(validate_read(req, MINOR), Err(ENXIO));

        // A bad grant is an error even at length zero, exactly as for a write.
        let req = parse(&request(MINOR, GRANT_INVALID, 0, 0));
        assert_eq!(validate_read(req, MINOR), Err(EINVAL));
    }

    /// The two directions share one rule set; nothing may drift between them.
    #[test]
    fn validate_read_agrees_with_validate_write_on_every_case() {
        for minor in [MINOR, 1, -1] {
            for gid in [GOOD_GID, 0, GRANT_INVALID, i32::MIN] {
                for len in [i32::MIN, -1, 0, 1, 256, 257, i32::MAX] {
                    let req = parse(&request(minor, gid, len, 0));
                    assert_eq!(
                        validate_read(req, MINOR),
                        validate_write(req, MINOR),
                        "minor {minor} gid {gid} len {len}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_waiter_is_woken_by_input_or_by_the_device_stopping_once() {
        // Input always wakes, whatever the device's state did.
        for (was, now) in [(false, false), (false, true), (true, true)] {
            assert!(wake_due(true, was, now), "input, {was} -> {now}");
        }
        // No input: only the transition into stopped wakes.
        assert!(wake_due(false, false, true));
        assert!(!wake_due(false, false, false));
        // Already stopped: nothing more is owed.
        assert!(!wake_due(false, true, true));
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
    /// and before the cannot-receive check, whatever the ring holds.
    #[test]
    fn a_zero_length_read_is_planned_before_anything_else() {
        for buffered in [0usize, 1, CDEV_MAX_IO] {
            for can_receive in [true, false] {
                assert_eq!(
                    read_plan(0, buffered, can_receive),
                    ReadPlan::Zero,
                    "buffered {buffered} can_receive {can_receive}"
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

    /// `EAGAIN` owes a `NOTIFY`; with no interrupt line none can be sent. (The
    /// name is from before the move: "no line" was then the only way a driver
    /// could be unable to receive.)
    #[test]
    fn a_read_with_no_line_is_refused_rather_than_parked() {
        for want in [1usize, 64, CDEV_MAX_IO] {
            for buffered in [0usize, 5] {
                assert_eq!(
                    read_plan(want, buffered, false),
                    ReadPlan::Stopped,
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

    // ---- a device that has stopped (slice 6.6, spec C9) ----

    /// Once input can no longer arrive, a non-zero read is `Stopped` — the
    /// driver's `EIO` — and never `Empty`: an `EAGAIN` there would park the
    /// reader behind a notification nothing is left to send.
    #[test]
    fn a_non_zero_read_of_a_stopped_device_is_stopped_never_empty() {
        for want in [1usize, 15, CDEV_MAX_IO, usize::MAX] {
            for buffered in [0usize, 1, CDEV_MAX_IO, usize::MAX] {
                assert_eq!(
                    read_plan(want, buffered, false),
                    ReadPlan::Stopped,
                    "want {want} buffered {buffered}"
                );
            }
        }
    }

    /// The zero-length answer still comes first: a stopped device answers `0`
    /// to a zero-length read, not `EIO`.
    #[test]
    fn a_zero_length_read_of_a_stopped_device_is_still_zero() {
        for buffered in [0usize, 1, CDEV_MAX_IO, usize::MAX] {
            assert_eq!(
                read_plan(0, buffered, false),
                ReadPlan::Zero,
                "buffered {buffered}"
            );
        }
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
