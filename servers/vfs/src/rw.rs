// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `VFS_WRITE` / `VFS_READ` request parsing, validation, and the short-write
//! loop's policy.
//!
//! Everything here is a total function over plain values, so it carries the
//! crate's unit tests; the IPC round-trip lives in `main.rs` and is verified
//! through the boot log. Same split as `drivers/tty`'s `cdev.rs` / `main.rs` and
//! `servers/ds`'s `registry.rs` / `main.rs`.
//!
//! ## Why `read` and `write` share this module
//!
//! It was `write.rs` until slice 5.8, and the rename is the point: `VFS_READ`
//! reuses `VFS_WRITE`'s payload **field for field**, so [`parse`] and [`validate`]
//! serve both without a line of change. And [`advance`]'s four rules turn out to
//! be direction-agnostic — "a peer reporting `0` ends the transfer" is exactly
//! what EOF means on the read side, and "an error after partial progress reports
//! the progress" is POSIX's rule for both.
//!
//! What is *not* shared is the looping. VFS drives `write` to completion because
//! `write()` may not expose a driver's staging limit; it does **not** loop on
//! `read`, because `read()` is allowed to return less than asked for. So `read`
//! uses [`parse`]/[`validate`] and never touches [`advance`] — the asymmetry is
//! in `main.rs`, where the two handlers are.
//!
//! ## Why the loop is a step function
//!
//! [`advance`] holds the whole of what makes a short-write loop correct — when to
//! stop, what to report, and what to do about a driver that answers implausibly —
//! with the SENDREC lifted out. That is what makes those rules testable at all: in
//! the loop itself they are only reachable through a working driver (TTY or the
//! memory driver), so a driver that reported `0`, or more bytes than it was
//! asked for, could not be exercised without breaking the driver. Here they are
//! three lines of test each.
//!
//! ## Where the length rules live, and where they don't
//!
//! [`validate`] owns the `len`/`buf` checks; the descriptor is `fd.rs`'s business
//! and is resolved separately by the caller. The two are deliberately not merged:
//! a descriptor is looked up in per-process state, a length is judged on its own,
//! and only the first grows a mount and a seek position under it.

use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{VFS_BUF_OFF, VFS_FD_OFF, VFS_LEN_OFF};
use minixrs_kernel_shared::error::{EFAULT, EINVAL};
use minixrs_kernel_shared::message::user_range_ok;
use minixrs_server_rt::{rd_i32, rd_u64};

/// A parsed `VFS_WRITE` / `VFS_READ` request. Field-for-field the payload, with
/// no interpretation applied — [`validate`] and `fd::resolve` do that.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Request {
    /// The descriptor to transfer on. Resolved against the caller's fd row.
    pub fd: i32,
    /// Bytes the caller asked to move.
    pub len: i32,
    /// The buffer's address **in the caller's own address space**. Not a grant
    /// id: the caller is an ordinary user process with no grant table, and VFS is
    /// what turns this into a magic grant naming the caller as owner.
    pub buf: u64,
}

/// Read a `VFS_WRITE` or `VFS_READ` request out of a message payload — the two
/// share one payload, so they share one parser.
///
/// Total: every field is a fixed-offset scalar read that cannot fail
/// (`server-rt`'s accessors return `0` for an out-of-range offset), so a
/// malformed request becomes an invalid *value* that the checks reject, never a
/// panic.
pub fn parse(msg: &Message) -> Request {
    Request {
        fd: rd_i32(msg, VFS_FD_OFF),
        len: rd_i32(msg, VFS_LEN_OFF),
        buf: rd_u64(msg, VFS_BUF_OFF),
    }
}

/// Decide how many bytes this request is asking for, or which errno to reply.
///
/// `Ok(0)` is a legal empty transfer, not an error — and it is checked **before**
/// the buffer, so a caller moving zero bytes is never asked to supply a valid
/// pointer (POSIX does not require one) and no grant is issued for it. That last
/// part matters: a zero-length request must not be usable to probe the granting
/// path, which is the rule TTY applies one layer down and the FS server applies
/// one layer up.
///
/// The checks, in order:
///
/// 1. `len < 0` → `EINVAL`. Left unchecked it would widen into a ~16 EiB `u64`
///    byte count on the grant.
/// 2. `len == 0` → `Ok(0)`.
/// 3. The buffer must lie in the user address range — `user_range_ok`, the
///    no-alignment variant grants use, since a `write()` buffer is a byte buffer.
///    This is **defence in depth, not the load-bearing gate**: the kernel's copy
///    engine walks the caller's page tables and answers `EFAULT` for anything
///    unmapped regardless (D5). It stays because rejecting a malformed pointer
///    before issuing a grant is cheaper and more legible than discovering it two
///    hops away.
pub fn validate(len: i32, buf: u64) -> Result<usize, i32> {
    if len < 0 {
        return Err(EINVAL);
    }
    let len = len as usize;
    if len == 0 {
        return Ok(0);
    }
    if !user_range_ok(buf, len as u64) {
        return Err(EFAULT);
    }
    Ok(len)
}

