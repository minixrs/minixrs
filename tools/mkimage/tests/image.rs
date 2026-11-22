// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! What a built image must be, checked by readers that are not the writer.
//!
//! This is an integration test on purpose: it can reach `minixrs_mkimage`'s
//! public API only, so nothing here can borrow the writer's own helpers for
//! finding things. Three readers stand in for the three real ones:
//!
//! - the FAT32 reader below ([`Volume`]) parses the boot sector's BPB, walks the
//!   FAT chains and the directory entries, and reassembles long names — the way
//!   edk2's FAT driver and Limine will;
//! - `minixrs_driver_rt::gpt` is the parser `virtio-blk` will use, a
//!   dev-dependency that shares no code with the writer;
//! - a few fields are read straight from the bytes at the offsets the UEFI
//!   specification gives.

use minixrs_driver_rt::gpt as parser;
use minixrs_kernel_shared::rootfs::{
    HDR_LABEL_OFF, IMAGE_LABEL, IMAGE_LABEL_LEN, ROOT_PARTITION_TYPE_GUID, ROOTFS_IMAGE_BYTES,
};
use minixrs_mkimage::{Error, Spec, build_image, layout, write_sparse};

const SECTOR: usize = 512;

fn le16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(b[off..off + 2].try_into().unwrap())
}

fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

fn le64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

fn sector(image: &[u8], lba: u64) -> &[u8] {
    let start = usize::try_from(lba).unwrap() * SECTOR;
    &image[start..start + SECTOR]
}

// ---------------------------------------------------------------------------
// Inputs.
// ---------------------------------------------------------------------------

/// `len` bytes that differ from sector to sector and from seed to seed, so a
/// cluster written to the wrong place or twice shows up as a mismatch.
fn pattern(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 24) as u8
        })
        .collect()
}

/// A root image as `mkimage` accepts one: the right length, the label where
/// `kernel-shared` says it goes, and a body no two blocks of which are alike.
fn root_image() -> Vec<u8> {
    let mut image = pattern(ROOTFS_IMAGE_BYTES, 0x6_4);
    image[HDR_LABEL_OFF..HDR_LABEL_OFF + IMAGE_LABEL_LEN].copy_from_slice(&IMAGE_LABEL);
    image
}

const KERNEL_LEN: usize = 3 * 1024 * 1024 + 123;

/// The three files a real ESP holds, at their real paths, plus an empty one.
fn boot_files() -> Vec<(String, Vec<u8>)> {
    vec![
        ("EFI/BOOT/BOOTAA64.EFI".into(), pattern(90_000, 1)),
        (
            "limine.conf".into(),
            b"timeout: 0\n/minix.rs\n    protocol: limine\n".to_vec(),
        ),
        ("boot/kernel".into(), pattern(KERNEL_LEN, 2)),
        ("boot/empty".into(), Vec::new()),
    ]
}

fn spec(with_root: bool) -> Spec {
    Spec {
        esp_files: boot_files(),
        root: with_root.then(root_image),
    }
}

fn esp_only(files: &[(&str, Vec<u8>)]) -> Spec {
    Spec {
        esp_files: files
            .iter()
            .map(|(p, b)| ((*p).to_string(), b.clone()))
            .collect(),
        root: None,
    }
}

fn esp(image: &[u8]) -> &[u8] {
    let start = usize::try_from(layout::ESP_FIRST_LBA).unwrap() * SECTOR;
    let len = usize::try_from(layout::ESP_SECTORS).unwrap() * SECTOR;
    &image[start..start + len]
}

// ---------------------------------------------------------------------------
// An independent, minimal FAT32 reader.
// ---------------------------------------------------------------------------

const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_LONG_NAME: u8 = 0x0F;
const END_OF_CHAIN: u32 = 0x0FFF_FFF8;

#[derive(Clone, Debug)]
struct DirEntry {
    long_name: Option<String>,
    short: [u8; 11],
    attr: u8,
    first_cluster: u32,
    size: u32,
}

impl DirEntry {
    /// `BASE.EXT` as the 11-byte short field spells it.
    fn short_name(&self) -> String {
        let base = String::from_utf8_lossy(&self.short[..8])
            .trim_end()
            .to_string();
        let ext = String::from_utf8_lossy(&self.short[8..])
            .trim_end()
            .to_string();
        if ext.is_empty() {
            base
        } else {
            format!("{base}.{ext}")
        }
    }

    /// The name a lookup matches: the long name if the entry has one.
    fn name(&self) -> String {
        self.long_name.clone().unwrap_or_else(|| self.short_name())
    }

    fn is_dir(&self) -> bool {
        self.attr & ATTR_DIRECTORY != 0
    }
}

/// The checksum a long-name entry carries of its short entry's 11 name bytes:
/// rotate right by one, add the next byte.
fn short_name_checksum(short: &[u8; 11]) -> u8 {
    short
        .iter()
        .fold(0u8, |sum, &c| sum.rotate_right(1).wrapping_add(c))
}

struct Volume<'a> {
    bytes: &'a [u8],
    bytes_per_sector: usize,
    sectors_per_cluster: usize,
    reserved_sectors: usize,
    fats: usize,
    fat_sectors: usize,
    total_sectors: usize,
    root_cluster: u32,
    fsinfo_sector: usize,
    backup_boot_sector: usize,
    first_data_sector: usize,
    clusters: u32,
}

