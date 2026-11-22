// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The BDEV request codec *and* its validation — the four payload fields
//! `BDEV_READ` and `BDEV_WRITE` share, and the one rule for which requests a
//! block driver refuses (slice 6.3, lifted out of `drivers/memory`).
//!
//! Two drivers serve this band now: `memory` for the ramdisk and, from slice
//! 6.3, `virtio-blk` for the disk. MFS's denial battery has to read identically
//! on both backends, so both drivers must refuse the same requests with the same
//! errno in the same order — and two copies of the checks would drift.
//! [`cdev`](crate::cdev) is the precedent, but it shares only the parse; BDEV
//! shares the validation too.
//!
//! ## Which minors exist is the driver's; everything after that is shared
//!
//! Through slice 6.3 the validation had no per-driver part at all: both drivers
//! had one minor, and the one thing a driver supplied was its device's block
//! count. Slice 6.4 gives `virtio-blk` partitions as minors, so the *first*
//! check — does this minor exist, and how many blocks is it — now differs by
//! driver, while the rest must not. The validation is therefore two functions:
//!
//! - [`validate_range`] is checks 2–6: length, grant id, the zero-length fast
//!   path, and the block range against a block count. It never looks at the
//!   minor.
//! - [`validate_read`] is the minor check for a driver whose only minor is
//!   [`BDEV_MINOR_ROOT`], followed by [`validate_range`].
//!
//! `memory` has only minor 0 and calls [`validate_read`] with the image's block
//! count, as it always has. `virtio-blk` resolves the minor to an extent itself
//! — `ENXIO` if it has none — and then calls [`validate_range`] with **that
//! extent's** block count; the byte offset it gets back is relative to the
//! extent, and adding the extent's first block is the driver's, after the range
//! check. Either way a request meets the minor check first and then the same
//! five checks in the same order, which is what keeps the refusal order shared.
//!
//! Direction-agnostic since slice 5.10a: `BDEV_READ` and `BDEV_WRITE` carry the
//! same four fields in the same places, so one parse and one validation serve
//! both and only the driver's dispatch knows which way the bytes go.
//!
//! Everything here is a total function over a [`Message`] or a scalar, so it
//! carries unit tests. The IPC round-trip and the kernel calls live in each
//! driver's `main.rs` and are verified through the boot log.
//!
//! ## Three things this module is careful about
//!
//! **The granter is not here.** [`parse_read`] reads only the four payload
//! fields; the granter comes from the kernel-stamped `m_source`. A granter *field*
//! would make any grant-holding driver a confused deputy, aiming a privileged
//! cross-address-space copy wherever its client pointed. There is deliberately no
//! way to express one through this API — and no grant *offset* either, because
//! every client through slice 5.9 grants a buffer whose block starts at offset 0.
//!
//! **An over-long request is `EINVAL`, not a short read.** This is the deliberate
//! departure from `CDEV_WRITE`'s clamp. A short *write* is a POSIX contract every
//! client already loops over; a short *block read* is useless, because a
//! filesystem cannot interpret half a block, so clamping would push a retry loop
//! into every FS caller for nothing.
//!
//! **An out-of-range block is `EINVAL`, not `EIO`.** A block device's size is
//! known to its client (MFS reads it from the superblock's `s_zones`), so asking
//! past the end is a caller bug. `EIO` is for a device that failed a
//! well-formed request, which `virtio-blk` reports and `memory` never can.

use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    BDEV_BLOCK_OFF, BDEV_BLOCK_SIZE, BDEV_GRANT_OFF, BDEV_LEN_OFF, BDEV_MAX_IO, BDEV_MINOR_NONE,
    BDEV_MINOR_OFF, BDEV_MINOR_ROOT,
};
use minixrs_kernel_shared::error::{EINVAL, ENXIO};
use minixrs_kernel_shared::grant::grant_valid;

use crate::payload::{rd_i32, rd_u64};

// `BDEV_MINOR_NONE` is the minor a denial probe names for "no such device". A
// driver that calls `validate_read` has the root minor and no other, so the
// probe's premise holds on it for as long as the two differ.
const _: () = assert!(BDEV_MINOR_NONE != BDEV_MINOR_ROOT);

