// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `BDEV_READ` request parsing and validation — the driver's pure logic.
//!
//! Everything here is a total function over a [`Message`] or a scalar, so it
//! carries the crate's unit tests. The IPC round-trip and the kernel calls live in
//! `main.rs` and are verified through the boot log. Same split as
//! `drivers/tty`'s `cdev.rs` / `main.rs`.
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
//! past the end is a caller bug. `EIO` stays reserved for Phase 6's real media
//! errors, where the request was well-formed and the device failed.

use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    BDEV_BLOCK_OFF, BDEV_BLOCK_SIZE, BDEV_GRANT_OFF, BDEV_LEN_OFF, BDEV_MAX_IO, BDEV_MINOR_OFF,
    BDEV_MINOR_RAMDISK,
};
use minixrs_kernel_shared::error::{EINVAL, ENXIO};
use minixrs_kernel_shared::grant::grant_valid;
use minixrs_kernel_shared::uspace::RAMDISK_WINDOW_SIZE;
use minixrs_server_rt::{rd_i32, rd_u64};

/// A parsed `BDEV_READ` (or `BDEV_WRITE`) request. Field-for-field the payload,
/// with no interpretation applied — [`validate_read`] does that.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct ReadRequest {
    /// Device minor. Only [`BDEV_MINOR_RAMDISK`] exists today.
    pub minor: i32,
    /// Grant id naming the client's destination buffer. Must carry `CPF_WRITE`
    /// and name this driver as grantee — both checked by the kernel, not here.
    pub gid: i32,
    /// Bytes the client asked for. Must not exceed [`BDEV_MAX_IO`].
    pub len: i32,
    /// Block number within the device.
    pub block: u64,
}

/// Read a `BDEV_READ` request out of a message payload.
///
/// Total: every field is a fixed-offset scalar read that cannot fail
/// (`server-rt`'s accessors return `0` for an out-of-range offset), so a malformed
/// request becomes an invalid *value* that [`validate_read`] rejects, never a panic.
pub fn parse_read(msg: &Message) -> ReadRequest {
    ReadRequest {
        minor: rd_i32(msg, BDEV_MINOR_OFF),
        gid: rd_i32(msg, BDEV_GRANT_OFF),
        len: rd_i32(msg, BDEV_LEN_OFF),
        block: rd_u64(msg, BDEV_BLOCK_OFF),
    }
}

/// Decide which bytes of the device `req` names, or which errno to reply.
///
/// `Ok((byte_offset, len))` means "copy `len` bytes starting `byte_offset` into
/// the device, and reply `len`" — including `Ok((0, 0))`, a legal zero-length read
/// that needs no copy at all. `Err(e)` is a negative errno for the reply.
///
/// The checks, in the order a driver should apply them — each is the first thing
/// that can be wrong given the ones before it:
///
/// 1. `minor != BDEV_MINOR_RAMDISK` → `ENXIO`. The *device* does not exist;
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
pub fn validate_read(req: ReadRequest, device_blocks: u64) -> Result<(u64, usize), i32> {
    if req.minor != BDEV_MINOR_RAMDISK {
        return Err(ENXIO);
    }
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
    if req.block >= device_blocks {
        return Err(EINVAL);
    }
    let byte_off = req
        .block
        .checked_mul(BDEV_BLOCK_SIZE as u64)
        .ok_or(EINVAL)?;
    let end = byte_off.checked_add(len as u64).ok_or(EINVAL)?;
    let device_bytes = device_blocks
        .checked_mul(BDEV_BLOCK_SIZE as u64)
        .ok_or(EINVAL)?;
    if end > device_bytes {
        return Err(EINVAL);
    }
    Ok((byte_off, len))
}