impl<'a> Volume<'a> {
    /// Read the geometry from the BPB — and from nothing else.
    fn open(bytes: &'a [u8]) -> Self {
        assert_eq!(&bytes[510..512], &[0x55, 0xAA], "boot sector signature");
        let bytes_per_sector = usize::from(le16(bytes, 11));
        let sectors_per_cluster = usize::from(bytes[13]);
        let reserved_sectors = usize::from(le16(bytes, 14));
        let fats = usize::from(bytes[16]);
        assert_eq!(le16(bytes, 17), 0, "FAT32 has no fixed root directory");
        assert_eq!(le16(bytes, 19), 0, "FAT32 uses the 32-bit total");
        assert_eq!(le16(bytes, 22), 0, "FAT32 uses the 32-bit FAT size");
        let total_sectors = le32(bytes, 32) as usize;
        let fat_sectors = le32(bytes, 36) as usize;
        let first_data_sector = reserved_sectors + fats * fat_sectors;
        let clusters = ((total_sectors - first_data_sector) / sectors_per_cluster) as u32;
        Self {
            bytes,
            bytes_per_sector,
            sectors_per_cluster,
            reserved_sectors,
            fats,
            fat_sectors,
            total_sectors,
            root_cluster: le32(bytes, 44),
            fsinfo_sector: usize::from(le16(bytes, 48)),
            backup_boot_sector: usize::from(le16(bytes, 50)),
            first_data_sector,
            clusters,
        }
    }

    fn sector(&self, n: usize) -> &'a [u8] {
        &self.bytes[n * self.bytes_per_sector..(n + 1) * self.bytes_per_sector]
    }

    fn fat(&self, copy: usize) -> &'a [u8] {
        let start = (self.reserved_sectors + copy * self.fat_sectors) * self.bytes_per_sector;
        &self.bytes[start..start + self.fat_sectors * self.bytes_per_sector]
    }

    /// FAT entry `n` of the first FAT, with the four reserved bits masked off.
    fn fat_entry(&self, n: u32) -> u32 {
        le32(self.fat(0), n as usize * 4) & 0x0FFF_FFFF
    }

    fn cluster_bytes(&self) -> usize {
        self.sectors_per_cluster * self.bytes_per_sector
    }

    fn cluster(&self, n: u32) -> &'a [u8] {
        assert!(
            n >= 2 && n < self.clusters + 2,
            "cluster {n} is outside the volume"
        );
        let first_sector = self.first_data_sector + (n as usize - 2) * self.sectors_per_cluster;
        let start = first_sector * self.bytes_per_sector;
        &self.bytes[start..start + self.cluster_bytes()]
    }

    /// Every cluster of the chain starting at `first`, one FAT entry at a time.
    fn chain(&self, first: u32) -> Vec<u32> {
        let mut chain = Vec::new();
        let mut at = first;
        loop {
            assert!(
                at >= 2 && at < self.clusters + 2,
                "chain from {first} reaches cluster {at}"
            );
            chain.push(at);
            assert!(
                chain.len() <= self.clusters as usize,
                "chain from {first} loops"
            );
            let next = self.fat_entry(at);
            assert_ne!(next, 0, "chain from {first} runs into a free cluster");
            if next >= END_OF_CHAIN {
                return chain;
            }
            at = next;
        }
    }

    fn read_chain(&self, first: u32) -> Vec<u8> {
        self.chain(first)
            .into_iter()
            .flat_map(|c| self.cluster(c).iter().copied())
            .collect()
    }

    /// Decode the directory whose first cluster is `first`, verifying every
    /// long-name run against the short entry it precedes.
    fn read_dir(&self, first: u32) -> Vec<DirEntry> {
        let data = self.read_chain(first);
        let mut entries = Vec::new();
        // (sequence byte, checksum, the 13 UTF-16 units), in on-disk order.
        let mut pending: Vec<(u8, u8, [u16; 13])> = Vec::new();
        let mut ended_at = None;
        for (i, raw) in data.as_chunks::<32>().0.iter().enumerate() {
            if raw[0] == 0x00 {
                ended_at = Some(i);
                break;
            }
            assert_ne!(raw[0], 0xE5, "a fresh volume has no deleted entries");
            let attr = raw[11];
            if attr == ATTR_LONG_NAME {
                assert_eq!(raw[12], 0, "long-name entry type");
                assert_eq!(le16(raw, 26), 0, "long-name entry cluster field");
                let mut units = [0u16; 13];
                let offsets = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
                for (unit, off) in units.iter_mut().zip(offsets) {
                    *unit = le16(raw, off);
                }
                pending.push((raw[0], raw[13], units));
                continue;
            }
            let short: [u8; 11] = raw[..11].try_into().unwrap();
            let long_name = if pending.is_empty() {
                None
            } else {
                let n = pending.len();
                assert_eq!(
                    pending[0].0,
                    0x40 | n as u8,
                    "the first long-name entry on disk is the last of the name"
                );
                for (i, p) in pending.iter().enumerate().skip(1) {
                    assert_eq!(usize::from(p.0), n - i, "long-name sequence number");
                }
                let sum = short_name_checksum(&short);
                for p in &pending {
                    assert_eq!(p.1, sum, "long-name checksum of {short:?}");
                }
                let units: Vec<u16> = pending.iter().rev().flat_map(|p| p.2).collect();
                let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
                assert!(
                    units[end..].iter().skip(1).all(|&u| u == 0xFFFF),
                    "after the terminator a long name is padded with 0xFFFF"
                );
                assert!(end > (n - 1) * 13, "a long-name entry holds no characters");
                Some(String::from_utf16(&units[..end]).unwrap())
            };
            pending.clear();
            entries.push(DirEntry {
                long_name,
                short,
                attr,
                first_cluster: u32::from(le16(raw, 20)) << 16 | u32::from(le16(raw, 26)),
                size: le32(raw, 28),
            });
        }
        assert!(pending.is_empty(), "a long name with no short entry");
        if let Some(end) = ended_at {
            assert!(
                data[end * 32..].iter().all(|&b| b == 0),
                "bytes after the directory's end marker"
            );
        }
        entries
    }

    /// The entries a lookup can match: no volume label, no `.`/`..`.
    fn children(&self, dir_cluster: u32) -> Vec<DirEntry> {
        self.read_dir(dir_cluster)
            .into_iter()
            .filter(|e| e.attr & ATTR_VOLUME_ID == 0 && e.short[0] != b'.')
            .collect()
    }

    /// Walk `path` from the root directory, one component at a time.
    fn find(&self, path: &str) -> DirEntry {
        let mut dir = self.root_cluster;
        let mut found = None;
        let mut components = path.split('/').peekable();
        while let Some(component) = components.next() {
            let entry = self
                .children(dir)
                .into_iter()
                .find(|e| e.name() == component)
                .unwrap_or_else(|| panic!("{component:?} of {path:?} not found"));
            if components.peek().is_some() {
                assert!(
                    entry.is_dir(),
                    "{component:?} of {path:?} is not a directory"
                );
                dir = entry.first_cluster;
            }
            found = Some(entry);
        }
        found.unwrap()
    }

    /// The contents of the file at `path`, and its cluster chain.
    fn read_file(&self, path: &str) -> (Vec<u8>, Vec<u32>) {
        let entry = self.find(path);
        assert!(!entry.is_dir(), "{path:?} is a directory");
        let size = entry.size as usize;
        if size == 0 {
            assert_eq!(entry.first_cluster, 0, "an empty file owns no cluster");
            return (Vec::new(), Vec::new());
        }
        let chain = self.chain(entry.first_cluster);
        assert_eq!(
            chain.len(),
            size.div_ceil(self.cluster_bytes()),
            "{path:?}: chain length against size"
        );
        let mut bytes = self.read_chain(entry.first_cluster);
        bytes.truncate(size);
        (bytes, chain)
    }

    /// Clusters reachable from the directory at `dir_cluster`, itself included.
    fn reachable(&self, dir_cluster: u32) -> usize {
        let mut used = self.chain(dir_cluster).len();
        for entry in self.children(dir_cluster) {
            if entry.is_dir() {
                used += self.reachable(entry.first_cluster);
            } else if entry.size != 0 {
                used += self.chain(entry.first_cluster).len();
            }
        }
        used
    }
}

