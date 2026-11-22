// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Where everything sits on the disk (slice 6.4, decision P1).
//!
//! ```text
//! LBA 0                 protective MBR
//! LBA 1                 primary GPT header
//! LBA 2..=33            primary partition entry array (128 x 128 bytes)
//! LBA 2048              partition 1: the ESP (FAT32), 34 MiB
//! LBA 71680             partition 2: the root, exactly ROOTFS_IMAGE_BYTES
//! LBA 75743..=75774     backup partition entry array
//! LBA 75775             backup GPT header
//! ```
//!
//! **Every number is derived, none is typed twice.** The inputs are the sector
//! size, the entry array's shape, the 1 MiB alignment, the ESP's size in MiB and
//! `ROOTFS_IMAGE_BYTES`; the rest follows, and the const assertions below hold
//! the properties the rest of the slice leans on. The literal LBAs in the
//! diagram above are documentation — a host test pins them once, because the
//! boot-marker files carry `lba=71680` and `capacity=75776`.
//!
//! The disk is the same size with or without a root partition: the rootless
//! image (the ramdisk boot's) leaves the root's range unused, so the driver's
//! `capacity=` marker reads the same on both boots.

use minixrs_kernel_shared::callnr::BDEV_BLOCK_SIZE;
use minixrs_kernel_shared::rootfs::ROOTFS_IMAGE_BYTES;

use crate::fat32;

/// Bytes in a sector. GPT calls this the logical block size; the image is a
/// 512-byte-sector disk, which is what QEMU's virtio-blk presents by default.
pub const SECTOR_SIZE: usize = 512;

/// Sectors in one mebibyte — the alignment every partition starts on.
pub const SECTORS_PER_MIB: u64 = (1024 * 1024 / SECTOR_SIZE) as u64;

/// Sectors in one `BDEV` block (4096 bytes). A partition served as a minor must
/// start on a multiple of this (decision P6).
pub const SECTORS_PER_BLOCK: u64 = (BDEV_BLOCK_SIZE / SECTOR_SIZE) as u64;

/// Partition entries in each entry array. 128 is what the UEFI specification
/// requires room for, and what every other GPT tool writes.
pub const GPT_ENTRY_COUNT: u32 = 128;

/// Bytes in one partition entry.
pub const GPT_ENTRY_SIZE: usize = 128;

/// Bytes in one whole entry array.
pub const GPT_ENTRY_ARRAY_BYTES: usize = GPT_ENTRY_COUNT as usize * GPT_ENTRY_SIZE;

/// Sectors in one whole entry array.
pub const GPT_ENTRY_ARRAY_SECTORS: u64 = (GPT_ENTRY_ARRAY_BYTES / SECTOR_SIZE) as u64;

/// LBA of the protective MBR.
pub const PROTECTIVE_MBR_LBA: u64 = 0;

/// LBA of the primary GPT header.
pub const PRIMARY_HEADER_LBA: u64 = PROTECTIVE_MBR_LBA + 1;

/// First LBA of the primary entry array.
pub const PRIMARY_ENTRIES_LBA: u64 = PRIMARY_HEADER_LBA + 1;

/// First LBA a partition may occupy: the sector after the primary entry array.
pub const FIRST_USABLE_LBA: u64 = PRIMARY_ENTRIES_LBA + GPT_ENTRY_ARRAY_SECTORS;

/// Sectors the backup GPT takes at the end of the disk: its entry array, then
/// its header in the last sector.
pub const BACKUP_GPT_SECTORS: u64 = GPT_ENTRY_ARRAY_SECTORS + 1;

/// The ESP's partition number (its 1-based slot in the entry array).
pub const ESP_PARTITION_NUMBER: u32 = 1;

/// The root's partition number (its 1-based slot in the entry array).
pub const ROOT_PARTITION_NUMBER: u32 = 2;

/// First LBA of the ESP: the first 1 MiB boundary a partition may start on.
pub const ESP_FIRST_LBA: u64 = align_up(FIRST_USABLE_LBA, SECTORS_PER_MIB);

/// Size of the ESP in mebibytes.
///
/// Chosen so the FAT32 volume has at least [`fat32::MIN_FAT32_CLUSTERS`]
/// clusters at one sector per cluster — below that bound a FAT driver reads the
/// volume as FAT16 whatever its boot sector says. The assertion below holds the
/// bound; shrinking this until it fails is a compile error, not a volume the
/// firmware misreads.
pub const ESP_MIB: u64 = 34;

/// Sectors in the ESP.
pub const ESP_SECTORS: u64 = ESP_MIB * SECTORS_PER_MIB;

/// The FAT32 geometry of the ESP.
pub const ESP_GEOMETRY: fat32::Geometry = fat32::geometry(ESP_SECTORS as u32);

/// First LBA of the root partition: the next 1 MiB boundary after the ESP.
pub const ROOT_FIRST_LBA: u64 = align_up(ESP_FIRST_LBA + ESP_SECTORS, SECTORS_PER_MIB);