/// Check the length the kernel reported for the ramdisk, and convert it to a block
/// count.
///
/// Cheap, host-testable, and it bounds the damage from a bogus reply: every later
/// range check is against the block count this returns, so a length the driver
/// accepted uncritically would become the ceiling on what it hands to
/// `SYS_SAFECOPY`.
///
/// `EINVAL` for zero, for a length that is not a whole number of blocks (the
/// driver serves whole blocks and nothing else), or for one larger than the VA
/// window the kernel reserves — which no honest reply can exceed, because the
/// kernel maps the image into that window.
pub fn validate_geometry(len: u64) -> Result<u64, i32> {
    if len == 0 {
        return Err(EINVAL);
    }
    if !len.is_multiple_of(BDEV_BLOCK_SIZE as u64) {
        return Err(EINVAL);
    }
    if len > RAMDISK_WINDOW_SIZE {
        return Err(EINVAL);
    }
    Ok(len / BDEV_BLOCK_SIZE as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::grant::{GRANT_INVALID, grant_id};
    use minixrs_server_rt::{wr_i32, wr_u64};

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
        // A client that sent nothing at all: minor 0 happens to be the ramdisk,
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
            let req = parse_read(&request(BDEV_MINOR_RAMDISK, GOOD_GID, 32, block));
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
            BDEV_MINOR_RAMDISK,
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
            let req = parse_read(&request(BDEV_MINOR_RAMDISK, GOOD_GID, len, 0));
            assert_eq!(validate_read(req, BLOCKS), Err(EINVAL), "len {len}");
        }
    }

    #[test]
    fn a_zero_length_read_is_ok_not_an_error() {
        let req = parse_read(&request(BDEV_MINOR_RAMDISK, GOOD_GID, 0, 0));
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
    fn a_negative_length_is_einval() {
        // Left unchecked this would widen into a ~16 EiB `u64` byte count for the
        // kernel copy call.
        for len in [-1i32, -4096, i32::MIN] {
            let req = parse_read(&request(BDEV_MINOR_RAMDISK, GOOD_GID, len, 0));
            assert_eq!(validate_read(req, BLOCKS), Err(EINVAL), "len {len}");
        }
    }

    #[test]
    fn an_invalid_grant_id_is_einval() {
        for gid in [GRANT_INVALID, -2, i32::MIN] {
            let req = parse_read(&request(BDEV_MINOR_RAMDISK, gid, 32, 0));
            assert_eq!(validate_read(req, BLOCKS), Err(EINVAL), "gid {gid}");
        }
    }

    #[test]
    fn a_block_past_the_device_is_einval() {
        for block in [BLOCKS, BLOCKS + 1, u64::MAX] {
            let req = parse_read(&request(BDEV_MINOR_RAMDISK, GOOD_GID, 32, block));
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
        let req = parse_read(&request(BDEV_MINOR_RAMDISK, GOOD_GID, 32, wrapping));
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
        let req = parse_read(&request(BDEV_MINOR_RAMDISK, GRANT_INVALID, 0, 0));
        assert_eq!(validate_read(req, BLOCKS), Err(EINVAL));
    }

    #[test]
    fn a_plausible_geometry_converts_to_a_block_count() {
        assert_eq!(validate_geometry(1024 * 1024), Ok(256));
        assert_eq!(validate_geometry(BDEV_BLOCK_SIZE as u64), Ok(1));
        assert_eq!(
            validate_geometry(RAMDISK_WINDOW_SIZE),
            Ok(RAMDISK_WINDOW_SIZE / BDEV_BLOCK_SIZE as u64)
        );
    }

    #[test]
    fn an_implausible_geometry_is_refused() {
        assert_eq!(validate_geometry(0), Err(EINVAL), "an empty device");
        assert_eq!(validate_geometry(1), Err(EINVAL), "a partial block");
        assert_eq!(
            validate_geometry(BDEV_BLOCK_SIZE as u64 + 1),
            Err(EINVAL),
            "a trailing partial block"
        );
        assert_eq!(
            validate_geometry(RAMDISK_WINDOW_SIZE + BDEV_BLOCK_SIZE as u64),
            Err(EINVAL),
            "larger than the window the kernel maps it into"
        );
        assert_eq!(validate_geometry(u64::MAX), Err(EINVAL));
    }
}