// ---------------------------------------------------------------------------
// Layout.
// ---------------------------------------------------------------------------

/// The literal numbers, once. The boot-marker files carry `lba=71680` and
/// `capacity=75776`; a constant that drifted fails here rather than in a boot.
#[test]
fn the_layout_numbers_are_the_ones_the_boot_markers_carry() {
    assert_eq!(layout::SECTOR_SIZE, 512);
    assert_eq!(layout::ESP_FIRST_LBA, 2048);
    assert_eq!(layout::ESP_SECTORS, 69_632);
    assert_eq!(layout::ROOT_FIRST_LBA, 71_680);
    assert_eq!(layout::ROOT_SECTORS, 2048);
    assert_eq!(layout::DISK_SECTORS, 75_776);
    assert_eq!(layout::FIRST_USABLE_LBA, 34);
    assert_eq!(layout::LAST_USABLE_LBA, 75_776 - 34);
    assert_eq!(layout::BACKUP_ENTRIES_LBA, 75_743);
    assert_eq!(layout::BACKUP_HEADER_LBA, 75_775);
    assert_eq!(layout::DISK_BYTES, 37 * 1024 * 1024);
}

#[test]
fn partitions_are_aligned_and_the_root_is_exactly_the_root_image() {
    const MIB: u64 = 1024 * 1024;
    for first_lba in [layout::ESP_FIRST_LBA, layout::ROOT_FIRST_LBA] {
        let byte = first_lba * 512;
        assert_eq!(byte % MIB, 0, "LBA {first_lba} is not on a 1 MiB boundary");
        assert_eq!(byte % 4096, 0, "LBA {first_lba} is not on a block boundary");
    }
    assert_eq!(layout::ROOT_SECTORS * 512, ROOTFS_IMAGE_BYTES as u64);
    assert_eq!(layout::DISK_SECTORS % 8, 0);
    assert_eq!(
        (layout::ESP_FIRST_LBA + layout::ESP_SECTORS).min(layout::ROOT_FIRST_LBA),
        layout::ESP_FIRST_LBA + layout::ESP_SECTORS,
        "the ESP runs into the root"
    );

    for with_root in [true, false] {
        let image = build_image(&spec(with_root)).unwrap();
        assert_eq!(image.len() as u64, layout::DISK_SECTORS * 512);
        assert_eq!(image.len(), layout::DISK_BYTES);
    }
}

#[test]
fn the_esp_has_enough_clusters_to_be_fat32() {
    assert_eq!(
        layout::ESP_GEOMETRY.clusters.max(65_525),
        layout::ESP_GEOMETRY.clusters
    );
    // And the reader, from the BPB alone, counts the same.
    let image = build_image(&spec(true)).unwrap();
    let volume = Volume::open(esp(&image));
    assert!(volume.clusters >= 65_525, "{} clusters", volume.clusters);
    assert_eq!(volume.clusters, layout::ESP_GEOMETRY.clusters);
    assert_eq!(volume.fat_sectors as u32, layout::ESP_GEOMETRY.fat_sectors);
    assert_eq!(
        volume.first_data_sector as u32,
        layout::ESP_GEOMETRY.first_data_sector
    );
}

#[test]
fn the_protective_mbr() {
    let image = build_image(&spec(true)).unwrap();
    let mbr = sector(&image, 0);
    assert_eq!(&mbr[510..], &[0x55, 0xAA]);
    assert!(mbr[..446].iter().all(|&b| b == 0), "boot code area");

    let entry = &mbr[446..462];
    assert_eq!(entry[0], 0x00, "not bootable");
    assert_eq!(&entry[1..4], &[0x00, 0x02, 0x00], "starting CHS of LBA 1");
    assert_eq!(entry[4], 0xEE, "GPT protective type");
    assert_eq!(&entry[5..8], &[0xFF, 0xFF, 0xFF], "ending CHS");
    assert_eq!(le32(entry, 8), 1, "starts at LBA 1");
    assert_eq!(u64::from(le32(entry, 12)), layout::DISK_SECTORS - 1);
    assert!(
        mbr[462..510].iter().all(|&b| b == 0),
        "three unused entries"
    );
}

/// A header's fields, read at the offsets UEFI 2.x table 5-5 gives.
struct RawHeader {
    my_lba: u64,
    alternate_lba: u64,
    first_usable: u64,
    last_usable: u64,
    disk_guid: [u8; 16],
    entries_lba: u64,
    entry_count: u32,
    entry_size: u32,
    entries_crc: u32,
}

