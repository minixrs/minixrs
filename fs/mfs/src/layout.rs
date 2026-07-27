// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Where everything sits on the device — the arithmetic `mkfs-mfs` and every
//! reader must agree on or nothing works.
//!
//! A MinixFS v3 device is laid out as:
//!
//! ```text
//!   block 0        boot block (bytes 0..1024) + superblock (bytes 1024..1055)
//!   block 1        reserved
//!   block 2..      inode bitmap, zone bitmap, inode table, then data zones
//! ```
//!
//! There is no I/O here and no floating-point: it is pure integer arithmetic over
//! `(ninodes, zones, block_size)`, which is exactly why it carries the crate's
//! densest unit tests. A one-block error anywhere in this file makes every inode
//! read return garbage from the middle of a bitmap.
//!
//! ## The two places minix.rs makes its own choice
//!
//! **[`START_BLOCK`] is 2.** MinixFS puts the superblock at byte 1024, so with
//! 1 KiB blocks the boot block and the superblock really are blocks 0 and 1 and
//! the bitmaps start at 2. With 4 KiB blocks both live inside block 0 and block 1
//! is spare — minix.rs keeps the bitmaps at 2 anyway rather than reclaiming it,
//! because `START_BLOCK = 2` is what MINIX's own `const.h` says and one spare
//! block out of 256 is not worth a divergence a reader could get wrong.
//!
//! **The zone bitmap is sized over *all* zones, not just the data zones.** The
//! exact size depends on `first_data_zone`, which depends on the bitmap size — a
//! circular definition MINIX's `mkfs.mfs` breaks by over-estimating in exactly
//! this way. Over-estimating keeps [`layout`] a pure non-iterative function, and
//! costs nothing: at 4 KiB blocks one bitmap block covers 32768 zones, which is
//! 128× the whole slice-5.7 image.

use crate::inode::INODE_SIZE;

/// First block of the filesystem proper — the inode bitmap. See the module docs
/// for why it is 2 rather than 1 at a 4 KiB block size.
pub const START_BLOCK: u32 = 2;

/// Blocks needed to hold `bits` bitmap bits at `block_size` bytes per block.
///
/// `0` bits needs `0` blocks; every real caller passes at least 1, because bit 0
/// of both bitmaps is reserved (neither inode 0 nor zone 0 exists).
pub fn bitmap_blocks(bits: usize, block_size: usize) -> u32 {
    let per_block = block_size * 8;
    bits.div_ceil(per_block) as u32
}

/// Where each region of the device starts, and how long it is. All values are
/// block numbers / block counts.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Layout {
    /// First block of the inode bitmap.
    pub imap_start: u32,
    /// Blocks of inode bitmap.
    pub imap_blocks: u32,
    /// First block of the zone bitmap.
    pub zmap_start: u32,
    /// Blocks of zone bitmap.
    pub zmap_blocks: u32,
    /// First block of the inode table.
    pub inode_start: u32,
    /// Blocks of inode table.
    pub inode_blocks: u32,
    /// First zone available for file data. With `log_zone_size == 0` a zone *is*
    /// a block, so this is also the first data block.
    pub first_data_zone: u32,
}

/// Compute the layout of a device with `ninodes` inodes and `zones` total zones.
///
/// `block_size` must be non-zero and a multiple of [`INODE_SIZE`]; every caller in
/// this workspace passes [`crate::MFS_BLOCK_SIZE`].
pub fn layout(ninodes: u32, zones: u32, block_size: usize) -> Layout {
    let inodes_per_block = block_size / INODE_SIZE;

    // Bit 0 of each bitmap is reserved (there is no inode 0 and no zone 0), hence
    // the `1 +`. The zone bitmap is deliberately sized over every zone — see the
    // module docs.
    let imap_blocks = bitmap_blocks(1 + ninodes as usize, block_size);
    let zmap_blocks = bitmap_blocks(1 + zones as usize, block_size);
    let inode_blocks = (ninodes as usize).div_ceil(inodes_per_block) as u32;

    let imap_start = START_BLOCK;
    let zmap_start = imap_start + imap_blocks;
    let inode_start = zmap_start + zmap_blocks;
    let first_data_zone = inode_start + inode_blocks;

    Layout {
        imap_start,
        imap_blocks,
        zmap_start,
        zmap_blocks,
        inode_start,
        inode_blocks,
        first_data_zone,
    }
}

/// Bit index in the **inode** bitmap for inode number `ino`.
///
/// The identity map: bit 0 is reserved because inode 0 does not exist, so inode
/// `n` is bit `n`.
pub fn imap_bit(ino: u32) -> u32 {
    ino
}

