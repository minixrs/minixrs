// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Partitions as minors (slice 6.4, spec P5, P6, P7): from the bytes of the
//! disk's first blocks to the table of extents the driver serves, and from a
//! request to the sector it names. Pure — no IPC, no device access, no `unsafe` —
//! so everything here is host-tested; `main.rs` reads the blocks and does the
//! I/O.
//!
//! ## The minors
//!
//! | Minor   | Extent                                                        |
//! | ------- | ------------------------------------------------------------- |
//! | 0       | the root partition, selected by its type GUID                 |
//! | 1       | the whole disk                                                |
//! | `1 + n` | GPT partition `n` (its 1-based slot), for each **used** entry |
//!
//! So the root is reachable twice — as minor 0 and as `1 +` its slot — and both
//! name the same blocks. An [`Extent`] is in 4096-byte BDEV blocks, so a
//! partition whose first LBA or sector count is not a multiple of eight has no
//! minor at all; if that partition is the root, the disk is refused
//! ([`RootFail::Align`]).
//!
//! ## The sequence
//!
//! 1. `main.rs` reads blocks `0..TABLE_BLOCKS` of the whole disk into a
//!    [`TABLE_BUF_BYTES`]-byte buffer.
//! 2. [`scan`] turns that buffer and the device's capacity into a
//!    [`MinorTable`], or the [`RootFail`] to print.
//! 3. Per request, [`MinorTable::locate`] answers with the first sector to
//!    transfer or the errno to reply.
//!
//! All arithmetic is `checked_*`: drivers ship `--release` with
//! `overflow-checks = false`.

// Only `main.rs`'s `not(test)`-gated paths reach most of this, so under
// `cargo test` (which drops `_start` and `main`) it would read as dead code.
#![cfg_attr(test, allow(dead_code))]

use crate::blk::{RootFail, SECTORS_PER_BLOCK, block_to_sector, device_blocks};
use minixrs_driver_rt::gpt::{self, Lookup, Partition};
use minixrs_kernel_shared::callnr::{BDEV_BLOCK_SIZE, BDEV_MINOR_ROOT, BDEV_MINOR_WHOLE_DISK};
use minixrs_kernel_shared::error::{EINVAL, ENXIO};
use minixrs_kernel_shared::rootfs::ROOT_PARTITION_TYPE_GUID;
use minixrs_server_rt::bdev::{ReadRequest, validate_range};

/// Whole BDEV blocks from the start of the disk that cover the protective MBR,
/// the primary GPT header and a full entry array (LBA 0..=33): blocks 0..=4.
pub const TABLE_BLOCKS: usize = gpt::TABLE_BYTES.div_ceil(BDEV_BLOCK_SIZE);
/// [`TABLE_BLOCKS`] in bytes — the buffer [`scan`] is handed: 20 480, LBA 0..=39.
pub const TABLE_BUF_BYTES: usize = TABLE_BLOCKS * BDEV_BLOCK_SIZE;

// 17 408 bytes of table round up to five 4096-byte blocks.
const _: () = assert!(TABLE_BLOCKS == 5);
const _: () = assert!(TABLE_BUF_BYTES >= gpt::TABLE_BYTES);

/// The minor that is the whole disk — the shared constant, so that the one
/// client that opens it (MFS's `bdev.minor` boot probe) cannot disagree.
pub const MINOR_WHOLE_DISK: i32 = BDEV_MINOR_WHOLE_DISK;
/// Minors the table has room for: the root, the whole disk, and one per GPT
/// slot — `0..=129`.
pub const MINORS: usize = 2 + gpt::MAX_ENTRIES as usize;

// The root's minor and the whole disk's are the first two slots of the table,
// and partition 1 comes right after them.
const _: () = assert!(BDEV_MINOR_ROOT == 0);
const _: () = assert!(MINOR_WHOLE_DISK == 1);
const _: () = assert!(MINORS == 130);

/// A run of whole BDEV blocks on the disk: what one minor serves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Extent {
    /// The extent's first 4096-byte block, counted from the start of the disk.
    pub first_block: u64,
    /// How many blocks it holds.
    pub blocks: u64,
}

impl Extent {
    /// The first device sector of this extent's block `block`, which is
    /// **relative to the extent** — the block number of a request
    /// `bdev::validate_range` accepted against [`Extent::blocks`].
    ///
    /// `None` if `block` is not inside the extent, or if the disk block or its
    /// sector does not fit a `u64`. The extent's first block is added by
    /// `checked_add`, and only here, after the range check.
    pub const fn sector(&self, block: u64) -> Option<u64> {
        if block >= self.blocks {
            return None;
        }
        match self.first_block.checked_add(block) {
            Some(disk_block) => block_to_sector(disk_block),
            None => None,
        }
    }
}

