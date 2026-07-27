// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The MinixFS v3 superblock: its on-disk layout, codec, and validity check.
//!
//! Layout is MINIX 3's `struct super_block` (`servers/mfs/super.h`), first 31
//! bytes — everything past `s_disk_version` is in-core state MINIX never writes
//! to disk. The superblock lives at byte [`SUPER_OFFSET`] (1024) of the device,
//! *inside* block 0, which is why block 0's first kilobyte (the boot block) is
//! free for the image header `tools/mkfs-mfs` puts there.
//!
//! The encoder exists because the writer needs it: one shared layout, exercised
//! in both directions by `mkfs-mfs`'s round-trip tests, is what makes those tests
//! mean something. A separate hand-written encoder in the tool would only prove
//! the tool agrees with itself.

/// Byte offset of the superblock within the device. MinixFS puts it in the second
/// 1 KiB "sector", so with 4096-byte blocks it sits inside block 0.
pub const SUPER_OFFSET: usize = 1024;

/// Bytes of `struct super_block` that are actually stored on disk. The rest of
/// MINIX's struct is in-core bookkeeping.
pub const SUPER_ON_DISK_LEN: usize = 31;

/// MinixFS v3 magic (`SUPER_V3` in MINIX 3's `super.h`).
pub const SUPER_MAGIC_V3: i16 = 0x4D5A;

/// `s_flags`: the filesystem was unmounted cleanly (MINIX 3 `MFSFLAG_CLEAN`).
pub const MFSFLAG_CLEAN: u16 = 0x0001;

// Field offsets within the on-disk superblock, in declaration order.
/// `s_ninodes` — usable inodes (u32).
pub const SUPER_NINODES_OFF: usize = 0;
/// `s_nzones` — v1 zone count, 0 when it does not fit (u16).
pub const SUPER_NZONES_OFF: usize = 4;
/// `s_imap_blocks` — blocks of inode bitmap (i16).
pub const SUPER_IMAP_BLOCKS_OFF: usize = 6;
/// `s_zmap_blocks` — blocks of zone bitmap (i16).
pub const SUPER_ZMAP_BLOCKS_OFF: usize = 8;
/// `s_firstdatazone_old` — first data zone, 0 when it does not fit (u16).
pub const SUPER_FIRSTDATAZONE_OLD_OFF: usize = 10;
/// `s_log_zone_size` — log2(blocks per zone); always 0 here (u16).
pub const SUPER_LOG_ZONE_SIZE_OFF: usize = 12;
/// `s_flags` — filesystem state flags (u16).
pub const SUPER_FLAGS_OFF: usize = 14;
/// `s_max_size` — largest representable file size (i32).
pub const SUPER_MAX_SIZE_OFF: usize = 16;
/// `s_zones` — total zones on the device (u32).
pub const SUPER_ZONES_OFF: usize = 20;
/// `s_magic` — [`SUPER_MAGIC_V3`] (i16).
pub const SUPER_MAGIC_OFF: usize = 24;
/// `s_pad2` — explicit padding MINIX writes as 0 (i16).
pub const SUPER_PAD2_OFF: usize = 26;
/// `s_block_size` — block size in bytes (u16).
pub const SUPER_BLOCK_SIZE_OFF: usize = 28;
/// `s_disk_version` — format sub-version, 0 (i8).
pub const SUPER_DISK_VERSION_OFF: usize = 30;

// The fields tile the on-disk superblock exactly, with no gap and no overlap.
const _: () = assert!(SUPER_NINODES_OFF + 4 == SUPER_NZONES_OFF);
const _: () = assert!(SUPER_NZONES_OFF + 2 == SUPER_IMAP_BLOCKS_OFF);
const _: () = assert!(SUPER_IMAP_BLOCKS_OFF + 2 == SUPER_ZMAP_BLOCKS_OFF);
const _: () = assert!(SUPER_ZMAP_BLOCKS_OFF + 2 == SUPER_FIRSTDATAZONE_OLD_OFF);
const _: () = assert!(SUPER_FIRSTDATAZONE_OLD_OFF + 2 == SUPER_LOG_ZONE_SIZE_OFF);
const _: () = assert!(SUPER_LOG_ZONE_SIZE_OFF + 2 == SUPER_FLAGS_OFF);
const _: () = assert!(SUPER_FLAGS_OFF + 2 == SUPER_MAX_SIZE_OFF);
const _: () = assert!(SUPER_MAX_SIZE_OFF + 4 == SUPER_ZONES_OFF);
const _: () = assert!(SUPER_ZONES_OFF + 4 == SUPER_MAGIC_OFF);
const _: () = assert!(SUPER_MAGIC_OFF + 2 == SUPER_PAD2_OFF);
const _: () = assert!(SUPER_PAD2_OFF + 2 == SUPER_BLOCK_SIZE_OFF);
const _: () = assert!(SUPER_BLOCK_SIZE_OFF + 2 == SUPER_DISK_VERSION_OFF);
const _: () = assert!(SUPER_DISK_VERSION_OFF + 1 == SUPER_ON_DISK_LEN);

