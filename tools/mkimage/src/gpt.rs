// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The partition tables: a protective MBR and both copies of the GPT.
//!
//! Written from the UEFI specification's "GUID Partition Table (GPT) Disk
//! Layout" chapter. **Nothing here is shared with `driver-rt::gpt`**, the parser
//! `virtio-blk` uses: the field offsets below and the ones there were each
//! taken from the specification, and a host test parses this writer's output
//! with that parser. edk2, which reads the same tables to find the ESP, is the
//! third reading.
//!
//! The image is reproducible, so the three GUIDs a GPT tool would normally draw
//! at random — the disk's and each partition's *unique* GUID — are constants.

use minixrs_kernel_shared::rootfs::ROOT_PARTITION_TYPE_GUID;

use crate::crc::crc32;
use crate::layout::{
    BACKUP_ENTRIES_LBA, BACKUP_HEADER_LBA, DISK_BYTES, DISK_SECTORS, ESP_FIRST_LBA, ESP_SECTORS,
    FIRST_USABLE_LBA, GPT_ENTRY_ARRAY_BYTES, GPT_ENTRY_COUNT, GPT_ENTRY_SIZE, LAST_USABLE_LBA,
    PRIMARY_ENTRIES_LBA, PRIMARY_HEADER_LBA, PROTECTIVE_MBR_LBA, ROOT_FIRST_LBA, ROOT_SECTORS,
    SECTOR_SIZE,
};

/// A GUID as the 16 bytes GPT stores: the first three fields little-endian, the
/// last eight bytes in the order they are written. `guid(0xC12A7328, 0xF81F,
/// 0x11D2, [0xBA, 0x4B, ..])` is the text form `C12A7328-F81F-11D2-BA4B-…`.
#[must_use]
pub const fn guid(time_low: u32, time_mid: u16, time_hi: u16, rest: [u8; 8]) -> [u8; 16] {
    let a = time_low.to_le_bytes();
    let b = time_mid.to_le_bytes();
    let c = time_hi.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], rest[0], rest[1], rest[2], rest[3],
        rest[4], rest[5], rest[6], rest[7],
    ]
}

/// The disk's GUID, `01C87657-FA93-4221-9F12-011CE0C42E0F`. Fixed: see the
/// module documentation.
pub const DISK_GUID: [u8; 16] = guid(
    0x01C8_7657,
    0xFA93,
    0x4221,
    [0x9F, 0x12, 0x01, 0x1C, 0xE0, 0xC4, 0x2E, 0x0F],
);

/// The EFI System Partition type, `C12A7328-F81F-11D2-BA4B-00A0C93EC93B` — the
/// one type GUID the firmware looks for.
pub const ESP_TYPE_GUID: [u8; 16] = guid(
    0xC12A_7328,
    0xF81F,
    0x11D2,
    [0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B],
);

/// The ESP's unique GUID, `4BEF5E26-A1E0-4D22-96DF-33C6AA1F576C`.
pub const ESP_UNIQUE_GUID: [u8; 16] = guid(
    0x4BEF_5E26,
    0xA1E0,
    0x4D22,
    [0x96, 0xDF, 0x33, 0xC6, 0xAA, 0x1F, 0x57, 0x6C],
);

/// The root partition's unique GUID, `15C269EB-0329-4BEE-B037-7576B91F3725`.
/// Its *type* is `kernel-shared`'s `ROOT_PARTITION_TYPE_GUID`.
pub const ROOT_UNIQUE_GUID: [u8; 16] = guid(
    0x15C2_69EB,
    0x0329,
    0x4BEE,
    [0xB0, 0x37, 0x75, 0x76, 0xB9, 0x1F, 0x37, 0x25],
);

/// UTF-16 code units in a partition entry's name field.
pub const NAME_UNITS: usize = 36;

/// One partition, as [`write_tables`] takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Partition {
    /// What the partition is, as on-disk bytes.
    pub type_guid: [u8; 16],
    /// Which partition it is, as on-disk bytes.
    pub unique_guid: [u8; 16],
    /// Its first LBA.
    pub first_lba: u64,
    /// Its length in sectors; at least 1.
    pub sectors: u64,
    /// Its human-readable name, at most [`NAME_UNITS`] UTF-16 units.
    pub name: &'static str,
}

/// Partition 1: the EFI System Partition.
pub const ESP: Partition = Partition {
    type_guid: ESP_TYPE_GUID,
    unique_guid: ESP_UNIQUE_GUID,
    first_lba: ESP_FIRST_LBA,
    sectors: ESP_SECTORS,
    name: "ESP",
};

/// Partition 2: the minix.rs root.
pub const ROOT: Partition = Partition {
    type_guid: ROOT_PARTITION_TYPE_GUID,
    unique_guid: ROOT_UNIQUE_GUID,
    first_lba: ROOT_FIRST_LBA,
    sectors: ROOT_SECTORS,
    name: "minix.rs root",
};