fn raw_header(image: &[u8], lba: u64) -> RawHeader {
    let s = sector(image, lba);
    assert_eq!(&s[..8], b"EFI PART");
    assert_eq!(le32(s, 8), 0x0001_0000, "revision 1.0");
    assert_eq!(le32(s, 12), 92, "header size");
    assert_eq!(le32(s, 20), 0, "reserved");
    // The header CRC covers the 92 bytes with its own field zeroed. Computed
    // with the parser's CRC-32, not the writer's.
    let mut covered = s[..92].to_vec();
    covered[16..20].fill(0);
    assert_eq!(le32(s, 16), parser::crc32(&covered), "header CRC at {lba}");
    assert!(s[92..].iter().all(|&b| b == 0), "rest of the header sector");
    RawHeader {
        my_lba: le64(s, 24),
        alternate_lba: le64(s, 32),
        first_usable: le64(s, 40),
        last_usable: le64(s, 48),
        disk_guid: s[56..72].try_into().unwrap(),
        entries_lba: le64(s, 72),
        entry_count: le32(s, 80),
        entry_size: le32(s, 84),
        entries_crc: le32(s, 88),
    }
}

#[test]
fn both_gpt_copies_are_valid_and_point_at_each_other() {
    for with_root in [true, false] {
        let image = build_image(&spec(with_root)).unwrap();
        let last_lba = (image.len() / SECTOR) as u64 - 1;

        let primary = raw_header(&image, 1);
        let backup = raw_header(&image, last_lba);

        assert_eq!(primary.my_lba, 1);
        assert_eq!(primary.alternate_lba, last_lba);
        assert_eq!(backup.my_lba, last_lba);
        assert_eq!(backup.alternate_lba, 1);

        assert_eq!(primary.entries_lba, 2);
        assert_eq!(backup.entries_lba, last_lba - 32);

        for h in [&primary, &backup] {
            assert_eq!(h.first_usable, 34);
            assert_eq!(h.last_usable, last_lba - 33);
            assert_eq!(h.entry_count, 128);
            assert_eq!(h.entry_size, 128);
            // 01C87657-FA93-4221-9F12-011CE0C42E0F, mixed-endian.
            assert_eq!(
                h.disk_guid,
                [
                    0x57, 0x76, 0xC8, 0x01, 0x93, 0xFA, 0x21, 0x42, 0x9F, 0x12, 0x01, 0x1C, 0xE0,
                    0xC4, 0x2E, 0x0F
                ]
            );
        }

        let array_bytes = 128 * 128;
        let primary_array = &image[2 * SECTOR..2 * SECTOR + array_bytes];
        let backup_start = usize::try_from(backup.entries_lba).unwrap() * SECTOR;
        let backup_array = &image[backup_start..backup_start + array_bytes];
        assert_eq!(primary_array, backup_array, "the two entry arrays differ");
        assert_eq!(primary.entries_crc, parser::crc32(primary_array));
        assert_eq!(backup.entries_crc, primary.entries_crc);
        // The backup array ends where the backup header begins.
        assert_eq!(backup_start + array_bytes, last_lba as usize * SECTOR);
    }
}

#[test]
fn the_partition_entries_field_by_field() {
    let image = build_image(&spec(true)).unwrap();
    let entries = &image[2 * SECTOR..];

    let esp_entry = &entries[..128];
    // C12A7328-F81F-11D2-BA4B-00A0C93EC93B as every ESP on earth stores it.
    assert_eq!(
        &esp_entry[..16],
        &[
            0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E,
            0xC9, 0x3B
        ]
    );
    // 4BEF5E26-A1E0-4D22-96DF-33C6AA1F576C.
    assert_eq!(
        &esp_entry[16..32],
        &[
            0x26, 0x5E, 0xEF, 0x4B, 0xE0, 0xA1, 0x22, 0x4D, 0x96, 0xDF, 0x33, 0xC6, 0xAA, 0x1F,
            0x57, 0x6C
        ]
    );
    assert_eq!(le64(esp_entry, 32), 2048);
    assert_eq!(
        le64(esp_entry, 40),
        2048 + 69_632 - 1,
        "last LBA, inclusive"
    );
    assert_eq!(le64(esp_entry, 48), 0, "attributes");
    assert_eq!(utf16_name(&esp_entry[56..128]), "ESP");

    let root_entry = &entries[128..256];
    assert_eq!(&root_entry[..16], &ROOT_PARTITION_TYPE_GUID);
    // 15C269EB-0329-4BEE-B037-7576B91F3725.
    assert_eq!(
        &root_entry[16..32],
        &[
            0xEB, 0x69, 0xC2, 0x15, 0x29, 0x03, 0xEE, 0x4B, 0xB0, 0x37, 0x75, 0x76, 0xB9, 0x1F,
            0x37, 0x25
        ]
    );
    assert_eq!(le64(root_entry, 32), 71_680);
    assert_eq!(le64(root_entry, 40), 71_680 + 2048 - 1);
    assert_eq!(le64(root_entry, 48), 0);
    assert_eq!(utf16_name(&root_entry[56..128]), "minix.rs root");

    assert!(
        entries[256..128 * 128].iter().all(|&b| b == 0),
        "126 unused entries"
    );
}

fn utf16_name(field: &[u8]) -> String {
    let units: Vec<u16> = field
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16(&units).unwrap()
}

// ---------------------------------------------------------------------------
// The driver's parser.
// ---------------------------------------------------------------------------

fn parse(image: &[u8]) -> (parser::Header, &[u8]) {
    let disk_sectors = (image.len() / SECTOR) as u64;
    let header = parser::parse_header(
        &image[parser::HEADER_OFFSET..parser::HEADER_OFFSET + SECTOR],
        disk_sectors,
    )
    .expect("the driver's parser refuses the header");
    let entries = &image[parser::ENTRIES_OFFSET..parser::ENTRIES_OFFSET + header.entries_bytes];
    parser::check_entries(entries, &header).expect("the driver's parser refuses the entry CRC");
    (header, entries)
}

