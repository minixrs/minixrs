// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The pure half of virtio-blk's request path (slice 6.3, spec B4, B6, B7): block
//! to sector arithmetic, the request header's bytes, what a status byte means, the
//! shape of a write, and the root-check decisions. No IPC and no device access —
//! `main.rs` does the queue, the interrupt and the kernel calls around it — so
//! everything here is host-tested.
//!
//! All arithmetic is `checked_*`: drivers ship `--release` with
//! `overflow-checks = false`, where `block * 8` on a large block would wrap and
//! name a valid sector.

// Nothing in `main.rs` calls this yet: the driver starts serving requests in the
// next commit of slice 6.3 (Task 5), which narrows this to the test config.
#![allow(dead_code)]

use minixrs_kernel_shared::callnr::BDEV_BLOCK_SIZE;
use minixrs_kernel_shared::error::EIO;
use minixrs_kernel_shared::rootfs::{
    HDR_BLOCKS_OFF, IMAGE_HDR_LEN, IMAGE_LABEL, IMAGE_LABEL_LEN, IMAGE_TAIL_LABEL,
};

/// Bytes in the device's unit, the sector (virtio-blk's `capacity` and a
/// request's `sector` are both in these, whatever block size the device reports).
pub const SECTOR_SIZE: usize = 512;

/// Sectors in one BDEV block: every device transfer is this many (spec B4).
pub const SECTORS_PER_BLOCK: u64 = (BDEV_BLOCK_SIZE / SECTOR_SIZE) as u64;

// A block that is not a whole number of sectors could not be transferred at all.
const _: () = assert!(BDEV_BLOCK_SIZE.is_multiple_of(SECTOR_SIZE));
const _: () = assert!(SECTORS_PER_BLOCK > 0);

/// Request type: read (the device writes the data buffer).
pub const VIRTIO_BLK_T_IN: u32 = 0;
/// Request type: write (the device reads the data buffer).
pub const VIRTIO_BLK_T_OUT: u32 = 1;

/// Status byte: the request succeeded.
pub const VIRTIO_BLK_S_OK: u8 = 0;
/// Status byte: device or media error.
pub const VIRTIO_BLK_S_IOERR: u8 = 1;
/// Status byte: the device does not support the request.
pub const VIRTIO_BLK_S_UNSUPP: u8 = 2;
/// What the driver presets the status byte to before submitting. No status the
/// specification defines, so a byte still holding it after completion was never
/// written by the device (spec B7).
pub const STATUS_UNWRITTEN: u8 = 0xFF;

/// Bytes in a request header: `type: u32`, `reserved: u32`, `sector: u64`.
pub const REQ_HEADER_LEN: usize = 16;

/// The device's size in BDEV blocks, from its `capacity` in sectors. `None` for an
/// empty device or one that is not a whole number of blocks: its last sectors
/// would be unreachable through BDEV, and the root image is never such a size.
pub const fn device_blocks(capacity_sectors: u64) -> Option<u64> {
    if capacity_sectors == 0 || !capacity_sectors.is_multiple_of(SECTORS_PER_BLOCK) {
        return None;
    }
    capacity_sectors.checked_div(SECTORS_PER_BLOCK)
}

/// First sector of BDEV block `block`; `None` on overflow.
pub const fn block_to_sector(block: u64) -> Option<u64> {
    block.checked_mul(SECTORS_PER_BLOCK)
}

/// A request header: little-endian `type`, a zero `reserved` word, `sector`.
pub const fn encode_header(req_type: u32, sector: u64) -> [u8; REQ_HEADER_LEN] {
    let t = req_type.to_le_bytes();
    let s = sector.to_le_bytes();
    [
        t[0], t[1], t[2], t[3], // type
        0, 0, 0, 0, // reserved
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7], // sector
    ]
}