// Field offsets in the protective MBR (UEFI, "Protective MBR").
const MBR_PARTITION_RECORD: usize = 446;
const MBR_SIGNATURE: usize = 510;
/// The partition type that says "this disk is GPT; keep out".
const MBR_TYPE_GPT_PROTECTIVE: u8 = 0xEE;

// Field offsets in a GPT header (UEFI, "GPT Header").
const H_SIGNATURE: usize = 0;
const H_REVISION: usize = 8;
const H_HEADER_SIZE: usize = 12;
const H_HEADER_CRC: usize = 16;
const H_MY_LBA: usize = 24;
const H_ALTERNATE_LBA: usize = 32;
const H_FIRST_USABLE: usize = 40;
const H_LAST_USABLE: usize = 48;
const H_DISK_GUID: usize = 56;
const H_ENTRIES_LBA: usize = 72;
const H_ENTRY_COUNT: usize = 80;
const H_ENTRY_SIZE: usize = 84;
const H_ENTRIES_CRC: usize = 88;
/// Bytes of header the header CRC covers, and the value of its size field.
const HEADER_SIZE: usize = 92;
/// Revision 1.0: minor in the low half, major in the high half.
const REVISION: u32 = 0x0001_0000;

// Field offsets in a partition entry (UEFI, "GPT Partition Entry").
const E_TYPE_GUID: usize = 0;
const E_UNIQUE_GUID: usize = 16;
const E_FIRST_LBA: usize = 32;
const E_LAST_LBA: usize = 40;
const E_NAME: usize = 56;

const _: () = assert!(E_NAME + NAME_UNITS * 2 == GPT_ENTRY_SIZE);
const _: () = assert!(HEADER_SIZE <= SECTOR_SIZE);

fn put(bytes: &mut [u8], offset: usize, value: &[u8]) {
    bytes[offset..offset + value.len()].copy_from_slice(value);
}

fn sector_mut(image: &mut [u8], lba: u64) -> &mut [u8] {
    // Every LBA passed here is a layout constant below DISK_SECTORS, and the
    // image is DISK_BYTES long, so the cast and the slice are in range.
    let start = lba as usize * SECTOR_SIZE;
    &mut image[start..start + SECTOR_SIZE]
}

/// The protective MBR: one partition record of type `0xEE` from LBA 1 to the
/// end of the disk, so a tool that knows only MBR sees a full disk it does not
/// understand rather than an empty one.
fn protective_mbr() -> [u8; SECTOR_SIZE] {
    let mut mbr = [0u8; SECTOR_SIZE];
    let record = &mut mbr[MBR_PARTITION_RECORD..MBR_PARTITION_RECORD + 16];
    // Not bootable; starting CHS 0/0/2, which is LBA 1.
    record[0] = 0x00;
    put(record, 1, &[0x00, 0x02, 0x00]);
    record[4] = MBR_TYPE_GPT_PROTECTIVE;
    // Ending CHS: the "too large to say" value, as every GPT tool writes it;
    // nothing addresses this disk by cylinder.
    put(record, 5, &[0xFF, 0xFF, 0xFF]);
    put(record, 8, &(PRIMARY_HEADER_LBA as u32).to_le_bytes());
    // Size in sectors, saturating: a disk past 2 TiB says 0xFFFFFFFF.
    let size = u32::try_from(DISK_SECTORS - 1).unwrap_or(u32::MAX);
    put(record, 12, &size.to_le_bytes());
    put(&mut mbr, MBR_SIGNATURE, &[0x55, 0xAA]);
    mbr
}

/// The entry array for `partitions`, slot `i` holding `partitions[i]`; the
/// remaining slots are unused (all zero).
fn entry_array(partitions: &[Partition]) -> Vec<u8> {
    assert!(
        partitions.len() <= GPT_ENTRY_COUNT as usize,
        "more partitions than entries"
    );
    let mut array = vec![0u8; GPT_ENTRY_ARRAY_BYTES];
    for (slot, p) in array
        .as_chunks_mut::<GPT_ENTRY_SIZE>()
        .0
        .iter_mut()
        .zip(partitions)
    {
        put(slot, E_TYPE_GUID, &p.type_guid);
        put(slot, E_UNIQUE_GUID, &p.unique_guid);
        put(slot, E_FIRST_LBA, &p.first_lba.to_le_bytes());
        // The last LBA is inclusive.
        let last_lba = p.first_lba + p.sectors - 1;
        put(slot, E_LAST_LBA, &last_lba.to_le_bytes());
        // Attributes stay zero. The name is UTF-16LE, NUL-padded.
        for (i, unit) in p.name.encode_utf16().take(NAME_UNITS).enumerate() {
            put(slot, E_NAME + 2 * i, &unit.to_le_bytes());
        }
    }
    array
}