#[test]
fn the_drivers_parser_finds_the_root_by_type_and_its_bytes_are_the_input() {
    let image = build_image(&spec(true)).unwrap();
    let (header, entries) = parse(&image);
    assert_eq!(header.entries, 128);
    assert_eq!(header.first_usable, 34);
    assert_eq!(header.last_usable, 75_776 - 34);

    let lookup = parser::find_type(entries, &header, &ROOT_PARTITION_TYPE_GUID).unwrap();
    let parser::Lookup::Found { index, partition } = lookup else {
        panic!("root partition: {lookup:?}");
    };
    assert_eq!(index, 2);
    assert_eq!(index, layout::ROOT_PARTITION_NUMBER);
    assert_eq!(partition.first_lba, 71_680);
    assert_eq!(partition.sectors(), Some(2048));

    // The bytes the driver would serve as minor 0 are the input, exactly — and
    // they are found through the table, not through this crate's constants.
    let start = usize::try_from(partition.first_lba).unwrap() * SECTOR;
    let len = usize::try_from(partition.sectors().unwrap()).unwrap() * SECTOR;
    assert!(
        image[start..start + len] == root_image()[..],
        "the root partition's bytes are not the root image"
    );
    assert_eq!(len, ROOTFS_IMAGE_BYTES);

    let esp_partition = parser::partition(entries, &header, 1).unwrap().unwrap();
    assert_eq!(
        esp_partition.type_guid,
        [
            0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E,
            0xC9, 0x3B
        ]
    );
    assert_eq!(esp_partition.first_lba, 2048);
    assert_eq!(esp_partition.sectors(), Some(69_632));
    assert_eq!(layout::ESP_PARTITION_NUMBER, 1);

    for n in 3..=128 {
        assert_eq!(parser::partition(entries, &header, n), Ok(None));
    }
}

#[test]
fn a_rootless_image_has_no_root_partition_and_the_same_size() {
    let image = build_image(&spec(false)).unwrap();
    assert_eq!(image.len(), build_image(&spec(true)).unwrap().len());

    let (header, entries) = parse(&image);
    assert_eq!(
        parser::find_type(entries, &header, &ROOT_PARTITION_TYPE_GUID),
        Ok(parser::Lookup::Absent)
    );
    assert_eq!(parser::partition(entries, &header, 2), Ok(None));
    assert!(parser::partition(entries, &header, 1).unwrap().is_some());

    // The whole slot is zero, not just its type, and the root's range is unused.
    assert!(
        image[2 * SECTOR + 128..2 * SECTOR + 256]
            .iter()
            .all(|&b| b == 0)
    );
    let start = usize::try_from(layout::ROOT_FIRST_LBA).unwrap() * SECTOR;
    assert!(
        image[start..start + ROOTFS_IMAGE_BYTES]
            .iter()
            .all(|&b| b == 0)
    );
}

#[test]
fn the_esp_is_the_same_with_and_without_a_root() {
    let with = build_image(&spec(true)).unwrap();
    let without = build_image(&spec(false)).unwrap();
    assert!(esp(&with) == esp(&without));
}

// ---------------------------------------------------------------------------
// FAT32.
// ---------------------------------------------------------------------------

#[test]
fn every_file_reads_back_by_its_full_path_and_long_name() {
    let image = build_image(&spec(true)).unwrap();
    let volume = Volume::open(esp(&image));
    for (path, bytes) in boot_files() {
        let (read, _) = volume.read_file(&path);
        assert_eq!(read.len(), bytes.len(), "{path}: length");
        assert!(read == bytes, "{path}: contents differ");
    }
}

#[test]
fn the_kernel_spans_many_clusters_and_its_chain_is_walked_entry_by_entry() {
    let image = build_image(&spec(true)).unwrap();
    let volume = Volume::open(esp(&image));
    let (bytes, chain) = volume.read_file("boot/kernel");
    assert_eq!(bytes.len(), KERNEL_LEN);
    assert_eq!(chain.len(), KERNEL_LEN.div_ceil(512));
    assert!(chain.len() > 6000, "not a multi-megabyte chain");

    // Entry by entry: each cluster's FAT entry names the next, and only the
    // last is an end-of-chain mark — the exact value the writer is told to use.
    for pair in chain.windows(2) {
        assert_eq!(volume.fat_entry(pair[0]), pair[1]);
    }
    assert_eq!(volume.fat_entry(*chain.last().unwrap()), 0x0FFF_FFFF);

    // Reassembled cluster by cluster from the chain, it is the input.
    let mut reassembled = Vec::new();
    for &c in &chain {
        reassembled.extend_from_slice(volume.cluster(c));
    }
    reassembled.truncate(KERNEL_LEN);
    assert!(reassembled == pattern(KERNEL_LEN, 2));
}

#[test]
fn an_empty_file_owns_no_cluster() {
    let image = build_image(&spec(true)).unwrap();
    let volume = Volume::open(esp(&image));
    let entry = volume.find("boot/empty");
    assert_eq!(entry.size, 0);
    assert_eq!(entry.first_cluster, 0);
    assert!(!entry.is_dir());
}