/// Why a superblock was rejected. Distinct variants because they mean different
/// things to whoever mounted the device: bad magic is "this is not a MinixFS",
/// the rest are "this is a MinixFS this build cannot read".
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SuperError {
    /// `s_magic` is not [`SUPER_MAGIC_V3`].
    BadMagic(i16),
    /// `s_block_size` is not [`crate::MFS_BLOCK_SIZE`].
    BadBlockSize(u16),
    /// `s_log_zone_size` is non-zero — multi-block zones are not supported.
    BadZoneSize(u16),
    /// `s_ninodes` or `s_zones` is zero: there is no filesystem here.
    Empty,
}

/// The on-disk superblock, decoded.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Superblock {
    pub ninodes: u32,
    pub nzones_v1: u16,
    pub imap_blocks: i16,
    pub zmap_blocks: i16,
    pub first_data_zone_old: u16,
    pub log_zone_size: u16,
    pub flags: u16,
    pub max_size: i32,
    pub zones: u32,
    pub magic: i16,
    pub pad2: i16,
    pub block_size: u16,
    pub disk_version: i8,
}

impl Superblock {
    /// Decode a superblock from the first [`SUPER_ON_DISK_LEN`] bytes of `b`.
    ///
    /// `None` only when `b` is too short — a well-sized but nonsensical
    /// superblock decodes fine and is rejected by [`validate`](Self::validate),
    /// so "malformed bytes" and "wrong filesystem" stay separable.
    pub fn from_le_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < SUPER_ON_DISK_LEN {
            return None;
        }
        Some(Self {
            ninodes: rd_u32(b, SUPER_NINODES_OFF),
            nzones_v1: rd_u16(b, SUPER_NZONES_OFF),
            imap_blocks: rd_u16(b, SUPER_IMAP_BLOCKS_OFF) as i16,
            zmap_blocks: rd_u16(b, SUPER_ZMAP_BLOCKS_OFF) as i16,
            first_data_zone_old: rd_u16(b, SUPER_FIRSTDATAZONE_OLD_OFF),
            log_zone_size: rd_u16(b, SUPER_LOG_ZONE_SIZE_OFF),
            flags: rd_u16(b, SUPER_FLAGS_OFF),
            max_size: rd_u32(b, SUPER_MAX_SIZE_OFF) as i32,
            zones: rd_u32(b, SUPER_ZONES_OFF),
            magic: rd_u16(b, SUPER_MAGIC_OFF) as i16,
            pad2: rd_u16(b, SUPER_PAD2_OFF) as i16,
            block_size: rd_u16(b, SUPER_BLOCK_SIZE_OFF),
            disk_version: b[SUPER_DISK_VERSION_OFF] as i8,
        })
    }

    /// Encode back to the on-disk form.
    pub fn to_le_bytes(&self) -> [u8; SUPER_ON_DISK_LEN] {
        let mut b = [0u8; SUPER_ON_DISK_LEN];
        wr_u32(&mut b, SUPER_NINODES_OFF, self.ninodes);
        wr_u16(&mut b, SUPER_NZONES_OFF, self.nzones_v1);
        wr_u16(&mut b, SUPER_IMAP_BLOCKS_OFF, self.imap_blocks as u16);
        wr_u16(&mut b, SUPER_ZMAP_BLOCKS_OFF, self.zmap_blocks as u16);
        wr_u16(
            &mut b,
            SUPER_FIRSTDATAZONE_OLD_OFF,
            self.first_data_zone_old,
        );
        wr_u16(&mut b, SUPER_LOG_ZONE_SIZE_OFF, self.log_zone_size);
        wr_u16(&mut b, SUPER_FLAGS_OFF, self.flags);
        wr_u32(&mut b, SUPER_MAX_SIZE_OFF, self.max_size as u32);
        wr_u32(&mut b, SUPER_ZONES_OFF, self.zones);
        wr_u16(&mut b, SUPER_MAGIC_OFF, self.magic as u16);
        wr_u16(&mut b, SUPER_PAD2_OFF, self.pad2 as u16);
        wr_u16(&mut b, SUPER_BLOCK_SIZE_OFF, self.block_size);
        b[SUPER_DISK_VERSION_OFF] = self.disk_version as u8;
        b
    }

    /// Is this a MinixFS v3 this build can read?
    ///
    /// Checked in the order that tells the caller the most: magic first ("is this
    /// even a MinixFS"), then the two shape constraints minix.rs imposes, then
    /// emptiness.
    pub fn validate(&self) -> Result<(), SuperError> {
        if self.magic != SUPER_MAGIC_V3 {
            return Err(SuperError::BadMagic(self.magic));
        }
        if self.block_size as usize != crate::MFS_BLOCK_SIZE {
            return Err(SuperError::BadBlockSize(self.block_size));
        }
        if self.log_zone_size != 0 {
            return Err(SuperError::BadZoneSize(self.log_zone_size));
        }
        if self.ninodes == 0 || self.zones == 0 {
            return Err(SuperError::Empty);
        }
        Ok(())
    }
}