/// One GPT header sector. The primary and the backup differ in three fields —
/// where the header itself is, where the other one is, and where its entry
/// array is — and so in their CRCs.
fn header(
    my_lba: u64,
    alternate_lba: u64,
    entries_lba: u64,
    entries_crc: u32,
) -> [u8; SECTOR_SIZE] {
    let mut h = [0u8; SECTOR_SIZE];
    put(&mut h, H_SIGNATURE, b"EFI PART");
    put(&mut h, H_REVISION, &REVISION.to_le_bytes());
    put(&mut h, H_HEADER_SIZE, &(HEADER_SIZE as u32).to_le_bytes());
    put(&mut h, H_MY_LBA, &my_lba.to_le_bytes());
    put(&mut h, H_ALTERNATE_LBA, &alternate_lba.to_le_bytes());
    put(&mut h, H_FIRST_USABLE, &FIRST_USABLE_LBA.to_le_bytes());
    put(&mut h, H_LAST_USABLE, &LAST_USABLE_LBA.to_le_bytes());
    put(&mut h, H_DISK_GUID, &DISK_GUID);
    put(&mut h, H_ENTRIES_LBA, &entries_lba.to_le_bytes());
    put(&mut h, H_ENTRY_COUNT, &GPT_ENTRY_COUNT.to_le_bytes());
    put(&mut h, H_ENTRY_SIZE, &(GPT_ENTRY_SIZE as u32).to_le_bytes());
    put(&mut h, H_ENTRIES_CRC, &entries_crc.to_le_bytes());
    // Last: the header's own CRC, over its 92 bytes with this field still zero.
    let crc = crc32(&h[..HEADER_SIZE]);
    put(&mut h, H_HEADER_CRC, &crc.to_le_bytes());
    h
}

/// Write the protective MBR, the primary header and entry array, and the backup
/// entry array and header into `image`. `partitions[i]` becomes partition
/// `i + 1`.
///
/// # Panics
///
/// If `image` is not [`DISK_BYTES`] long, or there are more partitions than
/// entries — both are this crate's own mistakes, not something an input can
/// cause.
pub fn write_tables(image: &mut [u8], partitions: &[Partition]) {
    assert_eq!(image.len(), DISK_BYTES, "the image is not the disk's size");

    let entries = entry_array(partitions);
    let entries_crc = crc32(&entries);

    sector_mut(image, PROTECTIVE_MBR_LBA).copy_from_slice(&protective_mbr());

    sector_mut(image, PRIMARY_HEADER_LBA).copy_from_slice(&header(
        PRIMARY_HEADER_LBA,
        BACKUP_HEADER_LBA,
        PRIMARY_ENTRIES_LBA,
        entries_crc,
    ));
    let primary = PRIMARY_ENTRIES_LBA as usize * SECTOR_SIZE;
    put(image, primary, &entries);

    let backup = BACKUP_ENTRIES_LBA as usize * SECTOR_SIZE;
    put(image, backup, &entries);
    sector_mut(image, BACKUP_HEADER_LBA).copy_from_slice(&header(
        BACKUP_HEADER_LBA,
        PRIMARY_HEADER_LBA,
        BACKUP_ENTRIES_LBA,
        entries_crc,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guid_is_stored_mixed_endian() {
        // The ESP type GUID's on-disk bytes are the best-known sixteen bytes in
        // GPT; a wrong encoding here is a disk no firmware boots.
        assert_eq!(
            ESP_TYPE_GUID,
            [
                0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E,
                0xC9, 0x3B
            ]
        );
        // And the encoder agrees with the hand-encoded constant in
        // `kernel-shared`: 10FDAC43-2E55-4E0E-8D18-2D46479E1B44.
        assert_eq!(
            guid(
                0x10FD_AC43,
                0x2E55,
                0x4E0E,
                [0x8D, 0x18, 0x2D, 0x46, 0x47, 0x9E, 0x1B, 0x44]
            ),
            ROOT_PARTITION_TYPE_GUID
        );
    }

    #[test]
    fn the_fixed_guids_are_distinct() {
        let all = [
            DISK_GUID,
            ESP_TYPE_GUID,
            ESP_UNIQUE_GUID,
            ROOT_UNIQUE_GUID,
            ROOT_PARTITION_TYPE_GUID,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn partition_names_fit_their_field() {
        for p in [ESP, ROOT] {
            assert!(p.name.encode_utf16().count() <= NAME_UNITS, "{}", p.name);
        }
    }

    #[test]
    fn the_last_lba_is_inclusive() {
        let array = entry_array(&[ESP, ROOT]);
        let last = u64::from_le_bytes(array[E_LAST_LBA..E_LAST_LBA + 8].try_into().unwrap());
        assert_eq!(last, ESP_FIRST_LBA + ESP_SECTORS - 1);
    }
}