/// Sectors in the root partition: exactly the root image, no slack.
pub const ROOT_SECTORS: u64 = (ROOTFS_IMAGE_BYTES / SECTOR_SIZE) as u64;

/// Sectors on the whole disk: everything above plus the backup GPT, rounded up
/// to a whole mebibyte (and so to a whole number of `BDEV` blocks).
pub const DISK_SECTORS: u64 = align_up(
    ROOT_FIRST_LBA + ROOT_SECTORS + BACKUP_GPT_SECTORS,
    SECTORS_PER_MIB,
);

/// Bytes on the whole disk — the length of a built image.
pub const DISK_BYTES: usize = DISK_SECTORS as usize * SECTOR_SIZE;

/// LBA of the backup GPT header: the disk's last sector.
pub const BACKUP_HEADER_LBA: u64 = DISK_SECTORS - 1;

/// First LBA of the backup entry array, which ends just before the backup
/// header.
pub const BACKUP_ENTRIES_LBA: u64 = BACKUP_HEADER_LBA - GPT_ENTRY_ARRAY_SECTORS;

/// Last LBA a partition may occupy (inclusive): the sector before the backup
/// entry array.
pub const LAST_USABLE_LBA: u64 = BACKUP_ENTRIES_LBA - 1;

/// Byte offset of the ESP in the image.
pub const ESP_OFFSET: usize = ESP_FIRST_LBA as usize * SECTOR_SIZE;

/// Bytes in the ESP.
pub const ESP_BYTES: usize = ESP_SECTORS as usize * SECTOR_SIZE;

/// Byte offset of the root partition in the image.
pub const ROOT_OFFSET: usize = ROOT_FIRST_LBA as usize * SECTOR_SIZE;

/// `n` rounded up to a multiple of `to`. Const-evaluated only, where an
/// overflow is a compile error rather than a wrapped layout.
const fn align_up(n: u64, to: u64) -> u64 {
    n.div_ceil(to) * to
}

// The entry array is a whole number of sectors, so "LBA 2..=33" is exact.
const _: () = assert!(GPT_ENTRY_ARRAY_BYTES.is_multiple_of(SECTOR_SIZE));
// A BDEV block and a mebibyte are whole numbers of sectors, and a mebibyte is a
// whole number of blocks — which is what makes "starts on a 1 MiB boundary"
// imply "starts on a block boundary".
const _: () = assert!(BDEV_BLOCK_SIZE.is_multiple_of(SECTOR_SIZE));
const _: () = assert!(SECTORS_PER_MIB.is_multiple_of(SECTORS_PER_BLOCK));

// Both partitions start on a 1 MiB boundary, inside the usable range, in order,
// without overlapping.
const _: () = assert!(ESP_FIRST_LBA.is_multiple_of(SECTORS_PER_MIB));
const _: () = assert!(ROOT_FIRST_LBA.is_multiple_of(SECTORS_PER_MIB));
const _: () = assert!(ESP_FIRST_LBA >= FIRST_USABLE_LBA);
const _: () = assert!(ESP_FIRST_LBA + ESP_SECTORS <= ROOT_FIRST_LBA);
const _: () = assert!(ROOT_FIRST_LBA + ROOT_SECTORS - 1 <= LAST_USABLE_LBA);

// The root partition is the root image exactly: a whole number of sectors and
// of blocks, with nothing after it for a driver to mistake for the tail block.
const _: () = assert!(ROOTFS_IMAGE_BYTES.is_multiple_of(SECTOR_SIZE));
const _: () = assert!(ROOT_SECTORS as usize * SECTOR_SIZE == ROOTFS_IMAGE_BYTES);
const _: () = assert!(ROOT_SECTORS.is_multiple_of(SECTORS_PER_BLOCK));

// The disk is a whole number of BDEV blocks, and the backup GPT is its tail.
const _: () = assert!(DISK_SECTORS.is_multiple_of(SECTORS_PER_BLOCK));
const _: () = assert!(BACKUP_ENTRIES_LBA + GPT_ENTRY_ARRAY_SECTORS == BACKUP_HEADER_LBA);
const _: () = assert!(LAST_USABLE_LBA + BACKUP_GPT_SECTORS + 1 == DISK_SECTORS);

// The FAT32 fields these feed are 32 bits wide.
const _: () = assert!(ESP_SECTORS <= u32::MAX as u64);
const _: () = assert!(ESP_FIRST_LBA <= u32::MAX as u64);

// **The FAT32 cluster bound.** Below 65 525 clusters the ESP is a FAT16 volume
// to every driver that follows Microsoft's rule, whatever the boot sector says.
const _: () = assert!(ESP_GEOMETRY.clusters >= fat32::MIN_FAT32_CLUSTERS);
// ...and each FAT has an entry for every cluster, plus the two reserved ones.
const _: () = assert!(
    ESP_GEOMETRY.fat_sectors as u64 * fat32::FAT_ENTRIES_PER_SECTOR as u64
        >= ESP_GEOMETRY.clusters as u64 + fat32::FIRST_DATA_CLUSTER as u64
);