fn rd_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn rd_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn wr_u16(b: &mut [u8], off: usize, v: u16) {
    b[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

fn wr_u32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plausible superblock for the slice-5.7 root image.
    fn sample() -> Superblock {
        Superblock {
            ninodes: 64,
            nzones_v1: 256,
            imap_blocks: 1,
            zmap_blocks: 1,
            first_data_zone_old: 5,
            log_zone_size: 0,
            flags: MFSFLAG_CLEAN,
            max_size: 4_222_976,
            zones: 256,
            magic: SUPER_MAGIC_V3,
            pad2: 0,
            block_size: crate::MFS_BLOCK_SIZE as u16,
            disk_version: 0,
        }
    }

    #[test]
    fn the_codec_round_trips_every_field() {
        // Each field of `sample` is distinct from the others where it can be, so
        // a swapped pair of offsets fails rather than cancelling out.
        let s = sample();
        assert_eq!(Superblock::from_le_bytes(&s.to_le_bytes()), Some(s));
    }

    #[test]
    fn the_encoded_form_is_exactly_the_on_disk_length() {
        assert_eq!(sample().to_le_bytes().len(), SUPER_ON_DISK_LEN);
        assert_eq!(SUPER_ON_DISK_LEN, 31);
        assert_eq!(SUPER_OFFSET, 1024);
    }

    #[test]
    fn a_short_buffer_decodes_to_none() {
        let bytes = sample().to_le_bytes();
        for short in 0..SUPER_ON_DISK_LEN {
            assert_eq!(Superblock::from_le_bytes(&bytes[..short]), None, "{short}");
        }
        assert!(Superblock::from_le_bytes(&bytes).is_some());
    }

    #[test]
    fn a_longer_buffer_reads_only_the_superblock() {
        // A caller hands in a whole 4096-byte block; the trailing bytes must be
        // ignored rather than shifting any field.
        let mut block = [0xAAu8; crate::MFS_BLOCK_SIZE];
        block[..SUPER_ON_DISK_LEN].copy_from_slice(&sample().to_le_bytes());
        assert_eq!(Superblock::from_le_bytes(&block), Some(sample()));
    }

    #[test]
    fn a_valid_superblock_validates() {
        assert_eq!(sample().validate(), Ok(()));
    }

    #[test]
    fn bad_magic_is_reported_with_the_value_seen() {
        let mut s = sample();
        s.magic = 0x137F; // MinixFS v1's magic: a real filesystem, wrong version.
        assert_eq!(s.validate(), Err(SuperError::BadMagic(0x137F)));
    }

    #[test]
    fn a_block_size_other_than_4096_is_refused() {
        // 1 KiB and 2 KiB are legal MinixFS v3, and deliberately not supported: a
        // block must be one page and one BDEV transfer.
        for bs in [1024u16, 2048, 512, 0] {
            let mut s = sample();
            s.block_size = bs;
            assert_eq!(s.validate(), Err(SuperError::BadBlockSize(bs)), "bs {bs}");
        }
    }

    #[test]
    fn a_multi_block_zone_is_refused() {
        let mut s = sample();
        s.log_zone_size = 1;
        assert_eq!(s.validate(), Err(SuperError::BadZoneSize(1)));
    }

    #[test]
    fn an_empty_filesystem_is_refused() {
        let mut s = sample();
        s.ninodes = 0;
        assert_eq!(s.validate(), Err(SuperError::Empty));

        let mut s = sample();
        s.zones = 0;
        assert_eq!(s.validate(), Err(SuperError::Empty));
    }

    #[test]
    fn magic_is_checked_before_the_shape_constraints() {
        // A caller pointed at the wrong device should hear "not a MinixFS", not a
        // complaint about a block size read out of unrelated bytes.
        let mut s = sample();
        s.magic = 0;
        s.block_size = 1024;
        s.log_zone_size = 3;
        s.ninodes = 0;
        assert_eq!(s.validate(), Err(SuperError::BadMagic(0)));
    }
}