/// What the short-write loop should do after one `CDEV_WRITE` reply.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Step {
    /// Send again from this offset. Only produced while there are bytes left.
    More(usize),
    /// Stop; this is the reply to the client.
    Done(i32),
}

/// Fold one driver reply `n` into a write that has already moved `off` of `len`
/// bytes.
///
/// A character driver may write fewer bytes than asked (`CDEV_MAX_IO`), which is
/// how it stages through a small buffer with no allocator. That is a *driver*
/// detail and POSIX `write()` may not expose it, so the caller re-sends with
/// `offset` advanced. One grant covers the whole buffer; only the offset moves.
///
/// Four rules, in the order they are applied:
///
/// 1. **An error after partial progress reports the progress.** POSIX's rule: the
///    bytes really did go out, and telling the caller otherwise makes it send them
///    twice. The error resurfaces on the next write, where it is still true. With
///    no progress, the error is the reply.
/// 2. **A driver reporting zero ends the write.** No progress means re-sending the
///    same request forever, and a server that spins is worse than a short write.
/// 3. **The offset is clamped to `len`.** A driver reporting *more* than it was
///    asked for would otherwise walk past the buffer and over-report the write.
///    TTY is trusted today, but this is the shape every future block/FS client
///    copies, and a count is the one thing a client acts on unconditionally.
/// 4. Otherwise: done when the buffer is out, else go round again.
pub fn advance(off: usize, len: usize, n: i32) -> Step {
    if n < 0 {
        return Step::Done(if off > 0 { off as i32 } else { n });
    }
    if n == 0 {
        return Step::Done(off as i32);
    }
    let off = off.saturating_add(n as usize).min(len);
    if off >= len {
        Step::Done(off as i32)
    } else {
        Step::More(off)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::callnr::CDEV_MAX_IO;
    use minixrs_server_rt::{wr_i32, wr_u64};

    /// A plausible user buffer: non-zero and well inside the user range.
    const BUF: u64 = 0x0010_0000;

    fn request(fd: i32, len: i32, buf: u64) -> Message {
        let mut m = Message {
            m_source: 0,
            m_type: 0,
            payload: [0u8; 96],
        };
        wr_i32(&mut m, VFS_FD_OFF, fd);
        wr_i32(&mut m, VFS_LEN_OFF, len);
        wr_u64(&mut m, VFS_BUF_OFF, buf);
        m
    }

    #[test]
    fn parse_reads_every_field_from_its_own_offset() {
        // Three distinct values, so a swapped pair of offsets would fail.
        let m = request(1, 64, BUF);
        assert_eq!(
            parse(&m),
            Request {
                fd: 1,
                len: 64,
                buf: BUF,
            }
        );
    }

    #[test]
    fn parse_of_a_zeroed_payload_yields_a_rejectable_request() {
        // A client that sent nothing: fd 0 happens to be open, but length 0 is a
        // no-op that never looks at the (null) buffer. Nothing panics, nothing
        // copies, and no grant is issued.
        let m = Message {
            m_source: 0,
            m_type: 0,
            payload: [0u8; 96],
        };
        let req = parse(&m);
        assert_eq!(req.len, 0);
        assert_eq!(validate(req.len, req.buf), Ok(0));
    }

    #[test]
    fn a_normal_length_passes_through() {
        for len in [1i32, 12, CDEV_MAX_IO as i32, CDEV_MAX_IO as i32 * 4, 4096] {
            assert_eq!(validate(len, BUF), Ok(len as usize), "len {len}");
        }
    }

    #[test]
    fn a_negative_length_is_einval() {
        // Left unchecked this would widen into a ~16 EiB `u64` on the grant.
        for len in [-1i32, -256, i32::MIN] {
            assert_eq!(validate(len, BUF), Err(EINVAL), "len {len}");
        }
    }

    #[test]
    fn a_zero_length_write_is_ok_not_an_error() {
        assert_eq!(validate(0, BUF), Ok(0));
    }

    #[test]
    fn a_zero_length_write_does_not_require_a_valid_buffer() {
        // POSIX does not make `write(fd, junk, 0)` an error, and the check order
        // is what guarantees it: length first, so no grant is ever issued for a
        // zero-length request and it cannot be used to probe the granting path.
        assert_eq!(validate(0, 0), Ok(0));
        assert_eq!(validate(0, u64::MAX), Ok(0));
    }

    #[test]
    fn a_buffer_outside_the_user_range_is_efault() {
        // NULL, a range that overflows, and one that runs past the top of the
        // user address space.
        assert_eq!(validate(8, 0), Err(EFAULT));
        assert_eq!(validate(8, u64::MAX), Err(EFAULT));
        assert_eq!(validate(8, minixrs_kernel_shared::message::USER_VA_TOP), {
            Err(EFAULT)
        });
    }

    #[test]
    fn the_length_check_precedes_the_buffer_check() {
        // A request wrong in two ways reports the *first* problem, so a client
        // fixing one learns about the next.
        assert_eq!(validate(-1, 0), Err(EINVAL));
    }

    #[test]
    fn a_full_write_in_one_go_is_done() {
        assert_eq!(advance(0, 100, 100), Step::Done(100));
    }

    #[test]
    fn a_short_write_asks_for_another_round() {
        // The core of the loop: the driver clamped, so keep going from there.
        assert_eq!(advance(0, 288, 256), Step::More(256));
        assert_eq!(advance(256, 288, 32), Step::Done(288));
    }

    #[test]
    fn a_multi_round_write_reaches_exactly_len() {
        // Drive the whole loop the way `write_all` does, with a driver that
        // clamps to CDEV_MAX_IO every time.
        let len = CDEV_MAX_IO * 3 + 7;
        let mut off = 0usize;
        let mut rounds = 0;
        let total = loop {
            rounds += 1;
            assert!(rounds <= 8, "loop failed to converge");
            let asked = len - off;
            let served = asked.min(CDEV_MAX_IO) as i32;
            match advance(off, len, served) {
                Step::More(next) => {
                    assert!(next > off, "no forward progress");
                    off = next;
                }
                Step::Done(rc) => break rc,
            }
        };
        assert_eq!(total, len as i32);
        assert_eq!(rounds, 4);
    }

    #[test]
    fn an_error_before_any_progress_is_reported_as_the_error() {
        assert_eq!(advance(0, 100, EFAULT), Step::Done(EFAULT));
        assert_eq!(advance(0, 100, EINVAL), Step::Done(EINVAL));
    }

    #[test]
    fn an_error_after_partial_progress_reports_the_progress() {
        // POSIX: those bytes really went out, so reporting the error instead
        // would make the caller send them a second time.
        assert_eq!(advance(64, 100, EFAULT), Step::Done(64));
    }

    #[test]
    fn the_same_parse_and_validate_serve_a_read() {
        // `VFS_READ` reuses `VFS_WRITE`'s payload field for field, which is what
        // the slice-5.8 rename of this module records. The assertion is that a
        // request built for one direction parses identically for the other —
        // there is no read-specific offset, and giving read its own would be a
        // divergence this test turns into a failure.
        let m = request(3, 4096, BUF);
        let req = parse(&m);
        assert_eq!(
            req,
            Request {
                fd: 3,
                len: 4096,
                buf: BUF
            }
        );
        assert_eq!(validate(req.len, req.buf), Ok(4096));

        // The refusals are shared too: a read with a negative length or an
        // unmapped buffer is wrong for exactly the reasons a write is.
        assert_eq!(validate(-1, BUF), Err(EINVAL));
        assert_eq!(validate(8, 0), Err(EFAULT));
    }

    #[test]
    fn advance_is_direction_agnostic_and_zero_means_eof() {
        // The other half of why this module is `rw` and not `write`: `advance`'s
        // four rules never mention a direction. "A peer reporting 0 ends the
        // transfer" is a driver that cannot take more on the write side and
        // *end of file* on the read side — the same rule, and the reason
        // `VFS_READ` needs no step function of its own.
        assert_eq!(advance(0, 100, 0), Step::Done(0));
        assert_eq!(advance(40, 100, 0), Step::Done(40));
    }

    #[test]
    fn a_driver_reporting_zero_ends_the_write_rather_than_spinning() {
        // Unreachable through a working driver (TTY or the memory driver), which
        // is exactly why it needs a test: in the loop this rule is only
        // observable by breaking the driver.
        assert_eq!(advance(0, 100, 0), Step::Done(0));
        assert_eq!(advance(40, 100, 0), Step::Done(40));
    }

    #[test]
    fn a_driver_reporting_more_than_asked_cannot_over_report_the_write() {
        // Also unreachable through a working driver (TTY or the memory driver).
        // Without the clamp the reply would exceed the length the client handed
        // over — the one number a client acts on unconditionally.
        assert_eq!(advance(0, 100, 100_000), Step::Done(100));
        assert_eq!(advance(90, 100, i32::MAX), Step::Done(100));
    }

    #[test]
    fn the_offset_never_exceeds_len_on_any_reply() {
        // The clamp as a property, over the whole interesting space rather than
        // three chosen points.
        let len = 64usize;
        for off in 0..=len {
            for n in [-1i32, 0, 1, 7, 63, 64, 65, 4096, i32::MAX] {
                let rc = match advance(off, len, n) {
                    Step::More(next) => {
                        assert!(next < len, "More past the end: off={off} n={n}");
                        assert!(next > off, "More without progress: off={off} n={n}");
                        continue;
                    }
                    Step::Done(rc) => rc,
                };
                if rc >= 0 {
                    assert!(rc as usize <= len, "over-reported: off={off} n={n} rc={rc}");
                }
            }
        }
    }
}