#[test]
fn the_boot_sector_fields() {
    let image = build_image(&spec(true)).unwrap();
    let vol = esp(&image);
    let v = Volume::open(vol);

    assert_eq!(&vol[..3], &[0xEB, 0x58, 0x90], "jump to the boot code");
    assert_eq!(vol[3..11].len(), 8);
    assert!(
        vol[3..11].iter().all(|b| (0x20..0x7F).contains(b)),
        "OEM name"
    );
    assert_eq!(v.bytes_per_sector, 512);
    assert_eq!(v.sectors_per_cluster, 1);
    assert_eq!(v.reserved_sectors, 32);
    assert_eq!(v.fats, 2);
    assert_eq!(vol[21], 0xF8, "media");
    assert_eq!(
        u64::from(le32(vol, 28)),
        layout::ESP_FIRST_LBA,
        "hidden sectors"
    );
    assert_eq!(v.total_sectors as u64, layout::ESP_SECTORS);
    assert_eq!(v.total_sectors * 512, vol.len());
    assert_eq!(le16(vol, 40), 0, "ext flags: both FATs mirrored");
    assert_eq!(le16(vol, 42), 0, "filesystem version 0.0");
    assert_eq!(v.root_cluster, 2);
    assert_eq!(v.fsinfo_sector, 1);
    assert_eq!(v.backup_boot_sector, 6);
    assert!(vol[52..64].iter().all(|&b| b == 0), "reserved");
    assert_eq!(vol[64], 0x80, "drive number");
    assert_eq!(vol[65], 0, "reserved1");
    assert_eq!(vol[66], 0x29, "extended boot signature");
    assert_ne!(le32(vol, 67), 0, "volume serial");
    assert_eq!(&vol[82..90], b"FAT32   ");

    // Each FAT has an entry for every cluster.
    assert!(v.fat_sectors * 512 / 4 >= v.clusters as usize + 2);

    // The volume label in the boot sector is the root directory's label entry.
    let label: Vec<DirEntry> = v
        .read_dir(v.root_cluster)
        .into_iter()
        .filter(|e| e.attr & ATTR_VOLUME_ID != 0)
        .collect();
    assert_eq!(label.len(), 1);
    assert_eq!(&label[0].short, &vol[71..82]);
    assert_eq!(label[0].first_cluster, 0);
}

#[test]
fn the_reserved_fat_entries_the_fat_copies_fsinfo_and_the_backup_boot_sector() {
    let image = build_image(&spec(true)).unwrap();
    let vol = esp(&image);
    let v = Volume::open(vol);

    // FAT[0] carries the media byte, FAT[1] is an end-of-chain mark; read
    // unmasked, so the reserved top nibble is checked too.
    assert_eq!(le32(v.fat(0), 0), 0x0FFF_FFF8);
    assert_eq!(le32(v.fat(0), 4), 0x0FFF_FFFF);
    assert_eq!(v.fat(0)[0], vol[21], "FAT[0]'s low byte is the media byte");
    assert!(v.fat(0) == v.fat(1), "the two FATs differ");

    // FSInfo.
    let fsinfo = v.sector(v.fsinfo_sector);
    assert_eq!(le32(fsinfo, 0), 0x4161_5252);
    assert_eq!(le32(fsinfo, 484), 0x6141_7272);
    assert_eq!(le32(fsinfo, 508), 0xAA55_0000);
    assert!(fsinfo[4..484].iter().all(|&b| b == 0));
    assert!(fsinfo[496..508].iter().all(|&b| b == 0));

    // Its free count is the number of zero FAT entries, and the used clusters
    // are exactly the ones reachable from the root: nothing lost, nothing
    // cross-linked short.
    let free = (2..v.clusters + 2).filter(|&n| v.fat_entry(n) == 0).count();
    assert_eq!(le32(fsinfo, 488) as usize, free);
    assert_eq!(v.clusters as usize - free, v.reachable(v.root_cluster));
    // Its hint is the first free cluster.
    let first_free = (2..v.clusters + 2).find(|&n| v.fat_entry(n) == 0).unwrap();
    assert_eq!(le32(fsinfo, 492), first_free);
    // Nothing is marked in the FAT beyond the last cluster.
    assert!(
        v.fat(0)[(v.clusters as usize + 2) * 4..]
            .iter()
            .all(|&b| b == 0)
    );

    // The backup boot record: boot sector, FSInfo and the third sector.
    let backup = v.backup_boot_sector;
    assert_eq!(v.sector(backup), v.sector(0), "backup boot sector");
    assert_eq!(v.sector(backup + 1), fsinfo, "backup FSInfo");
    assert_eq!(v.sector(backup + 2), v.sector(2));
    assert_eq!(&v.sector(2)[508..], &[0x00, 0x00, 0x55, 0xAA]);
}

#[test]
fn directories_carry_dot_entries_that_point_the_right_way() {
    let image = build_image(&spec(true)).unwrap();
    let v = Volume::open(esp(&image));

    let efi = v.find("EFI");
    let boot = v.find("EFI/BOOT");
    assert!(efi.is_dir() && boot.is_dir());
    assert_eq!(efi.size, 0, "a directory's size field is zero");

    let in_efi = v.read_dir(efi.first_cluster);
    assert_eq!(&in_efi[0].short, b".          ");
    assert_eq!(in_efi[0].first_cluster, efi.first_cluster);
    assert_eq!(&in_efi[1].short, b"..         ");
    assert_eq!(
        in_efi[1].first_cluster, 0,
        "`..` of a first-level directory is cluster 0, not the root's cluster"
    );
    assert!(in_efi[0].is_dir() && in_efi[1].is_dir());

    let in_boot = v.read_dir(boot.first_cluster);
    assert_eq!(&in_boot[0].short, b".          ");
    assert_eq!(in_boot[0].first_cluster, boot.first_cluster);
    assert_eq!(&in_boot[1].short, b"..         ");
    assert_eq!(in_boot[1].first_cluster, efi.first_cluster);

    // The root has neither.
    assert!(
        v.read_dir(v.root_cluster)
            .iter()
            .all(|e| e.short[0] != b'.')
    );
}

#[test]
fn short_names_and_long_names() {
    let image = build_image(&spec(true)).unwrap();
    let v = Volume::open(esp(&image));

    // Upper-case 8.3 names need no long entry.
    for path in ["EFI", "EFI/BOOT", "EFI/BOOT/BOOTAA64.EFI"] {
        let e = v.find(path);
        assert_eq!(e.long_name, None, "{path}");
    }
    assert_eq!(&v.find("EFI/BOOT/BOOTAA64.EFI").short, b"BOOTAA64EFI");

    // A four-character extension does not fit: long name plus a numeric tail.
    let conf = v.find("limine.conf");
    assert_eq!(conf.long_name.as_deref(), Some("limine.conf"));
    assert_eq!(&conf.short, b"LIMINE~1CON");
    assert_eq!(conf.short_name(), "LIMINE~1.CON");

    // Lower case that otherwise fits keeps its case in a long entry; the alias
    // is the name upper-cased, with no tail because nothing was lost.
    let boot = v.find("boot");
    assert_eq!(boot.long_name.as_deref(), Some("boot"));
    assert_eq!(&boot.short, b"BOOT       ");
    let kernel = v.find("boot/kernel");
    assert_eq!(kernel.long_name.as_deref(), Some("kernel"));
    assert_eq!(&kernel.short, b"KERNEL     ");
}