/// A parsed `BDEV_READ` (or `BDEV_WRITE`) request. Field-for-field the payload,
/// with no interpretation applied — [`validate_read`] and [`validate_range`] do
/// that.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct ReadRequest {
    /// Device minor. Which minors exist is each driver's own (slice 6.4):
    /// `memory` has only [`BDEV_MINOR_ROOT`] and lets [`validate_read`] refuse
    /// the rest; `virtio-blk` resolves a minor to a partition's extent itself,
    /// answers `ENXIO` if there is none, and passes that extent's block count
    /// to [`validate_range`], which never reads this field.
    pub minor: i32,
    /// Grant id naming the client's buffer. The access bit it must carry is the
    /// one the *direction* needs — `CPF_WRITE` for a `BDEV_READ`, whose copy
    /// lands in the client's buffer, `CPF_READ` for a `BDEV_WRITE`, whose copy is
    /// taken out of it. The kernel checks grantee and access; a driver
    /// re-implements neither, or the grant rules would have a second place to
    /// drift.
    pub gid: i32,
    /// Bytes the client asked for. Must not exceed [`BDEV_MAX_IO`].
    pub len: i32,
    /// Block number within the device.
    pub block: u64,
}

/// Read a `BDEV_READ` request out of a message payload.
///
/// Total: every field is a fixed-offset scalar read that cannot fail
/// (the payload accessors return `0` for an out-of-range offset), so a malformed
/// request becomes an invalid *value* that validation rejects, never a panic.
pub fn parse_read(msg: &Message) -> ReadRequest {
    ReadRequest {
        minor: rd_i32(msg, BDEV_MINOR_OFF),
        gid: rd_i32(msg, BDEV_GRANT_OFF),
        len: rd_i32(msg, BDEV_LEN_OFF),
        block: rd_u64(msg, BDEV_BLOCK_OFF),
    }
}

/// Decide which bytes of the device `req` names, or which errno to reply — for a
/// driver whose only minor is [`BDEV_MINOR_ROOT`].
///
/// `Ok((byte_offset, len))` means "copy `len` bytes starting `byte_offset` into
/// the device, and reply `len`" — including `Ok((0, 0))`, a legal zero-length read
/// that needs no copy at all. `Err(e)` is a negative errno for the reply.
///
/// The checks, in the order a driver should apply them — each is the first thing
/// that can be wrong given the ones before it:
///
/// 1. `minor != BDEV_MINOR_ROOT` → `ENXIO`. The *device* does not exist;
///    nothing is wrong with the request.
/// 2. `len < 0` → `EINVAL`. A negative length would otherwise widen into a huge
///    `u64` byte count for the kernel copy.
/// 3. `len > BDEV_MAX_IO` → `EINVAL`. See the module docs.
/// 4. An invalid grant id → `EINVAL`. Cheap local reject of the one value that can
///    never name a grant; the kernel still re-validates index, sequence, access,
///    and grantee.
/// 5. `len == 0` → `Ok((0, 0))`, checked *after* the gid so a zero-length read
///    with a garbage grant is still reported as the error it is (TTY's rule).
/// 6. `block` at or past `device_blocks`, or a range that would run off the end →
///    `EINVAL`. `checked_mul`/`checked_add` throughout: servers ship `--release`
///    with `overflow-checks = false`, where `block * 4096` on a large block simply
///    *wraps* and would name a valid offset inside the device.
///
/// Check 1 is this function's own; checks 2–6 are [`validate_range`], which a
/// driver with more than one minor calls directly after resolving the minor
/// itself.
pub fn validate_read(req: ReadRequest, device_blocks: u64) -> Result<(u64, usize), i32> {
    if req.minor != BDEV_MINOR_ROOT {
        return Err(ENXIO);
    }
    validate_range(req, device_blocks)
}