/// The extent of a GPT partition, or `None` if it has none: its first LBA or its
/// sector count is not a multiple of [`SECTORS_PER_BLOCK`], or its range is
/// inverted or too large to count.
pub const fn partition_extent(p: &Partition) -> Option<Extent> {
    let Some(sectors) = p.sectors() else {
        return None;
    };
    if !p.first_lba.is_multiple_of(SECTORS_PER_BLOCK) || !sectors.is_multiple_of(SECTORS_PER_BLOCK)
    {
        return None;
    }
    // Exact: both are multiples of the divisor, which a const assert in `blk`
    // holds above zero.
    let (Some(first_block), Some(blocks)) = (
        p.first_lba.checked_div(SECTORS_PER_BLOCK),
        sectors.checked_div(SECTORS_PER_BLOCK),
    ) else {
        return None;
    };
    Some(Extent {
        first_block,
        blocks,
    })
}

/// What a well-formed request asks of the device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Located {
    /// Zero bytes: reply `0` with no grant use and no device I/O.
    Empty,
    /// `len` bytes (`1..=BDEV_MAX_IO`) of the block that starts at device sector
    /// `sector`.
    Io {
        /// The block's first 512-byte sector, counted from the start of the disk.
        sector: u64,
        /// Bytes of the block the client asked for.
        len: usize,
    },
}

/// The minors of a disk whose partition table was accepted and which has exactly
/// one root partition, block-aligned. Built by [`scan`] and by nothing else.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MinorTable {
    /// Indexed by minor.
    extents: [Option<Extent>; MINORS],
    /// The root partition's 1-based slot.
    root_index: u32,
    /// The root partition's first sector.
    root_lba: u64,
    /// Minor 0's extent, again: the table cannot exist without it.
    root: Extent,
}

impl MinorTable {
    /// The extent `minor` serves; `None` — which the driver answers `ENXIO` —
    /// for a negative minor, one past the table, an unused GPT slot, and a
    /// partition that is not block-aligned.
    pub fn extent(&self, minor: i32) -> Option<Extent> {
        let slot = usize::try_from(minor).ok()?;
        self.extents.get(slot).copied().flatten()
    }

    /// Minor 0's extent: the root partition. The root check runs against this.
    pub const fn root(&self) -> Extent {
        self.root
    }

    /// The root partition's number — its 1-based slot in the entry array — for
    /// the `root ok … part=<n>` marker. Its other minor is `1 +` this.
    pub const fn root_index(&self) -> u32 {
        self.root_index
    }

    /// The root partition's first 512-byte sector, for the `root ok … lba=<n>`
    /// marker.
    pub const fn root_lba(&self) -> u64 {
        self.root_lba
    }

    /// Decide what `req` asks of the device, or which errno to reply. The whole
    /// of a request's validation, in the order both block drivers share:
    ///
    /// 1. the minor has no extent → `ENXIO` (this driver's own check);
    /// 2. `bdev::validate_range` against that extent's block count → `EINVAL`
    ///    for a bad length, grant id or block, exactly as `drivers/memory`;
    /// 3. a zero-length request → [`Located::Empty`];
    /// 4. otherwise the sector, by [`Extent::sector`] — `EINVAL` if it does not
    ///    fit, which no extent [`scan`] built can cause.
    pub fn locate(&self, req: ReadRequest) -> Result<Located, i32> {
        let extent = self.extent(req.minor).ok_or(ENXIO)?;
        let (_, len) = validate_range(req, extent.blocks)?;
        if len == 0 {
            return Ok(Located::Empty);
        }
        let sector = extent.sector(req.block).ok_or(EINVAL)?;
        Ok(Located::Io { sector, len })
    }
}

