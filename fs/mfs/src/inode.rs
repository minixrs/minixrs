// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The MinixFS v3 inode: its on-disk layout and codec.
//!
//! Layout is MINIX 3's `struct d2_inode` (`servers/mfs/inode.h`) — 64 bytes, of
//! which the last 40 are the ten zone pointers. Seven are direct, the eighth is
//! the single-indirect block, the ninth double-indirect, and the tenth is
//! reserved (MINIX never uses a triple-indirect on V2/V3).
//!
//! Inode *numbering* starts at 1: inode 0 does not exist, which is what makes 0 a
//! usable "no such entry" marker in a directory slot. [`ROOT_INODE`] is 1.

/// Bytes per on-disk inode. Divides the block size evenly (64 inodes per block).
pub const INODE_SIZE: usize = 64;

/// Zone pointers stored directly in the inode (`V2_NR_DZONES`).
pub const NR_DIRECT_ZONES: usize = 7;

/// Total zone pointers in an inode (`V2_NR_TZONES`): 7 direct, 1 single-indirect,
/// 1 double-indirect, 1 reserved.
pub const NR_TZONES: usize = 10;

/// Index of the single-indirect zone pointer within `Inode::zone`.
pub const SINGLE_INDIRECT_SLOT: usize = NR_DIRECT_ZONES;

/// Index of the double-indirect zone pointer. Never followed by [`crate::read`] —
/// see that module's `zone_for_offset` for why the single-indirect span already
/// covers every file this filesystem can hold.
pub const DOUBLE_INDIRECT_SLOT: usize = NR_DIRECT_ZONES + 1;

/// The root directory's inode number. Inode 0 does not exist.
pub const ROOT_INODE: u32 = 1;

/// `i_mode` type field mask (MINIX `I_TYPE`).
pub const I_TYPE: u16 = 0o170000;
/// `i_mode` type: regular file.
pub const I_REGULAR: u16 = 0o100000;
/// `i_mode` type: directory.
pub const I_DIRECTORY: u16 = 0o040000;

// Field offsets within the on-disk inode, in declaration order.
/// `d2_mode` (u16).
pub const INODE_MODE_OFF: usize = 0;
/// `d2_nlinks` (u16).
pub const INODE_NLINKS_OFF: usize = 2;
/// `d2_uid` (i16).
pub const INODE_UID_OFF: usize = 4;
/// `d2_gid` (u16).
pub const INODE_GID_OFF: usize = 6;
/// `d2_size` (i32).
pub const INODE_SIZE_OFF: usize = 8;
/// `d2_atime` (i32).
pub const INODE_ATIME_OFF: usize = 12;
/// `d2_mtime` (i32).
pub const INODE_MTIME_OFF: usize = 16;
/// `d2_ctime` (i32).
pub const INODE_CTIME_OFF: usize = 20;
/// `d2_zone[0]` (u32 × [`NR_TZONES`]).
pub const INODE_ZONE_OFF: usize = 24;

// The zone array is what fills the inode out to exactly 64 bytes.
const _: () = assert!(INODE_ZONE_OFF + NR_TZONES * 4 == INODE_SIZE);
const _: () = assert!(NR_DIRECT_ZONES < NR_TZONES);
const _: () = assert!(crate::MFS_BLOCK_SIZE.is_multiple_of(INODE_SIZE));

/// An on-disk inode, decoded.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Inode {
    pub mode: u16,
    pub nlinks: u16,
    pub uid: i16,
    pub gid: u16,
    /// File size in bytes. Signed on disk (MINIX's `i32_t`); a negative value is
    /// a corrupt inode, and [`crate::read::zone_for_offset`] never consults it.
    pub size: i32,
    pub atime: i32,
    pub mtime: i32,
    pub ctime: i32,
    /// Zone pointers. `0` is a hole — zone 0 is not a data zone in MinixFS.
    pub zone: [u32; NR_TZONES],
}

impl Inode {
    /// An all-zero inode: mode 0, no links, no zones. What a freshly formatted
    /// inode table is full of.
    pub const EMPTY: Self = Self {
        mode: 0,
        nlinks: 0,
        uid: 0,
        gid: 0,
        size: 0,
        atime: 0,
        mtime: 0,
        ctime: 0,
        zone: [0; NR_TZONES],
    };