/// What a completed request's status byte means. Only [`VIRTIO_BLK_S_OK`] is
/// success; `IOERR`, `UNSUPP`, [`STATUS_UNWRITTEN`] (the device completed the
/// request without writing the byte) and every other value are `Err(EIO)`.
pub const fn status_errno(status: u8) -> Result<(), i32> {
    match status {
        VIRTIO_BLK_S_OK => Ok(()),
        _ => Err(EIO),
    }
}

/// How a `BDEV_WRITE` of some length reaches the device (spec B4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WritePlan {
    /// Zero bytes: answered `0` with no grant use and no device I/O.
    Empty,
    /// A whole block: copy the client's bytes into the bounce page, write it.
    Whole,
    /// Part of a block: read the block into the bounce page, copy the client's
    /// bytes over its start, write it back — the rest of the block survives, as
    /// it does on `drivers/memory`.
    Partial,
}

/// The plan for a write of `len` bytes. `len` has already been validated to be at
/// most `BDEV_MAX_IO` (one block) by `server-rt`'s `bdev::validate_read` before
/// this is called, so everything that is neither empty nor whole is a partial
/// block.
pub const fn write_plan(len: usize) -> WritePlan {
    if len == 0 {
        WritePlan::Empty
    } else if len == BDEV_BLOCK_SIZE {
        WritePlan::Whole
    } else {
        WritePlan::Partial
    }
}

/// Why a device is not a minix.rs root (spec B6): `drivers/memory`'s `selfcheck`
/// refusals, in the same order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RootFail {
    /// Block 0 does not start with `IMAGE_LABEL`.
    Label,
    /// The header's block count is not the device's.
    Blocks {
        /// The count the image header carries.
        hdr: u32,
        /// The device's size in blocks.
        dev: u64,
    },
    /// The last block does not start with `IMAGE_TAIL_LABEL`.
    TailLabel,
}

/// Check the first `IMAGE_HDR_LEN` bytes of block 0: the label, then that the
/// header's little-endian block count equals the device's — mkfs's geometry bound
/// to the device's, two independently derived numbers.
///
/// Device-level only: no superblock is decoded and `minixrs-mfs` is not a
/// dependency, because a block driver must not depend on the filesystem format.
pub fn check_head(hdr: &[u8; IMAGE_HDR_LEN], device_blocks: u64) -> Result<(), RootFail> {
    if hdr[..IMAGE_LABEL_LEN] != IMAGE_LABEL {
        return Err(RootFail::Label);
    }
    let hdr_blocks = hdr_u32(hdr, HDR_BLOCKS_OFF);
    if u64::from(hdr_blocks) != device_blocks {
        return Err(RootFail::Blocks {
            hdr: hdr_blocks,
            dev: device_blocks,
        });
    }
    Ok(())
}

/// Check the first `IMAGE_HDR_LEN` bytes of the last block: the tail label, which
/// deliberately differs from the head's, so a device that answers every read with
/// block 0 fails here.
pub fn check_tail(hdr: &[u8; IMAGE_HDR_LEN]) -> Result<(), RootFail> {
    if hdr[..IMAGE_LABEL_LEN] != IMAGE_TAIL_LABEL {
        return Err(RootFail::TailLabel);
    }
    Ok(())
}

/// A little-endian `u32` at `off` in the image header (an on-disk image written by
/// a build host, so explicitly little-endian); `0` if `off` does not leave room
/// for one, which no caller's constant offset does.
fn hdr_u32(hdr: &[u8; IMAGE_HDR_LEN], off: usize) -> u32 {
    let bytes = off
        .checked_add(4)
        .and_then(|end| hdr.get(off..end))
        .and_then(|b| <[u8; 4]>::try_from(b).ok());
    match bytes {
        Some(b) => u32::from_le_bytes(b),
        None => 0,
    }
}