/// Read the partition table out of `buf` — the disk from LBA 0, at least
/// [`gpt::TABLE_BYTES`] long; `main.rs` passes the [`TABLE_BUF_BYTES`] of blocks
/// `0..TABLE_BLOCKS` — for a device of `capacity` 512-byte sectors.
///
/// Refuses for the first of these that holds (spec P7):
///
/// 1. `capacity` is 0 or not a multiple of eight — [`RootFail::Geometry`];
/// 2. the header, the entry array's CRC or any entry was refused by
///    `driver-rt::gpt` — [`RootFail::Gpt`] (a short `buf` reads as `header
///    short` or `entries short`);
/// 3. more than one partition has the root type — [`RootFail::GptAmbiguous`];
/// 4. none has — [`RootFail::NoRootPartition`];
/// 5. the root partition is not block-aligned — [`RootFail::Align`].
///
/// Any *other* partition that is not block-aligned is not a refusal: it simply
/// has no minor.
pub fn scan(buf: &[u8], capacity: u64) -> Result<MinorTable, RootFail> {
    let disk_blocks = device_blocks(capacity).ok_or(RootFail::Geometry { capacity })?;

    // A buffer too short to hold either part reads as an empty slice, which
    // the parser refuses as short.
    let header_bytes = buf.get(gpt::HEADER_OFFSET..).unwrap_or(&[]);
    let entry_bytes = buf.get(gpt::ENTRIES_OFFSET..).unwrap_or(&[]);
    let header = gpt::parse_header(header_bytes, capacity).map_err(RootFail::Gpt)?;
    gpt::check_entries(entry_bytes, &header).map_err(RootFail::Gpt)?;

    // `find_type` decodes every entry, so a malformed one anywhere is reported
    // here, before anything is concluded from the entries.
    let lookup =
        gpt::find_type(entry_bytes, &header, &ROOT_PARTITION_TYPE_GUID).map_err(RootFail::Gpt)?;
    let (root_index, root_part) = match lookup {
        Lookup::Ambiguous => return Err(RootFail::GptAmbiguous),
        Lookup::Absent => return Err(RootFail::NoRootPartition),
        Lookup::Found { index, partition } => (index, partition),
    };
    let root = partition_extent(&root_part).ok_or(RootFail::Align)?;

    let mut extents = [None; MINORS];
    let whole_disk = Extent {
        first_block: 0,
        blocks: disk_blocks,
    };
    for (minor, extent) in [(BDEV_MINOR_ROOT, root), (MINOR_WHOLE_DISK, whole_disk)] {
        if let Some(slot) = minor_slot(&mut extents, i64::from(minor)) {
            *slot = Some(extent);
        }
    }
    for n in 1..=header.entries {
        // Cannot fail after `find_type` decoded the same entries; reported all
        // the same.
        let Some(p) = gpt::partition(entry_bytes, &header, n).map_err(RootFail::Gpt)? else {
            continue;
        };
        // Partition `n` is minor `1 + n`. An unaligned one stays `None`.
        let minor = i64::from(MINOR_WHOLE_DISK).checked_add(i64::from(n));
        if let Some(slot) = minor.and_then(|m| minor_slot(&mut extents, m)) {
            *slot = partition_extent(&p);
        }
    }

    Ok(MinorTable {
        extents,
        root_index,
        root_lba: root_part.first_lba,
        root,
    })
}