    /// Decode an inode from the first [`INODE_SIZE`] bytes of `b`. `None` if `b`
    /// is short.
    pub fn from_le_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < INODE_SIZE {
            return None;
        }
        let mut zone = [0u32; NR_TZONES];
        for (i, z) in zone.iter_mut().enumerate() {
            *z = rd_u32(b, INODE_ZONE_OFF + i * 4);
        }
        Some(Self {
            mode: rd_u16(b, INODE_MODE_OFF),
            nlinks: rd_u16(b, INODE_NLINKS_OFF),
            uid: rd_u16(b, INODE_UID_OFF) as i16,
            gid: rd_u16(b, INODE_GID_OFF),
            size: rd_u32(b, INODE_SIZE_OFF) as i32,
            atime: rd_u32(b, INODE_ATIME_OFF) as i32,
            mtime: rd_u32(b, INODE_MTIME_OFF) as i32,
            ctime: rd_u32(b, INODE_CTIME_OFF) as i32,
            zone,
        })
    }

    /// Encode back to the on-disk form.
    pub fn to_le_bytes(&self) -> [u8; INODE_SIZE] {
        let mut b = [0u8; INODE_SIZE];
        wr_u16(&mut b, INODE_MODE_OFF, self.mode);
        wr_u16(&mut b, INODE_NLINKS_OFF, self.nlinks);
        wr_u16(&mut b, INODE_UID_OFF, self.uid as u16);
        wr_u16(&mut b, INODE_GID_OFF, self.gid);
        wr_u32(&mut b, INODE_SIZE_OFF, self.size as u32);
        wr_u32(&mut b, INODE_ATIME_OFF, self.atime as u32);
        wr_u32(&mut b, INODE_MTIME_OFF, self.mtime as u32);
        wr_u32(&mut b, INODE_CTIME_OFF, self.ctime as u32);
        for (i, z) in self.zone.iter().enumerate() {
            wr_u32(&mut b, INODE_ZONE_OFF + i * 4, *z);
        }
        b
    }

    /// Is this a directory?
    pub fn is_dir(&self) -> bool {
        self.mode & I_TYPE == I_DIRECTORY
    }

    /// Is this a regular file?
    pub fn is_reg(&self) -> bool {
        self.mode & I_TYPE == I_REGULAR
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

    fn sample() -> Inode {
        let mut zone = [0u32; NR_TZONES];
        // Distinct, non-sequential values so a swapped pair of slots fails.
        for (i, z) in zone.iter_mut().enumerate() {
            *z = 100 + (i as u32) * 7;
        }
        Inode {
            mode: I_REGULAR | 0o644,
            nlinks: 1,
            uid: -3,
            gid: 42,
            size: 123_456,
            atime: 11,
            mtime: 22,
            ctime: 33,
            zone,
        }
    }

    #[test]
    fn the_codec_round_trips_every_field() {
        let i = sample();
        assert_eq!(Inode::from_le_bytes(&i.to_le_bytes()), Some(i));
    }

    #[test]
    fn a_short_buffer_decodes_to_none() {
        let bytes = sample().to_le_bytes();
        for short in [0usize, 1, 24, INODE_SIZE - 1] {
            assert_eq!(Inode::from_le_bytes(&bytes[..short]), None, "{short}");
        }
    }

    #[test]
    fn the_encoded_form_is_exactly_sixty_four_bytes() {
        assert_eq!(sample().to_le_bytes().len(), INODE_SIZE);
        assert_eq!(INODE_SIZE, 64);
        assert_eq!(crate::MFS_BLOCK_SIZE / INODE_SIZE, 64);
    }

    #[test]
    fn the_zone_array_starts_where_the_timestamps_end() {
        // The zone pointers are what a reader actually follows, so pin their
        // offset against the fields that precede them rather than a literal.
        assert_eq!(INODE_CTIME_OFF + 4, INODE_ZONE_OFF);
        assert_eq!(INODE_ZONE_OFF, 24);
        assert_eq!(SINGLE_INDIRECT_SLOT, 7);
        assert_eq!(DOUBLE_INDIRECT_SLOT, 8);
    }

    #[test]
    fn a_zone_pointer_lands_at_its_own_offset() {
        // Encode one non-zero pointer at a time and check nothing else moved.
        for slot in 0..NR_TZONES {
            let mut i = Inode::EMPTY;
            i.zone[slot] = 0xDEAD_BEEF;
            let b = i.to_le_bytes();
            assert_eq!(
                &b[INODE_ZONE_OFF + slot * 4..INODE_ZONE_OFF + slot * 4 + 4],
                &0xDEAD_BEEFu32.to_le_bytes(),
                "slot {slot}"
            );
            assert_eq!(Inode::from_le_bytes(&b).unwrap().zone[slot], 0xDEAD_BEEF);
        }
    }

    #[test]
    fn the_type_bits_classify_regular_files_and_directories() {
        let mut i = Inode::EMPTY;
        i.mode = I_REGULAR | 0o755;
        assert!(i.is_reg());
        assert!(!i.is_dir());

        i.mode = I_DIRECTORY | 0o755;
        assert!(i.is_dir());
        assert!(!i.is_reg());

        // Permission bits alone are neither: an all-zero inode is a free slot.
        assert!(!Inode::EMPTY.is_dir());
        assert!(!Inode::EMPTY.is_reg());
    }

    #[test]
    fn an_empty_inode_encodes_to_all_zeroes() {
        assert_eq!(Inode::EMPTY.to_le_bytes(), [0u8; INODE_SIZE]);
        assert_eq!(Inode::from_le_bytes(&[0u8; INODE_SIZE]), Some(Inode::EMPTY));
    }
}