/// The last block of a device of `device_blocks` blocks; `None` for an empty one.
pub const fn tail_block(device_blocks: u64) -> Option<u64> {
    device_blocks.checked_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::callnr::BDEV_MAX_IO;
    use minixrs_kernel_shared::rootfs::{ROOTFS_IMAGE_BLOCKS, ROOTFS_IMAGE_BYTES};

    /// The root image's size in sectors: what the disk boot's device reports.
    const ROOT_SECTORS: u64 = (ROOTFS_IMAGE_BYTES / SECTOR_SIZE) as u64;

    /// A well-formed block-0 header for a device of `blocks` blocks.
    fn head(blocks: u32) -> [u8; IMAGE_HDR_LEN] {
        let mut h = [0u8; IMAGE_HDR_LEN];
        h[..IMAGE_LABEL_LEN].copy_from_slice(&IMAGE_LABEL);
        h[HDR_BLOCKS_OFF..HDR_BLOCKS_OFF + 4].copy_from_slice(&blocks.to_le_bytes());
        h
    }

    /// A well-formed last-block header.
    fn tail() -> [u8; IMAGE_HDR_LEN] {
        let mut h = [0u8; IMAGE_HDR_LEN];
        h[..IMAGE_LABEL_LEN].copy_from_slice(&IMAGE_TAIL_LABEL);
        h
    }

    #[test]
    fn a_block_is_eight_sectors() {
        assert_eq!(SECTORS_PER_BLOCK, 8);
        assert_eq!(SECTORS_PER_BLOCK * SECTOR_SIZE as u64, 4096);
    }

    #[test]
    fn device_blocks_divides_a_whole_capacity() {
        assert_eq!(device_blocks(2048), Some(256));
        assert_eq!(
            device_blocks(ROOT_SECTORS),
            Some(u64::from(ROOTFS_IMAGE_BLOCKS))
        );
        assert_eq!(device_blocks(8), Some(1));
        assert_eq!(device_blocks(16), Some(2));
        // The largest whole capacity there is.
        assert_eq!(device_blocks(u64::MAX - 7), Some(u64::MAX / 8));
    }

    #[test]
    fn device_blocks_refuses_an_empty_device() {
        assert_eq!(device_blocks(0), None);
    }

    #[test]
    fn device_blocks_refuses_a_ragged_capacity() {
        // One sector short of, and one past, the root image.
        assert_eq!(device_blocks(2047), None);
        assert_eq!(device_blocks(2049), None);
        // Less than one block.
        for sectors in 1..8 {
            assert_eq!(device_blocks(sectors), None, "{sectors}");
        }
        // 6.2's blank disk was 2560 sectors: whole. One more is not.
        assert_eq!(device_blocks(2560), Some(320));
        assert_eq!(device_blocks(2561), None);
        assert_eq!(device_blocks(u64::MAX), None);
    }

    #[test]
    fn block_to_sector_multiplies_by_eight() {
        assert_eq!(block_to_sector(0), Some(0));
        assert_eq!(block_to_sector(1), Some(8));
        assert_eq!(block_to_sector(255), Some(2040));
        // Not the identity: sector = block is mutation 2.
        assert_ne!(block_to_sector(255), Some(255));
    }

    #[test]
    fn block_to_sector_is_checked() {
        assert_eq!(block_to_sector(u64::MAX), None);
        // The first block whose sector does not fit, and the last that does.
        assert_eq!(block_to_sector(u64::MAX / 8 + 1), None);
        assert_eq!(block_to_sector(u64::MAX / 8), Some(u64::MAX - 7));
        // A power of two that a wrapping multiply would turn into sector 0.
        assert_eq!(block_to_sector(1 << 61), None);
    }

    #[test]
    fn the_last_blocks_last_sector_is_the_last_sector() {
        for capacity in [8u64, 2048, 2560, u64::MAX - 7] {
            let blocks = device_blocks(capacity).expect("a whole capacity");
            let last = tail_block(blocks).expect("a non-empty device");
            let first_sector = block_to_sector(last).expect("in range");
            assert_eq!(
                first_sector.checked_add(SECTORS_PER_BLOCK - 1),
                Some(capacity - 1),
                "{capacity}"
            );
        }
    }

    #[test]
    fn tail_block_is_the_last_one() {
        assert_eq!(tail_block(256), Some(255));
        assert_eq!(tail_block(1), Some(0));
        assert_eq!(tail_block(u64::MAX), Some(u64::MAX - 1));
    }

    #[test]
    fn tail_block_of_an_empty_device_is_none() {
        assert_eq!(tail_block(0), None);
    }

    #[test]
    fn header_bytes_are_pinned() {
        // type = 1 (OUT), reserved = 0, sector = 0x0807_0605_0403_0201.
        assert_eq!(
            encode_header(VIRTIO_BLK_T_OUT, 0x0807_0605_0403_0201),
            [
                0x01, 0x00, 0x00, 0x00, // type, little-endian
                0x00, 0x00, 0x00, 0x00, // reserved
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, // sector, little-endian
            ]
        );
        // A read of sector 2040 (block 255 of the root image).
        assert_eq!(
            encode_header(VIRTIO_BLK_T_IN, 2040),
            [0, 0, 0, 0, 0, 0, 0, 0, 0xF8, 0x07, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn header_type_uses_all_four_bytes_and_leaves_reserved_zero() {
        let h = encode_header(0xAABB_CCDD, u64::MAX);
        assert_eq!(h[0..4], [0xDD, 0xCC, 0xBB, 0xAA]);
        assert_eq!(h[4..8], [0; 4]);
        assert_eq!(h[8..16], [0xFF; 8]);
        assert_eq!(h.len(), REQ_HEADER_LEN);
    }

    #[test]
    fn request_types_and_statuses_are_the_specifications() {
        assert_eq!((VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT), (0, 1));
        assert_eq!(
            (VIRTIO_BLK_S_OK, VIRTIO_BLK_S_IOERR, VIRTIO_BLK_S_UNSUPP),
            (0, 1, 2)
        );
        assert_eq!(STATUS_UNWRITTEN, 0xFF);
    }

    #[test]
    fn only_status_ok_is_success() {
        assert_eq!(status_errno(VIRTIO_BLK_S_OK), Ok(()));
        for status in 1..=u8::MAX {
            assert_eq!(status_errno(status), Err(EIO), "{status:#x}");
        }
    }

    #[test]
    fn device_errors_are_eio() {
        assert_eq!(status_errno(VIRTIO_BLK_S_IOERR), Err(EIO));
        assert_eq!(status_errno(VIRTIO_BLK_S_UNSUPP), Err(EIO));
    }

    #[test]
    fn a_status_the_device_never_wrote_is_eio() {
        assert_eq!(status_errno(STATUS_UNWRITTEN), Err(EIO));
        // The sentinel must not be a status the device can legitimately report.
        for defined in [VIRTIO_BLK_S_OK, VIRTIO_BLK_S_IOERR, VIRTIO_BLK_S_UNSUPP] {
            assert_ne!(STATUS_UNWRITTEN, defined);
        }
    }

    #[test]
    fn write_plan_has_three_shapes() {
        assert_eq!(write_plan(0), WritePlan::Empty);
        assert_eq!(write_plan(BDEV_BLOCK_SIZE), WritePlan::Whole);
        assert_eq!(write_plan(BDEV_MAX_IO), WritePlan::Whole);
        // Everything in between reads the block first, so its tail survives.
        assert_eq!(write_plan(1), WritePlan::Partial);
        assert_eq!(write_plan(SECTOR_SIZE), WritePlan::Partial);
        assert_eq!(write_plan(BDEV_BLOCK_SIZE - 1), WritePlan::Partial);
        for len in 1..BDEV_BLOCK_SIZE {
            assert_eq!(write_plan(len), WritePlan::Partial, "{len}");
        }
    }

    #[test]
    fn head_check_accepts_the_image_this_build_packs() {
        assert_eq!(
            check_head(&head(ROOTFS_IMAGE_BLOCKS), u64::from(ROOTFS_IMAGE_BLOCKS)),
            Ok(())
        );
        // Bytes after the block count are not this check's business.
        let mut h = head(256);
        h[HDR_BLOCKS_OFF + 4..].fill(0xA5);
        assert_eq!(check_head(&h, 256), Ok(()));
    }

    #[test]
    fn head_check_refuses_a_wrong_label() {
        // A blank disk.
        assert_eq!(check_head(&[0u8; IMAGE_HDR_LEN], 256), Err(RootFail::Label));
        // One byte off, first and last of the label.
        for i in [0, IMAGE_LABEL_LEN - 1] {
            let mut h = head(256);
            h[i] ^= 1;
            assert_eq!(check_head(&h, 256), Err(RootFail::Label), "byte {i}");
        }
    }

    #[test]
    fn head_check_reports_the_label_before_the_block_count() {
        // Both wrong: the label is what is reported, as `selfcheck` does.
        let mut h = head(7);
        h[0] ^= 1;
        assert_eq!(check_head(&h, 256), Err(RootFail::Label));
    }

    #[test]
    fn head_check_refuses_a_block_count_that_is_not_the_devices() {
        assert_eq!(
            check_head(&head(256), 320),
            Err(RootFail::Blocks { hdr: 256, dev: 320 })
        );
        assert_eq!(
            check_head(&head(320), 256),
            Err(RootFail::Blocks { hdr: 320, dev: 256 })
        );
        // A device larger than any header can say never matches by truncation.
        assert_eq!(
            check_head(&head(256), (1 << 32) + 256),
            Err(RootFail::Blocks {
                hdr: 256,
                dev: (1 << 32) + 256
            })
        );
    }

    #[test]
    fn head_block_count_is_little_endian_at_its_offset() {
        let mut h = head(0);
        h[HDR_BLOCKS_OFF..HDR_BLOCKS_OFF + 4].copy_from_slice(&[0x04, 0x03, 0x02, 0x01]);
        assert_eq!(check_head(&h, 0x0102_0304), Ok(()));
        assert_eq!(
            check_head(&h, 0x0403_0201),
            Err(RootFail::Blocks {
                hdr: 0x0102_0304,
                dev: 0x0403_0201
            })
        );
    }

    #[test]
    fn hdr_u32_is_total() {
        let h = head(0x0102_0304);
        assert_eq!(hdr_u32(&h, HDR_BLOCKS_OFF), 0x0102_0304);
        assert_eq!(hdr_u32(&h, IMAGE_HDR_LEN - 4), 0);
        // No room for four bytes, and an offset whose end overflows.
        assert_eq!(hdr_u32(&head(u32::MAX), IMAGE_HDR_LEN - 3), 0);
        assert_eq!(hdr_u32(&head(u32::MAX), usize::MAX), 0);
    }

    #[test]
    fn tail_check_accepts_the_tail_label() {
        assert_eq!(check_tail(&tail()), Ok(()));
        let mut t = tail();
        t[IMAGE_LABEL_LEN..].fill(0xA5);
        assert_eq!(check_tail(&t), Ok(()));
    }

    #[test]
    fn tail_check_refuses_anything_else() {
        assert_eq!(check_tail(&[0u8; IMAGE_HDR_LEN]), Err(RootFail::TailLabel));
        for i in [0, IMAGE_LABEL_LEN - 1] {
            let mut t = tail();
            t[i] ^= 1;
            assert_eq!(check_tail(&t), Err(RootFail::TailLabel), "byte {i}");
        }
    }

    #[test]
    fn head_and_tail_labels_are_not_interchangeable() {
        // A device answering every read with block 0 passes the head check and
        // must fail the tail check — the whole reason there are two labels.
        assert_eq!(check_tail(&head(256)), Err(RootFail::TailLabel));
        // And the tail block is not a head.
        assert_eq!(check_head(&tail(), 256), Err(RootFail::Label));
        assert_ne!(IMAGE_LABEL, IMAGE_TAIL_LABEL);
    }
}