/// The table's slot for `minor`, if it has one.
fn minor_slot(extents: &mut [Option<Extent>; MINORS], minor: i64) -> Option<&mut Option<Extent>> {
    extents.get_mut(usize::try_from(minor).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_driver_rt::gpt::GptError;
    use minixrs_kernel_shared::callnr::BDEV_MAX_IO;
    use minixrs_kernel_shared::rootfs::{IMAGE_LABEL, ROOTFS_IMAGE_BLOCKS};

    /// The disk the slice's plan lays out: 75 776 sectors (37 MiB).
    const DISK: u64 = 75_776;
    /// The ESP: LBA 2048 (1 MiB), 69 632 sectors (34 MiB).
    const ESP_LBA: u64 = 2048;
    const ESP_SECTORS: u64 = 69_632;
    /// The root: LBA 71 680 (35 MiB), 2048 sectors (the 1 MiB root image).
    const ROOT_LBA: u64 = 71_680;
    const ROOT_SECTORS: u64 = 2048;

    /// The EFI System Partition type, C12A7328-F81F-11D2-BA4B-00A0C93EC93B, in
    /// its on-disk byte order.
    const ESP: [u8; 16] = [
        0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9,
        0x3B,
    ];
    const ROOT: [u8; 16] = ROOT_PARTITION_TYPE_GUID;
    /// Some other type.
    const OTHER: [u8; 16] = [0x07; 16];

    /// One used entry: the 1-based slot, the type, the first LBA, the sector
    /// count.
    type Part = (usize, [u8; 16], u64, u64);

    /// A [`TABLE_BUF_BYTES`] buffer holding a protective MBR's signature, a
    /// sealed primary header for a disk of `capacity` sectors declaring
    /// `slots` entries, and the entry array with `parts` filled in.
    fn table_with(capacity: u64, slots: usize, parts: &[Part]) -> Vec<u8> {
        let mut buf = vec![0u8; TABLE_BUF_BYTES];
        buf[510] = 0x55;
        buf[511] = 0xAA;

        for &(n, guid, first, sectors) in parts {
            let at = gpt::ENTRIES_OFFSET + (n - 1) * 128;
            let e = &mut buf[at..at + 128];
            e[0..16].copy_from_slice(&guid);
            e[16..32].copy_from_slice(&[0xEE; 16]); // unique GUID, not read
            e[32..40].copy_from_slice(&first.to_le_bytes());
            // The last LBA is inclusive.
            e[40..48].copy_from_slice(&(first + sectors - 1).to_le_bytes());
        }
        let entries_crc = gpt::crc32(&buf[gpt::ENTRIES_OFFSET..gpt::ENTRIES_OFFSET + slots * 128]);

        let h = &mut buf[gpt::HEADER_OFFSET..gpt::HEADER_OFFSET + 512];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes()); // revision 1.0
        h[12..16].copy_from_slice(&92u32.to_le_bytes()); // header size
        h[24..32].copy_from_slice(&1u64.to_le_bytes()); // MyLBA
        h[32..40].copy_from_slice(&(capacity - 1).to_le_bytes()); // AlternateLBA
        h[40..48].copy_from_slice(&34u64.to_le_bytes()); // FirstUsableLBA
        // The backup entries (32 sectors) and header (1) end the disk.
        h[48..56].copy_from_slice(&(capacity - 34).to_le_bytes()); // LastUsableLBA
        h[56..72].copy_from_slice(&[0xD1; 16]); // disk GUID
        h[72..80].copy_from_slice(&2u64.to_le_bytes()); // PartitionEntryLBA
        h[80..84].copy_from_slice(&u32::try_from(slots).unwrap().to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes()); // entry size
        h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
        let crc = gpt::crc32(&h[..92]); // over the header with its CRC field zero
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        buf
    }

    /// A 128-slot table on the plan's disk.
    fn table(parts: &[Part]) -> Vec<u8> {
        table_with(DISK, 128, parts)
    }

    /// The image `mkimage` writes for a disk boot: the ESP, then the root.
    fn disk_image() -> Vec<u8> {
        table(&[
            (1, ESP, ESP_LBA, ESP_SECTORS),
            (2, ROOT, ROOT_LBA, ROOT_SECTORS),
        ])
    }

    fn minors(buf: &[u8]) -> MinorTable {
        scan(buf, DISK).expect("an acceptable table")
    }

    fn req(minor: i32, len: i32, block: u64) -> ReadRequest {
        ReadRequest {
            minor,
            gid: 7,
            len,
            block,
        }
    }

    const fn ext(first_block: u64, blocks: u64) -> Extent {
        Extent {
            first_block,
            blocks,
        }
    }

    const fn part(first_lba: u64, last_lba: u64) -> Partition {
        Partition {
            type_guid: OTHER,
            first_lba,
            last_lba,
        }
    }

    // ---- constants ---------------------------------------------------------

    #[test]
    fn the_table_is_five_blocks() {
        // 34 sectors × 512 = 17 408 bytes; 17 408 / 4096 = 4.25, so 5 blocks,
        // 5 × 4096 = 20 480 bytes = 40 sectors (LBA 0..=39).
        assert_eq!(TABLE_BLOCKS, 5);
        assert_eq!(TABLE_BUF_BYTES, 20_480);
        assert_eq!(TABLE_BUF_BYTES / 512, 40);
    }

    #[test]
    fn there_are_130_minors() {
        assert_eq!(MINORS, 130);
        assert_eq!(MINOR_WHOLE_DISK, 1);
        assert_eq!(BDEV_MINOR_ROOT, 0);
    }

    // ---- extents -----------------------------------------------------------

    #[test]
    fn a_partition_becomes_blocks_by_exact_division() {
        // The ESP: 2048 / 8 = 256, 69 632 / 8 = 8704.
        assert_eq!(
            partition_extent(&part(ESP_LBA, ESP_LBA + ESP_SECTORS - 1)),
            Some(ext(256, 8704))
        );
        // The root: 71 680 / 8 = 8960, 2048 / 8 = 256.
        assert_eq!(
            partition_extent(&part(ROOT_LBA, ROOT_LBA + ROOT_SECTORS - 1)),
            Some(ext(8960, 256))
        );
        // One block, at the start of the disk.
        assert_eq!(partition_extent(&part(0, 7)), Some(ext(0, 1)));
        // The largest aligned partition a u64 can describe.
        assert_eq!(
            partition_extent(&part(8, u64::MAX)),
            Some(ext(1, u64::MAX / 8))
        );
    }

    #[test]
    fn a_partition_that_is_not_whole_blocks_has_no_extent() {
        // Starts one sector late; the length is still 2048.
        assert_eq!(partition_extent(&part(2049, 2049 + 2047)), None);
        // Starts on a block; one sector short, and one over.
        assert_eq!(partition_extent(&part(2048, 2048 + 2046)), None);
        assert_eq!(partition_extent(&part(2048, 2048 + 2048)), None);
        // Every misalignment of the start, with a whole-block length.
        for off in 1..8 {
            assert_eq!(
                partition_extent(&part(2048 + off, 2055 + off)),
                None,
                "{off}"
            );
        }
        // Every length that is not a whole block.
        for sectors in 1..8 {
            assert_eq!(
                partition_extent(&part(2048, 2047 + sectors)),
                None,
                "{sectors}"
            );
        }
    }

    #[test]
    fn a_partition_whose_range_cannot_be_counted_has_no_extent() {
        // Inverted (the parser never returns one).
        assert_eq!(partition_extent(&part(16, 8)), None);
        // 0..=u64::MAX is 2^64 sectors, which no u64 holds.
        assert_eq!(partition_extent(&part(0, u64::MAX)), None);
    }

    #[test]
    fn a_block_of_an_extent_is_its_first_block_plus_the_request() {
        let root = ext(8960, 256);
        // Block 0 is the partition's first sector, not sector 0.
        assert_eq!(root.sector(0), Some(71_680));
        assert_eq!(root.sector(1), Some(71_688));
        // The last block: (8960 + 255) × 8 = 73 720, whose last sector 73 727
        // is the partition's last.
        assert_eq!(root.sector(255), Some(73_720));
        assert_eq!(73_720 + 7, ROOT_LBA + ROOT_SECTORS - 1);
        // The whole disk: the identity extent.
        assert_eq!(ext(0, 9472).sector(0), Some(0));
        assert_eq!(ext(0, 9472).sector(9471), Some(75_768));
    }

    #[test]
    fn a_block_past_the_extent_has_no_sector() {
        let root = ext(8960, 256);
        // One past the last block would be the first block after the partition.
        assert_eq!(root.sector(256), None);
        assert_eq!(root.sector(u64::MAX), None);
        assert_eq!(ext(0, 0).sector(0), None);
    }

    #[test]
    fn extent_arithmetic_is_checked() {
        // `first_block + block` overflows: a wrapping add would name block 1.
        let high = ext(u64::MAX - 3, 10);
        assert_eq!(high.sector(5), None);
        assert_eq!(high.sector(4), None);
        // The sum fits but its sector does not: an extent ending at u64::MAX.
        let end = ext(u64::MAX - 10, 10);
        assert_eq!(end.sector(0), None);
        assert_eq!(end.sector(9), None);
        // A wrapping multiply would turn this into sector 0.
        assert_eq!(ext(1 << 61, 1).sector(0), None);
        // The last block whose sector fits, and the first that does not.
        assert_eq!(ext(u64::MAX / 8, 1).sector(0), Some(u64::MAX - 7));
        assert_eq!(ext(u64::MAX / 8 - 1, 2).sector(1), Some(u64::MAX - 7));
        assert_eq!(ext(u64::MAX / 8, 2).sector(1), None);
    }

    // ---- the minor table ---------------------------------------------------

    #[test]
    fn the_disk_image_has_the_root_twice_the_esp_and_the_whole_disk() {
        let t = minors(&disk_image());
        // Minor 0 and minor 1 + 2 are the same blocks.
        assert_eq!(t.extent(0), Some(ext(8960, 256)));
        assert_eq!(t.extent(3), Some(ext(8960, 256)));
        assert_eq!(t.extent(0), t.extent(3));
        assert_eq!(t.root(), ext(8960, 256));
        // The root image's own size.
        assert_eq!(t.root().blocks, u64::from(ROOTFS_IMAGE_BLOCKS));
        // Minor 2 is partition 1, the ESP.
        assert_eq!(t.extent(2), Some(ext(256, 8704)));
        // Minor 1 is the whole disk: 75 776 / 8 = 9472 blocks from block 0.
        assert_eq!(t.extent(1), Some(ext(0, 9472)));
        // Partition 3 does not exist.
        assert_eq!(t.extent(4), None);
        // Where the root is, for the marker.
        assert_eq!(t.root_index(), 2);
        assert_eq!(t.root_lba(), 71_680);
    }

    #[test]
    fn minors_outside_the_table_do_not_exist() {
        let t = minors(&disk_image());
        for minor in [-1, -2, i32::MIN, 130, 131, 256, i32::MAX] {
            assert_eq!(t.extent(minor), None, "{minor}");
        }
        // Inside the table, unused slots.
        for minor in 4..130 {
            assert_eq!(t.extent(minor), None, "{minor}");
        }
    }

    #[test]
    fn a_minor_is_one_plus_the_slot_not_a_count_of_used_entries() {
        // Slots 1 and 3 used, slot 2 empty.
        let t = minors(&table(&[
            (1, ESP, ESP_LBA, ESP_SECTORS),
            (3, ROOT, ROOT_LBA, ROOT_SECTORS),
        ]));
        assert_eq!(t.extent(2), Some(ext(256, 8704)));
        assert_eq!(t.extent(3), None);
        assert_eq!(t.extent(4), Some(ext(8960, 256)));
        assert_eq!(t.extent(0), t.extent(4));
        assert_eq!((t.root_index(), t.root_lba()), (3, 71_680));
    }

    #[test]
    fn the_last_slot_is_minor_129() {
        let t = minors(&table(&[(128, ROOT, ROOT_LBA, ROOT_SECTORS)]));
        assert_eq!(t.extent(129), Some(ext(8960, 256)));
        assert_eq!(t.extent(0), t.extent(129));
        assert_eq!(t.root_index(), 128);
        assert_eq!(t.extent(128), None);
        assert_eq!(t.extent(2), None);
    }

    #[test]
    fn the_root_is_found_by_type_wherever_it_is() {
        // The root first and the ESP second: position does not matter.
        let t = minors(&table(&[
            (1, ROOT, ROOT_LBA, ROOT_SECTORS),
            (2, ESP, ESP_LBA, ESP_SECTORS),
        ]));
        assert_eq!(t.extent(0), Some(ext(8960, 256)));
        assert_eq!(t.extent(2), Some(ext(8960, 256)));
        assert_eq!(t.extent(3), Some(ext(256, 8704)));
        assert_eq!((t.root_index(), t.root_lba()), (1, 71_680));
    }

    #[test]
    fn a_table_declaring_fewer_slots_has_fewer_minors() {
        let t = minors(&table_with(
            DISK,
            4,
            &[
                (1, ESP, ESP_LBA, ESP_SECTORS),
                (4, ROOT, ROOT_LBA, ROOT_SECTORS),
            ],
        ));
        assert_eq!(t.extent(5), Some(ext(8960, 256)));
        assert_eq!(t.extent(2), Some(ext(256, 8704)));
        for minor in 6..130 {
            assert_eq!(t.extent(minor), None, "{minor}");
        }
    }

    #[test]
    fn an_unaligned_partition_that_is_not_the_root_just_has_no_minor() {
        // Partition 3 starts one sector off a block; partition 4 is one sector
        // short of whole blocks. Both sit in the gap before the ESP.
        let t = minors(&table(&[
            (1, ESP, ESP_LBA, ESP_SECTORS),
            (2, ROOT, ROOT_LBA, ROOT_SECTORS),
            (3, OTHER, 41, 8),
            (4, OTHER, 64, 15),
            (5, OTHER, 80, 16),
        ]));
        assert_eq!(t.extent(4), None);
        assert_eq!(t.extent(5), None);
        // Its aligned neighbour, and everything else, is unaffected.
        assert_eq!(t.extent(6), Some(ext(10, 2)));
        assert_eq!(t.extent(0), Some(ext(8960, 256)));
        assert_eq!(t.extent(2), Some(ext(256, 8704)));
        assert_eq!(t.extent(1), Some(ext(0, 9472)));
    }

    // ---- refusals ----------------------------------------------------------

    #[test]
    fn an_unaligned_root_is_refused() {
        // One sector late (mkimage's mutation 10).
        let late = table(&[
            (1, ESP, ESP_LBA, ESP_SECTORS),
            (2, ROOT, ROOT_LBA + 1, ROOT_SECTORS),
        ]);
        assert_eq!(scan(&late, DISK), Err(RootFail::Align));
        // On a block, but not a whole number of them.
        let ragged = table(&[
            (1, ESP, ESP_LBA, ESP_SECTORS),
            (2, ROOT, ROOT_LBA, ROOT_SECTORS - 1),
        ]);
        assert_eq!(scan(&ragged, DISK), Err(RootFail::Align));
        assert_eq!(RootFail::Align.to_string(), "root FAIL align");
    }

    #[test]
    fn two_root_partitions_are_refused_not_ordered() {
        let two = table(&[(1, ROOT, ESP_LBA, 2048), (2, ROOT, ROOT_LBA, ROOT_SECTORS)]);
        let why = scan(&two, DISK).unwrap_err();
        assert_eq!(why, RootFail::GptAmbiguous);
        assert_eq!(why.to_string(), "root FAIL gpt ambiguous");
        // Ambiguity is reported before either one's alignment.
        let two_unaligned = table(&[(1, ROOT, 41, 9), (2, ROOT, ROOT_LBA + 1, 9)]);
        assert_eq!(scan(&two_unaligned, DISK), Err(RootFail::GptAmbiguous));
    }

    #[test]
    fn a_disk_with_no_root_partition_is_refused() {
        // The rootless image the ramdisk boot attaches: the ESP alone.
        let rootless = table(&[(1, ESP, ESP_LBA, ESP_SECTORS)]);
        let why = scan(&rootless, DISK).unwrap_err();
        assert_eq!(why, RootFail::NoRootPartition);
        assert_eq!(why.to_string(), "root FAIL no root partition");
        // A valid table with no partition at all.
        assert_eq!(scan(&table(&[]), DISK), Err(RootFail::NoRootPartition));
    }

    #[test]
    fn a_disk_with_no_gpt_is_refused_by_its_signature() {
        // A blank disk.
        let blank = vec![0u8; TABLE_BUF_BYTES];
        let why = scan(&blank, DISK).unwrap_err();
        assert_eq!(why, RootFail::Gpt(GptError::Signature));
        assert_eq!(why.to_string(), "root FAIL gpt signature");

        // 6.3's disk: a raw MinixFS image from LBA 0. No longer a root.
        let mut raw = vec![0u8; TABLE_BUF_BYTES];
        raw[..IMAGE_LABEL.len()].copy_from_slice(&IMAGE_LABEL);
        raw[1024..1040].fill(0x5A); // something where a superblock would be
        let why = scan(&raw, 2048).unwrap_err();
        assert_eq!(why, RootFail::Gpt(GptError::Signature));
        assert_eq!(why.to_string(), "root FAIL gpt signature");
    }

    #[test]
    fn a_damaged_table_is_refused_with_the_parsers_reason() {
        // A header byte flipped after sealing.
        let mut b = disk_image();
        b[gpt::HEADER_OFFSET + 40] ^= 1;
        let why = scan(&b, DISK).unwrap_err();
        assert_eq!(why, RootFail::Gpt(GptError::Crc));
        assert_eq!(why.to_string(), "root FAIL gpt crc");

        // An entry byte flipped after sealing.
        let mut b = disk_image();
        b[gpt::ENTRIES_OFFSET + 200] ^= 1;
        let why = scan(&b, DISK).unwrap_err();
        assert_eq!(why, RootFail::Gpt(GptError::EntriesCrc));
        assert_eq!(why.to_string(), "root FAIL gpt entries crc");

        // A table written for a larger disk than this device is.
        // (Its last usable LBA, 75 742, is past a 75 736-sector device.)
        let why = scan(&disk_image(), DISK - 40).unwrap_err();
        assert_eq!(why, RootFail::Gpt(GptError::UsableRange));
        assert_eq!(why.to_string(), "root FAIL gpt usable range");

        // A correctly sealed entry that leaves the usable area condemns the
        // table, even though the root itself is fine.
        let b = table(&[(1, ROOT, ROOT_LBA, ROOT_SECTORS), (2, OTHER, DISK - 40, 16)]);
        let why = scan(&b, DISK).unwrap_err();
        assert_eq!(why, RootFail::Gpt(GptError::EntryRange));
        assert_eq!(why.to_string(), "root FAIL gpt entry range");
    }

    #[test]
    fn a_table_fault_is_reported_before_what_the_entries_say() {
        // Two roots, and a bad entry CRC: the CRC is the verdict.
        let mut b = table(&[(1, ROOT, ESP_LBA, 2048), (2, ROOT, ROOT_LBA, ROOT_SECTORS)]);
        b[gpt::ENTRIES_OFFSET + 16] ^= 1;
        assert_eq!(scan(&b, DISK), Err(RootFail::Gpt(GptError::EntriesCrc)));
        // No root, and a bad header CRC.
        let mut b = table(&[(1, ESP, ESP_LBA, ESP_SECTORS)]);
        b[gpt::HEADER_OFFSET + 56] ^= 1;
        assert_eq!(scan(&b, DISK), Err(RootFail::Gpt(GptError::Crc)));
    }

    #[test]
    fn a_ragged_or_empty_device_is_refused_before_its_table_is_read() {
        // One sector over the plan's disk.
        let why = scan(&disk_image(), 75_777).unwrap_err();
        assert_eq!(why, RootFail::Geometry { capacity: 75_777 });
        assert_eq!(why.to_string(), "root FAIL geometry capacity=75777");
        // Geometry first, whatever the table is: a blank disk, a good one.
        let blank = vec![0u8; TABLE_BUF_BYTES];
        assert_eq!(
            scan(&blank, 75_777),
            Err(RootFail::Geometry { capacity: 75_777 })
        );
        assert_eq!(
            scan(&disk_image(), 0),
            Err(RootFail::Geometry { capacity: 0 })
        );
        assert_eq!(scan(&blank, 0), Err(RootFail::Geometry { capacity: 0 }));
        // A table that is good for 75 784 sectors, on a device one sector short.
        let b = table_with(75_784, 128, &[(1, ROOT, ROOT_LBA, ROOT_SECTORS)]);
        assert!(scan(&b, 75_784).is_ok());
        assert_eq!(
            scan(&b, 75_783),
            Err(RootFail::Geometry { capacity: 75_783 })
        );
    }

    #[test]
    fn a_short_buffer_is_a_refusal_not_a_panic() {
        let b = disk_image();
        for len in [0, 1, 511, 512, 1023] {
            assert_eq!(
                scan(&b[..len], DISK),
                Err(RootFail::Gpt(GptError::HeaderShort)),
                "{len}"
            );
        }
        for len in [1024, 4096, gpt::TABLE_BYTES - 1] {
            assert_eq!(
                scan(&b[..len], DISK),
                Err(RootFail::Gpt(GptError::EntriesShort)),
                "{len}"
            );
        }
        // Exactly the table, without the fifth block's padding, is enough.
        assert_eq!(scan(&b[..gpt::TABLE_BYTES], DISK), scan(&b, DISK));
        assert!(scan(&b[..gpt::TABLE_BYTES], DISK).is_ok());
    }

    #[test]
    fn bytes_after_the_table_are_not_read() {
        let mut b = disk_image();
        b[gpt::TABLE_BYTES..].fill(0xA5);
        assert_eq!(scan(&b, DISK), scan(&disk_image(), DISK));
    }

    // ---- requests ----------------------------------------------------------

    #[test]
    fn a_request_is_located_inside_its_minors_extent() {
        let t = minors(&disk_image());
        let io = |sector, len| Ok(Located::Io { sector, len });
        // Minor 0, block 0: the root partition's first sector — not sector 0
        // (mutation 1).
        assert_eq!(t.locate(req(0, 4096, 0)), io(71_680, 4096));
        // The same block through the root's other minor.
        assert_eq!(t.locate(req(3, 4096, 0)), io(71_680, 4096));
        // The root's last block.
        assert_eq!(t.locate(req(0, 4096, 255)), io(73_720, 4096));
        // The ESP: block 256 of the disk.
        assert_eq!(t.locate(req(2, 4096, 0)), io(2048, 4096));
        assert_eq!(t.locate(req(2, 4096, 8703)), io(71_672, 4096));
        // The whole disk: block 0 is sector 0, the protective MBR (mutation 11).
        assert_eq!(t.locate(req(1, 512, 0)), io(0, 512));
        assert_eq!(t.locate(req(1, 4096, 9471)), io(75_768, 4096));
        // The whole disk's block 8960 is the root's block 0.
        assert_eq!(t.locate(req(1, 4096, 8960)), t.locate(req(0, 4096, 0)));
        // A partial block keeps its length.
        assert_eq!(t.locate(req(0, 1, 7)), io(71_736, 1));
    }

    #[test]
    fn a_request_cannot_reach_past_its_own_extent() {
        let t = minors(&disk_image());
        // One past the root's last block is still on the disk (the backup
        // table follows) — and refused.
        assert_eq!(t.locate(req(0, 4096, 256)), Err(EINVAL));
        assert_eq!(t.locate(req(3, 4096, 256)), Err(EINVAL));
        // One past the ESP's last block is the root's first.
        assert_eq!(t.locate(req(2, 4096, 8704)), Err(EINVAL));
        // One past the disk.
        assert_eq!(t.locate(req(1, 4096, 9472)), Err(EINVAL));
        assert_eq!(t.locate(req(0, 4096, u64::MAX)), Err(EINVAL));
        // The root's blocks are reachable as whole-disk blocks, but a root
        // block number that large is not a root block.
        assert_eq!(t.locate(req(0, 4096, 8960)), Err(EINVAL));
    }

    #[test]
    fn a_missing_minor_is_enxio_before_anything_else_is_looked_at() {
        let t = minors(&disk_image());
        for minor in [-1, 4, 129, 130, i32::MAX, i32::MIN] {
            assert_eq!(t.locate(req(minor, 4096, 0)), Err(ENXIO), "{minor}");
            // ENXIO, not EINVAL, when the rest of the request is wrong too.
            assert_eq!(t.locate(req(minor, -1, 0)), Err(ENXIO), "{minor}");
            assert_eq!(t.locate(req(minor, 4096, u64::MAX)), Err(ENXIO), "{minor}");
            assert_eq!(t.locate(req(minor, 0, 0)), Err(ENXIO), "{minor}");
            let bad_grant = ReadRequest {
                gid: -1,
                ..req(minor, 4096, 0)
            };
            assert_eq!(t.locate(bad_grant), Err(ENXIO), "{minor}");
        }
        // An unaligned partition's minor is as absent as an unused slot's.
        let t = minors(&table(&[
            (1, ROOT, ROOT_LBA, ROOT_SECTORS),
            (2, OTHER, 41, 8),
        ]));
        assert_eq!(t.locate(req(3, 4096, 0)), Err(ENXIO));
    }

    #[test]
    fn the_shared_refusals_apply_on_every_minor() {
        let t = minors(&disk_image());
        for minor in [0, 1, 2, 3] {
            assert_eq!(t.locate(req(minor, -1, 0)), Err(EINVAL), "{minor}");
            let over = i32::try_from(BDEV_MAX_IO).unwrap() + 1;
            assert_eq!(t.locate(req(minor, over, 0)), Err(EINVAL), "{minor}");
            let bad_grant = ReadRequest {
                gid: -1,
                ..req(minor, 4096, 0)
            };
            assert_eq!(t.locate(bad_grant), Err(EINVAL), "{minor}");
            // A zero-length request with a bad grant is still the error it is.
            let bad_grant_empty = ReadRequest {
                gid: -1,
                ..req(minor, 0, 0)
            };
            assert_eq!(t.locate(bad_grant_empty), Err(EINVAL), "{minor}");
        }
    }

    #[test]
    fn a_zero_length_request_touches_nothing() {
        let t = minors(&disk_image());
        assert_eq!(t.locate(req(0, 0, 0)), Ok(Located::Empty));
        assert_eq!(t.locate(req(1, 0, 0)), Ok(Located::Empty));
        // As on `drivers/memory`: the block of an empty request is not checked.
        assert_eq!(t.locate(req(0, 0, u64::MAX)), Ok(Located::Empty));
    }

    #[test]
    fn locate_agrees_with_the_shared_range_check() {
        // On minor 0 `locate` accepts exactly what `validate_range` accepts for
        // the root's block count, so the refusal order both drivers share is
        // the shared function's.
        let t = minors(&disk_image());
        let blocks = t.root().blocks;
        for len in [-1, 0, 1, 4096, 4097] {
            for block in [0, 1, 255, 256, 8960, u64::MAX] {
                for gid in [-1, 0, 7] {
                    let r = ReadRequest {
                        minor: 0,
                        gid,
                        len,
                        block,
                    };
                    let want = validate_range(r, blocks).map(|(off, n)| {
                        if n == 0 {
                            Located::Empty
                        } else {
                            Located::Io {
                                sector: 71_680 + off / 512,
                                len: n,
                            }
                        }
                    });
                    assert_eq!(t.locate(r), want, "{r:?}");
                }
            }
        }
    }
}