/// Bit index in the **zone** bitmap for zone number `zone`.
///
/// MINIX's convention (`alloc_zone`/`free_zone` in `servers/mfs/alloc.c`): the
/// bitmap is based at `first_data_zone - 1`, so bit 0 corresponds to a zone that
/// is *not* a data zone and is always marked in use. `None` for a zone below that
/// base — such a zone is metadata and has no bitmap bit at all.
pub fn zmap_bit(zone: u32, first_data_zone: u32) -> Option<u32> {
    zone.checked_sub(first_data_zone - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BS: usize = crate::MFS_BLOCK_SIZE;

    #[test]
    fn bitmap_blocks_rounds_up_at_every_boundary() {
        let per_block = BS * 8; // 32768 bits in a 4 KiB block
        assert_eq!(bitmap_blocks(0, BS), 0);
        assert_eq!(bitmap_blocks(1, BS), 1);
        assert_eq!(bitmap_blocks(per_block - 1, BS), 1);
        assert_eq!(bitmap_blocks(per_block, BS), 1);
        assert_eq!(bitmap_blocks(per_block + 1, BS), 2);
        assert_eq!(bitmap_blocks(2 * per_block, BS), 2);
        assert_eq!(bitmap_blocks(2 * per_block + 1, BS), 3);
    }

    #[test]
    fn bitmap_blocks_scales_with_the_block_size() {
        // A smaller block holds fewer bits, so the same bit count needs more
        // blocks. (1 KiB is not a size minix.rs formats, but the arithmetic must
        // not have 4096 baked in.)
        assert_eq!(bitmap_blocks(8192, 1024), 1);
        assert_eq!(bitmap_blocks(8193, 1024), 2);
        assert_eq!(bitmap_blocks(8193, 4096), 1);
    }

    /// The geometry of the slice-5.7 root image, computed by hand:
    ///
    /// * imap: `1 + 64 = 65` bits → 1 block, at 2
    /// * zmap: `1 + 256 = 257` bits → 1 block, at 3
    /// * inodes: `64 / (4096/64) = 1` block, at 4
    /// * first data zone: 5
    #[test]
    fn the_rootfs_geometry_matches_the_hand_computation() {
        assert_eq!(
            layout(64, 256, BS),
            Layout {
                imap_start: 2,
                imap_blocks: 1,
                zmap_start: 3,
                zmap_blocks: 1,
                inode_start: 4,
                inode_blocks: 1,
                first_data_zone: 5,
            }
        );
    }

    /// A second, larger geometry, also by hand: 4096 inodes needs
    /// `4096 / 64 = 64` inode-table blocks, and both bitmaps still fit in one.
    #[test]
    fn a_larger_geometry_matches_the_hand_computation() {
        assert_eq!(
            layout(4096, 65_536, BS),
            Layout {
                imap_start: 2,
                imap_blocks: 1,
                zmap_start: 3,
                zmap_blocks: 3, // 65537 bits / 32768 = 3 blocks
                inode_start: 6,
                inode_blocks: 64,
                first_data_zone: 70,
            }
        );
    }

    #[test]
    fn every_region_is_contiguous_and_ascending() {
        // Whatever the geometry, the four regions tile the device head to tail
        // with no gap: a gap would waste blocks, an overlap would corrupt.
        for (ninodes, zones) in [(1u32, 1u32), (64, 256), (1000, 5000), (4096, 65_536)] {
            let l = layout(ninodes, zones, BS);
            assert_eq!(l.imap_start, START_BLOCK, "{ninodes}/{zones}");
            assert_eq!(l.zmap_start, l.imap_start + l.imap_blocks);
            assert_eq!(l.inode_start, l.zmap_start + l.zmap_blocks);
            assert_eq!(l.first_data_zone, l.inode_start + l.inode_blocks);
        }
    }

    #[test]
    fn the_inode_table_is_sized_by_inodes_per_block() {
        let ipb = (BS / INODE_SIZE) as u32;
        assert_eq!(ipb, 64);
        assert_eq!(layout(1, 256, BS).inode_blocks, 1);
        assert_eq!(layout(ipb, 256, BS).inode_blocks, 1);
        assert_eq!(layout(ipb + 1, 256, BS).inode_blocks, 2);
        assert_eq!(layout(2 * ipb, 256, BS).inode_blocks, 2);
    }

    #[test]
    fn the_zone_bitmap_is_sized_over_every_zone_not_just_data_zones() {
        // The over-estimate the module docs describe: sizing over `1 + zones`
        // rather than `zones - first_data_zone + 1` is what keeps this function
        // non-iterative. It can only ever be too big, never too small.
        let l = layout(64, 256, BS);
        let data_zone_bits = (256 - l.first_data_zone + 1) as usize;
        assert_eq!(
            l.zmap_blocks.min(bitmap_blocks(data_zone_bits, BS)),
            bitmap_blocks(data_zone_bits, BS),
            "the zone bitmap must never be smaller than the data zones need"
        );
    }

    #[test]
    fn bit_zero_of_each_bitmap_is_reserved() {
        // Inode 0 and the zone just below the first data zone both map to bit 0,
        // which mkfs marks in use. That reservation is what lets 0 double as
        // "no such inode" / "hole" everywhere else in the format.
        assert_eq!(imap_bit(0), 0);
        assert_eq!(imap_bit(crate::inode::ROOT_INODE), 1);

        let fdz = layout(64, 256, BS).first_data_zone;
        assert_eq!(zmap_bit(fdz - 1, fdz), Some(0));
        assert_eq!(zmap_bit(fdz, fdz), Some(1));
        assert_eq!(zmap_bit(fdz + 10, fdz), Some(11));
    }

    #[test]
    fn a_metadata_zone_has_no_bitmap_bit() {
        // Anything below `first_data_zone - 1` is bitmap or inode table; asking
        // for its bit is a caller bug, and answering with a wrapped index would
        // silently mark an unrelated data zone.
        let fdz = layout(64, 256, BS).first_data_zone; // 5
        assert_eq!(zmap_bit(0, fdz), None);
        assert_eq!(zmap_bit(3, fdz), None);
        assert_eq!(zmap_bit(4, fdz), Some(0));
    }
}