/// Checks 2–6 of [`validate_read`], in the same order with the same errnos, and
/// **without** the minor check: decide which bytes of a `blocks`-long extent
/// `req` names (slice 6.4).
///
/// For a driver that has more than one minor. `virtio-blk` resolves `req.minor`
/// to an extent first — answering `ENXIO` itself if there is none, which is
/// check 1 in the driver's hands — and then passes that extent's block count
/// here. `req.minor` is not read: a non-root minor is not refused, and a caller
/// that skips its own minor check has no minor check at all.
///
/// The `byte_offset` in `Ok((byte_offset, len))` is relative to the **start of
/// the extent**, not of the disk. Adding the extent's first block is the
/// caller's, by `checked_add`, after this has accepted the request — so a
/// request can never reach past its own partition into the next one.
pub fn validate_range(req: ReadRequest, blocks: u64) -> Result<(u64, usize), i32> {
    if req.len < 0 {
        return Err(EINVAL);
    }
    if req.len as usize > BDEV_MAX_IO {
        return Err(EINVAL);
    }
    if !grant_valid(req.gid) {
        return Err(EINVAL);
    }
    let len = req.len as usize;
    if len == 0 {
        // Nothing is copied, so there is no offset to be out of range.
        return Ok((0, 0));
    }
    if req.block >= blocks {
        return Err(EINVAL);
    }
    let byte_off = req
        .block
        .checked_mul(BDEV_BLOCK_SIZE as u64)
        .ok_or(EINVAL)?;
    let end = byte_off.checked_add(len as u64).ok_or(EINVAL)?;
    let extent_bytes = blocks.checked_mul(BDEV_BLOCK_SIZE as u64).ok_or(EINVAL)?;
    if end > extent_bytes {
        return Err(EINVAL);
    }
    Ok((byte_off, len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::{wr_i32, wr_u64};
    use minixrs_kernel_shared::grant::{GRANT_INVALID, grant_id};

    /// The slice-5.7 image: 256 blocks of 4 KiB.
    const BLOCKS: u64 = 256;
    const GOOD_GID: i32 = grant_id(3, 1);

    /// Build a well-formed `BDEV_READ` payload. The `granter` is deliberately not
    /// a parameter — there is no field for it — and neither is a grant offset.
    fn request(minor: i32, gid: i32, len: i32, block: u64) -> Message {
        let mut m = Message {
            m_source: 0,
            m_type: 0,
            payload: [0u8; 96],
        };
        wr_i32(&mut m, BDEV_MINOR_OFF, minor);
        wr_i32(&mut m, BDEV_GRANT_OFF, gid);
        wr_i32(&mut m, BDEV_LEN_OFF, len);
        wr_u64(&mut m, BDEV_BLOCK_OFF, block);
        m
    }

    #[test]
    fn parse_reads_every_field_from_its_own_offset() {
        // Four distinct values, so a swapped pair of offsets would fail.
        let m = request(0, GOOD_GID, 64, 17);
        assert_eq!(
            parse_read(&m),
            ReadRequest {
                minor: 0,
                gid: GOOD_GID,
                len: 64,
                block: 17,
            }
        );
    }

    #[test]
    fn parse_of_a_zeroed_payload_yields_a_rejectable_request() {
        // A client that sent nothing at all: minor 0 happens to be the root device,
        // but grant id 0 has sequence 0 and index 0, and length 0 is a no-op. The
        // point is that nothing panics and nothing copies.
        let m = Message {
            m_source: 0,
            m_type: 0,
            payload: [0u8; 96],
        };
        let req = parse_read(&m);
        assert_eq!(req.len, 0);
        assert_eq!(validate_read(req, BLOCKS), Ok((0, 0)));
    }

    #[test]
    fn a_normal_read_resolves_to_its_byte_offset() {
        for block in [0u64, 1, 5, BLOCKS - 1] {
            let req = parse_read(&request(BDEV_MINOR_ROOT, GOOD_GID, 32, block));
            assert_eq!(
                validate_read(req, BLOCKS),
                Ok((block * BDEV_BLOCK_SIZE as u64, 32)),
                "block {block}"
            );
        }
    }

    #[test]
    fn a_whole_block_read_of_the_last_block_ends_exactly_at_the_device_end() {
        // The boundary the range check exists for: this is legal, and one byte
        // more would not be.
        let req = parse_read(&request(
            BDEV_MINOR_ROOT,
            GOOD_GID,
            BDEV_MAX_IO as i32,
            BLOCKS - 1,
        ));
        assert_eq!(
            validate_read(req, BLOCKS),
            Ok(((BLOCKS - 1) * BDEV_BLOCK_SIZE as u64, BDEV_MAX_IO))
        );
    }

    #[test]
    fn an_over_long_read_is_refused_not_clamped() {
        // The deliberate departure from CDEV. A driver that clamped here would
        // hand MFS half a block and report the truncation in a return value MFS
        // has no way to act on.
        for len in [BDEV_MAX_IO as i32 + 1, BDEV_MAX_IO as i32 * 2, i32::MAX] {
            let req = parse_read(&request(BDEV_MINOR_ROOT, GOOD_GID, len, 0));
            assert_eq!(validate_read(req, BLOCKS), Err(EINVAL), "len {len}");
        }
    }

    #[test]
    fn a_zero_length_read_is_ok_not_an_error() {
        let req = parse_read(&request(BDEV_MINOR_ROOT, GOOD_GID, 0, 0));
        assert_eq!(validate_read(req, BLOCKS), Ok((0, 0)));
    }

    #[test]
    fn an_unknown_minor_is_enxio() {
        for minor in [1i32, 7, -1, i32::MAX] {
            let req = parse_read(&request(minor, GOOD_GID, 32, 0));
            assert_eq!(validate_read(req, BLOCKS), Err(ENXIO), "minor {minor}");
        }
    }

    #[test]
    fn the_minor_nobody_has_is_enxio_whatever_else_the_request_says() {
        // What MFS's `bad-minor` (a read) and `wr-minor` (a write, block 1)
        // denial probes send a driver with one minor.
        for (len, block) in [(32, 0), (32, 1), (0, 0), (-1, 0), (32, u64::MAX)] {
            let req = parse_read(&request(BDEV_MINOR_NONE, GOOD_GID, len, block));
            assert_eq!(validate_read(req, BLOCKS), Err(ENXIO), "{len} {block}");
        }
    }

    #[test]
    fn a_negative_length_is_einval() {
        // Left unchecked this would widen into a ~16 EiB `u64` byte count for the
        // kernel copy call.
        for len in [-1i32, -4096, i32::MIN] {
            let req = parse_read(&request(BDEV_MINOR_ROOT, GOOD_GID, len, 0));
            assert_eq!(validate_read(req, BLOCKS), Err(EINVAL), "len {len}");
        }
    }

    #[test]
    fn an_invalid_grant_id_is_einval() {
        for gid in [GRANT_INVALID, -2, i32::MIN] {
            let req = parse_read(&request(BDEV_MINOR_ROOT, gid, 32, 0));
            assert_eq!(validate_read(req, BLOCKS), Err(EINVAL), "gid {gid}");
        }
    }

    #[test]
    fn a_block_past_the_device_is_einval() {
        for block in [BLOCKS, BLOCKS + 1, u64::MAX] {
            let req = parse_read(&request(BDEV_MINOR_ROOT, GOOD_GID, 32, block));
            assert_eq!(validate_read(req, BLOCKS), Err(EINVAL), "block {block}");
        }
    }

    /// The reason every step uses `checked_*`. Servers ship `--release` with
    /// `overflow-checks = false`, so `block * 4096` on a huge block number wraps
    /// silently — and a wrapped product is a *valid-looking* offset inside the
    /// device, which is the worst possible outcome for a range check.
    #[test]
    fn a_block_number_that_would_overflow_its_byte_offset_is_refused() {
        // `u64::MAX / 4096 + 1` is the smallest block whose byte offset wraps.
        let wrapping = u64::MAX / BDEV_BLOCK_SIZE as u64 + 1;
        assert_eq!(wrapping.wrapping_mul(BDEV_BLOCK_SIZE as u64), 0);
        let req = parse_read(&request(BDEV_MINOR_ROOT, GOOD_GID, 32, wrapping));
        // Caught by the device-size check before the multiply is even reached,
        // which is itself the point: the cheap check comes first.
        assert_eq!(validate_read(req, BLOCKS), Err(EINVAL));
        // ...and it is still refused for a device large enough to reach it.
        assert_eq!(validate_read(req, u64::MAX), Err(EINVAL));
    }

    /// Check order matters: a request that is wrong in two ways must report the
    /// *first* problem, so a client fixing one error learns about the next.
    #[test]
    fn the_minor_check_precedes_every_other_check() {
        let req = parse_read(&request(9, GRANT_INVALID, -5, u64::MAX));
        assert_eq!(validate_read(req, BLOCKS), Err(ENXIO));

        // ...and a bad grant is reported even on a zero-length read, rather than
        // being masked by the `Ok((0, 0))` fast path.
        let req = parse_read(&request(BDEV_MINOR_ROOT, GRANT_INVALID, 0, 0));
        assert_eq!(validate_read(req, BLOCKS), Err(EINVAL));
    }

    // --- `validate_range` on its own (slice 6.4) ---------------------------
    //
    // `virtio-blk` calls this with a *partition's* block count after resolving
    // the minor itself, so these use an extent that is not the whole device.

    /// A partition-sized extent, deliberately not [`BLOCKS`].
    const EXTENT: u64 = 40;

    #[test]
    fn the_range_check_does_not_look_at_the_minor() {
        // The minor is the caller's to resolve: by the time this runs it has
        // already become an extent, so no value of it is refused here — where
        // `validate_read` answers `ENXIO` for every one of these but the root.
        for minor in [BDEV_MINOR_ROOT, 1, 2, 3, 7, -1, i32::MIN, i32::MAX] {
            let req = parse_read(&request(minor, GOOD_GID, 32, 5));
            assert_eq!(
                validate_range(req, EXTENT),
                Ok((5 * BDEV_BLOCK_SIZE as u64, 32)),
                "minor {minor}"
            );
        }
    }

    #[test]
    fn the_range_check_accepts_the_extents_last_block_and_refuses_one_past() {
        let whole = BDEV_BLOCK_SIZE as i32;
        let last = parse_read(&request(3, GOOD_GID, whole, EXTENT - 1));
        assert_eq!(
            validate_range(last, EXTENT),
            Ok(((EXTENT - 1) * BDEV_BLOCK_SIZE as u64, BDEV_BLOCK_SIZE))
        );
        let past = parse_read(&request(3, GOOD_GID, whole, EXTENT));
        assert_eq!(validate_range(past, EXTENT), Err(EINVAL));
        // The offset is relative to the extent, not the disk: the same block
        // number is in range for a larger extent and out of range for this one.
        assert_eq!(
            validate_range(past, EXTENT + 1),
            Ok((EXTENT * BDEV_BLOCK_SIZE as u64, BDEV_BLOCK_SIZE))
        );
    }

    #[test]
    fn the_range_check_refuses_everything_on_an_empty_extent_but_a_zero_length() {
        let req = parse_read(&request(2, GOOD_GID, 32, 0));
        assert_eq!(validate_range(req, 0), Err(EINVAL));
        let empty = parse_read(&request(2, GOOD_GID, 0, 0));
        assert_eq!(validate_range(empty, 0), Ok((0, 0)));
    }

    #[test]
    fn the_range_check_keeps_the_length_and_grant_refusals_in_order() {
        // Checks 2-5, each alone, on a minor `validate_read` would have stopped.
        for len in [-1i32, i32::MIN, BDEV_MAX_IO as i32 + 1, i32::MAX] {
            let req = parse_read(&request(2, GOOD_GID, len, 0));
            assert_eq!(validate_range(req, EXTENT), Err(EINVAL), "len {len}");
        }
        for gid in [GRANT_INVALID, -2, i32::MIN] {
            let req = parse_read(&request(2, gid, 32, 0));
            assert_eq!(validate_range(req, EXTENT), Err(EINVAL), "gid {gid}");
        }
        // A bad grant is reported on a zero-length request, not masked by the
        // `Ok((0, 0))` fast path...
        let req = parse_read(&request(2, GRANT_INVALID, 0, 0));
        assert_eq!(validate_range(req, EXTENT), Err(EINVAL));
        // ...and a zero-length request with a good grant is `Ok` even at a block
        // far outside the extent, because nothing is copied.
        let req = parse_read(&request(2, GOOD_GID, 0, u64::MAX));
        assert_eq!(validate_range(req, EXTENT), Ok((0, 0)));
    }

    #[test]
    fn the_range_check_refuses_a_block_whose_byte_offset_overflows() {
        let wrapping = u64::MAX / BDEV_BLOCK_SIZE as u64 + 1;
        assert_eq!(wrapping.wrapping_mul(BDEV_BLOCK_SIZE as u64), 0);
        let req = parse_read(&request(1, GOOD_GID, 32, wrapping));
        // Past a real extent it is the cheap block check that refuses it...
        assert_eq!(validate_range(req, EXTENT), Err(EINVAL));
        // ...and with an extent large enough to contain the block, the multiply.
        assert_eq!(validate_range(req, u64::MAX), Err(EINVAL));
        // The last block whose offset does *not* wrap is still refused when the
        // extent's own byte length cannot be represented.
        let req = parse_read(&request(1, GOOD_GID, 32, wrapping - 1));
        assert_eq!(validate_range(req, u64::MAX), Err(EINVAL));
    }

    #[test]
    fn the_range_check_refuses_an_end_that_overflows() {
        // The largest extent whose byte length fits a `u64`, and its last block:
        // the offset is representable and a whole block ends exactly at the
        // extent's end, so this is the top of what can be accepted at all.
        let blocks = u64::MAX / BDEV_BLOCK_SIZE as u64;
        let req = parse_read(&request(1, GOOD_GID, BDEV_BLOCK_SIZE as i32, blocks - 1));
        assert_eq!(
            validate_range(req, blocks),
            Ok(((blocks - 1) * BDEV_BLOCK_SIZE as u64, BDEV_BLOCK_SIZE))
        );
        // One block more is out of range, and with a larger extent it is the
        // extent's byte length that overflows — refused either way.
        let req = parse_read(&request(1, GOOD_GID, BDEV_BLOCK_SIZE as i32, blocks));
        assert_eq!(validate_range(req, blocks), Err(EINVAL));
        assert_eq!(validate_range(req, blocks + 1), Err(EINVAL));
    }

    #[test]
    fn validate_read_is_the_minor_check_then_the_range_check() {
        // On the root minor the two agree on every outcome...
        let cases = [
            request(BDEV_MINOR_ROOT, GOOD_GID, 32, 0),
            request(BDEV_MINOR_ROOT, GOOD_GID, 0, 0),
            request(BDEV_MINOR_ROOT, GOOD_GID, BDEV_MAX_IO as i32, BLOCKS - 1),
            request(BDEV_MINOR_ROOT, GOOD_GID, 32, BLOCKS),
            request(BDEV_MINOR_ROOT, GOOD_GID, -1, 0),
            request(BDEV_MINOR_ROOT, GOOD_GID, BDEV_MAX_IO as i32 + 1, 0),
            request(BDEV_MINOR_ROOT, GRANT_INVALID, 32, 0),
            request(BDEV_MINOR_ROOT, GOOD_GID, 32, u64::MAX),
        ];
        for m in &cases {
            let req = parse_read(m);
            assert_eq!(
                validate_read(req, BLOCKS),
                validate_range(req, BLOCKS),
                "{req:?}"
            );
        }
        // ...and off it `validate_read` stops at `ENXIO` where the range check
        // alone would have gone on to accept the request.
        let req = parse_read(&request(1, GOOD_GID, 32, 0));
        assert_eq!(validate_read(req, BLOCKS), Err(ENXIO));
        assert_eq!(validate_range(req, BLOCKS), Ok((0, 32)));
    }

    #[test]
    fn a_write_validates_exactly_like_a_read() {
        // The payload and every geometry check are shared (W1's sibling on the BDEV
        // band). The one field that differs is the grant's access bit, which the
        // kernel checks in `verify_grant`, not this driver.
        let m = request(BDEV_MINOR_ROOT, 5, BDEV_BLOCK_SIZE as i32, 3);
        let req = parse_read(&m);
        assert_eq!(
            validate_read(req, 256),
            Ok((3 * BDEV_BLOCK_SIZE as u64, BDEV_BLOCK_SIZE))
        );
    }

    #[test]
    fn an_over_long_write_is_refused_not_clamped() {
        // Same rule as the read, and for the same reason: a client that cannot
        // interpret a fraction of a block gains nothing from a short transfer, and
        // EIO means the device failed, which is not what happened here.
        let m = request(BDEV_MINOR_ROOT, 5, BDEV_BLOCK_SIZE as i32 + 1, 0);
        assert_eq!(validate_read(parse_read(&m), 256), Err(EINVAL));
    }

    #[test]
    fn a_write_past_the_device_is_einval() {
        let m = request(BDEV_MINOR_ROOT, 5, BDEV_BLOCK_SIZE as i32, 256);
        assert_eq!(validate_read(parse_read(&m), 256), Err(EINVAL));
    }
}