#[test]
fn numeric_tails_are_unique_within_a_directory() {
    let files = [
        ("limine.conf", b"a".to_vec()),
        ("limine.config", b"b".to_vec()),
        ("limine-backup.conf", b"c".to_vec()),
        // A real 8.3 name that is exactly what the first alias would have been.
        ("LIMINE~1.CON", b"d".to_vec()),
        (
            "a name with spaces and a very long tail indeed.text",
            b"e".to_vec(),
        ),
        (".hidden", b"f".to_vec()),
        ("plus+sign.txt", b"g".to_vec()),
    ];
    let image = build_image(&esp_only(&files)).unwrap();
    let v = Volume::open(esp(&image));

    let mut shorts = Vec::new();
    for (path, bytes) in &files {
        let (read, _) = v.read_file(path);
        assert_eq!(&read, bytes, "{path}");
        shorts.push(v.find(path).short);
    }
    let mut unique = shorts.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), shorts.len(), "two entries share a short name");

    assert_eq!(&v.find("LIMINE~1.CON").short, b"LIMINE~1CON");
    assert_eq!(v.find("LIMINE~1.CON").long_name, None);
    assert_eq!(&v.find("limine.conf").short, b"LIMINE~2CON");
    assert_eq!(&v.find("limine.config").short, b"LIMINE~3CON");
    assert_eq!(&v.find("limine-backup.conf").short, b"LIMINE~4CON");
    assert_eq!(
        &v.find("a name with spaces and a very long tail indeed.text")
            .short,
        b"ANAMEW~1TEX"
    );
    assert_eq!(&v.find(".hidden").short, b"HIDDEN~1   ");
    assert_eq!(&v.find("plus+sign.txt").short, b"PLUS_S~1TXT");

    // Every short name is space-padded upper-case from the short character set.
    for short in shorts {
        assert!(
            short.iter().all(|&c| c == b' '
                || c.is_ascii_digit()
                || c.is_ascii_uppercase()
                || b"~_".contains(&c)),
            "{short:?}"
        );
    }
}

#[test]
fn a_long_name_of_exactly_thirteen_characters_has_no_terminator() {
    // 13 and 26 characters fill their entries; 14 needs a second for one unit.
    let files = [
        ("thirteen.char", b"13".to_vec()),
        ("fourteen.chars", b"14".to_vec()),
        ("twenty-six-characters.here", b"26".to_vec()),
    ];
    let image = build_image(&esp_only(&files)).unwrap();
    let v = Volume::open(esp(&image));
    for (path, bytes) in &files {
        assert_eq!(&v.read_file(path).0, bytes, "{path}");
    }
    // Slots in the root: the label, then 1+1, 2+1 and 2+1.
    let raw = v.read_chain(v.root_cluster);
    let used = raw
        .as_chunks::<32>()
        .0
        .iter()
        .take_while(|e| e[0] != 0)
        .count();
    assert_eq!(used, 1 + 2 + 3 + 3);
}

#[test]
fn a_directory_larger_than_one_cluster_is_chained() {
    // 40 files with 31-character names: 40 x (3 long entries + 1 short) + the
    // two dot entries is 162 slots, 16 to a cluster.
    let files: Vec<(String, Vec<u8>)> = (0..40)
        .map(|i| {
            (
                format!("many/a rather long file name {i:02}.data"),
                pattern(700 + i, i as u32),
            )
        })
        .collect();
    let image = build_image(&Spec {
        esp_files: files.clone(),
        root: None,
    })
    .unwrap();
    let v = Volume::open(esp(&image));

    let dir = v.find("many");
    assert_eq!(files[0].0.len(), "many/".len() + 31);
    assert_eq!(v.chain(dir.first_cluster).len(), 162usize.div_ceil(16));
    assert_eq!(v.children(dir.first_cluster).len(), 40);
    for (path, bytes) in &files {
        assert_eq!(&v.read_file(path).0, bytes, "{path}");
    }
    let fsinfo = v.sector(v.fsinfo_sector);
    let free = (2..v.clusters + 2).filter(|&n| v.fat_entry(n) == 0).count();
    assert_eq!(le32(fsinfo, 488) as usize, free);
    assert_eq!(v.clusters as usize - free, v.reachable(v.root_cluster));
}

#[test]
fn an_esp_with_no_files_is_still_a_volume() {
    let image = build_image(&esp_only(&[])).unwrap();
    let v = Volume::open(esp(&image));
    assert!(v.children(v.root_cluster).is_empty());
    assert_eq!(v.chain(v.root_cluster), vec![2]);
    assert_eq!(le32(v.sector(1), 488), v.clusters - 1);
    assert_eq!(le32(v.sector(1), 492), 3);
}

// ---------------------------------------------------------------------------
// Determinism, and the sparse writer.
// ---------------------------------------------------------------------------

#[test]
fn the_same_inputs_give_the_same_bytes() {
    for with_root in [true, false] {
        let first = build_image(&spec(with_root)).unwrap();
        let second = build_image(&spec(with_root)).unwrap();
        assert!(first == second, "two builds differ (root: {with_root})");
    }
}

#[test]
fn the_sparse_writer_produces_the_image_and_skips_the_zeros() {
    /// A seekable sink that records how many bytes were really written.
    struct Counting {
        inner: std::io::Cursor<Vec<u8>>,
        written: usize,
    }
    impl std::io::Write for Counting {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written += buf.len();
            self.inner.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }
    impl std::io::Seek for Counting {
        fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    for with_root in [true, false] {
        let image = build_image(&spec(with_root)).unwrap();
        let mut out = Counting {
            inner: std::io::Cursor::new(Vec::new()),
            written: 0,
        };
        write_sparse(&mut out, &image).unwrap();
        assert!(out.inner.get_ref() == &image, "the written file differs");
        // About 3 MiB of files (and 1 MiB of root) in a 37 MiB image.
        assert!(
            out.written < 6 * 1024 * 1024,
            "{} bytes written for a mostly empty image",
            out.written
        );
    }

    // An image whose tail is zero still comes out at full length.
    let mut tail_zero = vec![0u8; 3 * 4096 + 17];
    tail_zero[5] = 1;
    let mut out = std::io::Cursor::new(Vec::new());
    write_sparse(&mut out, &tail_zero).unwrap();
    assert_eq!(out.get_ref(), &tail_zero);

    let mut out = std::io::Cursor::new(Vec::new());
    write_sparse(&mut out, &[]).unwrap();
    assert!(out.get_ref().is_empty());
}

// ---------------------------------------------------------------------------
// Refusals.
// ---------------------------------------------------------------------------

fn refusal(spec: &Spec) -> Error {
    // Not `expect_err`: that would print 37 MiB of image on a failure.
    match build_image(spec) {
        Ok(_) => panic!("the image was built"),
        Err(e) => e,
    }
}

#[test]
fn a_root_image_of_the_wrong_length_is_refused() {
    for len in [
        0,
        ROOTFS_IMAGE_BYTES - 1,
        ROOTFS_IMAGE_BYTES + 1,
        2 * ROOTFS_IMAGE_BYTES,
    ] {
        let mut root = root_image();
        root.resize(len, 0);
        let err = refusal(&Spec {
            esp_files: boot_files(),
            root: Some(root),
        });
        assert_eq!(err, Error::RootLength { got: len });
        assert!(err.to_string().contains(&ROOTFS_IMAGE_BYTES.to_string()));
    }
}

#[test]
fn a_root_image_without_the_label_is_refused() {
    // All zero; one label byte wrong; the *tail* label in the head's place.
    let mut wrong_byte = root_image();
    wrong_byte[HDR_LABEL_OFF + IMAGE_LABEL_LEN - 2] ^= 1;
    let mut tail_label = root_image();
    tail_label[HDR_LABEL_OFF..HDR_LABEL_OFF + IMAGE_LABEL_LEN]
        .copy_from_slice(&minixrs_kernel_shared::rootfs::IMAGE_TAIL_LABEL);
    for root in [vec![0u8; ROOTFS_IMAGE_BYTES], wrong_byte, tail_label] {
        let err = refusal(&Spec {
            esp_files: boot_files(),
            root: Some(root),
        });
        assert_eq!(err, Error::RootLabel);
    }
}

#[test]
fn files_too_large_for_the_esp_are_refused() {
    let clusters = layout::ESP_GEOMETRY.clusters as usize;

    // One file of every cluster but the root directory's: fits exactly.
    let fits = esp_only(&[("BIG", vec![0xA5; (clusters - 1) * 512])]);
    let image = build_image(&fits).unwrap();
    let v = Volume::open(esp(&image));
    assert_eq!(le32(v.sector(1), 488), 0, "free count of a full volume");
    assert_eq!(
        le32(v.sector(1), 492),
        0xFFFF_FFFF,
        "no free cluster to hint at"
    );
    let (bytes, chain) = v.read_file("BIG");
    assert_eq!(chain.len(), clusters - 1);
    assert_eq!(
        *chain.last().unwrap(),
        v.clusters + 1,
        "the last cluster of the volume"
    );
    assert!(bytes.iter().all(|&b| b == 0xA5));
    drop(image);

    // One byte more does not.
    let over = esp_only(&[("BIG", vec![0xA5; (clusters - 1) * 512 + 1])]);
    assert!(
        matches!(refusal(&over), Error::EspFull { .. }),
        "{:?}",
        refusal(&over)
    );

    // Nor do two files that only overflow together.
    let half = vec![0u8; clusters * 512 / 2];
    let two = esp_only(&[("A", half.clone()), ("B", half)]);
    let err = refusal(&two);
    assert!(matches!(err, Error::EspFull { .. }), "{err:?}");
    assert!(err.to_string().contains("ESP"));
}

#[test]
fn a_duplicate_path_is_refused() {
    let exact = esp_only(&[("boot/kernel", vec![1]), ("boot/kernel", vec![2])]);
    assert_eq!(
        refusal(&exact),
        Error::DuplicatePath {
            path: "boot/kernel".into()
        }
    );

    // FAT lookups ignore case, so these two are one name.
    let by_case = esp_only(&[("boot/kernel", vec![1]), ("BOOT/Kernel", vec![2])]);
    assert_eq!(
        refusal(&by_case),
        Error::DuplicatePath {
            path: "BOOT/Kernel".into()
        }
    );

    // A file where a directory is needed, and the reverse.
    let file_then_dir = esp_only(&[("boot", vec![1]), ("boot/kernel", vec![2])]);
    assert!(matches!(
        refusal(&file_then_dir),
        Error::PathConflict { .. }
    ));
    let dir_then_file = esp_only(&[("boot/kernel", vec![1]), ("boot", vec![2])]);
    assert!(matches!(
        refusal(&dir_then_file),
        Error::PathConflict { .. }
    ));
}

#[test]
fn an_invalid_path_is_refused() {
    let long = "x".repeat(256);
    for path in [
        "",
        "/boot/kernel",
        "boot//kernel",
        "boot/",
        "boot/../kernel",
        "..",
        "./kernel",
        "boot\\kernel",
        "what?",
        "star*",
        "a:b",
        "quo\"te",
        "less<",
        "more>",
        "pipe|",
        "tab\there",
        "nul\0",
        "trailing.",
        "trailing ",
        " leading",
        "caf\u{e9}",
        long.as_str(),
    ] {
        let err = refusal(&esp_only(&[(path, vec![1])]));
        assert!(
            matches!(&err, Error::InvalidPath { path: p, .. } if p == path),
            "{path:?}: {err:?}"
        );
    }

    // The longest name FAT allows is accepted.
    let longest = "y".repeat(255);
    let image = build_image(&esp_only(&[(longest.as_str(), vec![7])])).unwrap();
    assert_eq!(Volume::open(esp(&image)).read_file(&longest).0, vec![7]);
}
