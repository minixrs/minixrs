// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! A FAT32 volume, written whole: the ESP that edk2's FAT driver and Limine
//! read the kernel from.
//!
//! Written from Microsoft's FAT specification ("FAT32 File System
//! Specification", the `fatgen103` document). The volume is laid out as
//!
//! ```text
//! sector 0            boot sector (BPB)        } the reserved region,
//! sector 1            FSInfo                   } RESERVED_SECTORS long;
//! sector 2            third boot-record sector } sectors 6..=8 are the
//! sector 6, 7, 8      the backup of 0, 1, 2    } backup boot record
//! sector 32           FAT 1
//! sector 32 + F       FAT 2 (identical)
//! sector 32 + 2F      the data region: cluster 2 onwards
//! ```
//!
//! and it is *created*, never modified, which makes two things simple:
//!
//! - **allocation is contiguous** — the root directory at cluster 2, then every
//!   other directory, then every file, each a run of consecutive clusters whose
//!   FAT entries each name the next;
//! - **nothing is ever deleted**, so there are no `0xE5` entries and each
//!   directory ends at its first all-zero entry.
//!
//! Names follow the VFAT rules. A name that is already a valid upper-case 8.3
//! name gets a short entry alone. Any other name gets long-name entries
//! carrying its exact spelling in front of a generated short alias: the name
//! upper-cased if that loses nothing (`kernel` → `KERNEL`), otherwise a
//! basis-name with a numeric tail (`limine.conf` → `LIMINE~1.CON`).
//!
//! Timestamps are all zero and the volume serial is fixed, so the same files
//! give the same bytes.

use std::collections::BTreeSet;

use crate::Error;

/// Bytes per sector, as the BPB records it.
pub const BYTES_PER_SECTOR: usize = 512;

/// Sectors per cluster. One, the smallest: it is what gets a 34 MiB volume over
/// [`MIN_FAT32_CLUSTERS`].
pub const SECTORS_PER_CLUSTER: u32 = 1;

/// Bytes per cluster.
pub const CLUSTER_BYTES: usize = BYTES_PER_SECTOR * SECTORS_PER_CLUSTER as usize;

/// Sectors before the first FAT. 32 is what the specification says a FAT32
/// volume "typically" has, and what every formatter writes.
pub const RESERVED_SECTORS: u32 = 32;

/// Copies of the FAT.
pub const FAT_COPIES: u32 = 2;

/// The number of the first cluster in the data region: FAT entries 0 and 1 are
/// reserved.
pub const FIRST_DATA_CLUSTER: u32 = 2;

/// The root directory's first cluster.
pub const ROOT_CLUSTER: u32 = FIRST_DATA_CLUSTER;

/// Sector of the FSInfo structure.
pub const FSINFO_SECTOR: u32 = 1;

/// Sector of the backup boot sector; the backup FSInfo follows it.
pub const BACKUP_BOOT_SECTOR: u32 = 6;

/// Sectors in the boot record that the backup duplicates: the boot sector,
/// FSInfo, and a third sector that holds only a signature.
pub const BOOT_RECORD_SECTORS: u32 = 3;

/// The media descriptor: `0xF8`, "fixed disk". FAT entry 0's low byte repeats
/// it.
pub const MEDIA: u8 = 0xF8;

/// Bytes in one FAT32 entry.
pub const FAT_ENTRY_BYTES: usize = 4;

/// FAT entries in one sector of a FAT.
pub const FAT_ENTRIES_PER_SECTOR: u32 = (BYTES_PER_SECTOR / FAT_ENTRY_BYTES) as u32;

/// The fewest clusters a FAT32 volume may have. **The FAT type is decided by
/// the cluster count and by nothing else**: a volume with fewer is FAT16 to a
/// conforming driver even if its boot sector says `FAT32`.
pub const MIN_FAT32_CLUSTERS: u32 = 65_525;

/// The most clusters a FAT32 volume may have: cluster numbers from
/// `0x0FFFFFF7` up are the bad-cluster and end-of-chain marks.
pub const MAX_FAT32_CLUSTERS: u32 = 0x0FFF_FFF7 - FIRST_DATA_CLUSTER;

/// The volume label, in the boot sector and as the root directory's first
/// entry.
pub const VOLUME_LABEL: [u8; 11] = *b"MINIXRS ESP";

/// The volume serial number. A formatter derives it from the clock; fixed
/// here, because the image is reproducible.
pub const VOLUME_SERIAL: u32 = 0x6D78_0604;

/// The OEM name: who formatted the volume. Informational.
pub const OEM_NAME: [u8; 8] = *b"MINIXRS ";

/// FAT entry 0: the media descriptor in the low byte, ones above it.
const FAT_ENTRY_MEDIA: u32 = 0x0FFF_FF00 | MEDIA as u32;
/// The end-of-chain mark, also FAT entry 1's value (whose two top bits are the
/// "volume is clean" and "no I/O errors" flags, both set).
const END_OF_CHAIN: u32 = 0x0FFF_FFFF;

const DIR_ENTRY_BYTES: usize = 32;
/// A directory may not exceed 2 MiB, i.e. 65 536 entries.
const MAX_DIR_ENTRIES: usize = 65_536;
/// Characters in the longest long name.
const MAX_NAME_CHARS: usize = 255;
/// UTF-16 units one long-name entry carries.
const LONG_ENTRY_UNITS: usize = 13;
/// The highest numeric tail, `~999999`.
const MAX_NUMERIC_TAIL: u32 = 999_999;

const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
/// Read-only | hidden | system | volume-id: the combination that marks a
/// long-name entry, which pre-VFAT systems skip.
const ATTR_LONG_NAME: u8 = 0x0F;
/// Set in the sequence byte of a name's last long entry (the first on disk).
const LAST_LONG_ENTRY: u8 = 0x40;

// Boot sector (BPB) field offsets.
const BS_JUMP: usize = 0;
const BS_OEM_NAME: usize = 3;
const BPB_BYTES_PER_SECTOR: usize = 11;
const BPB_SECTORS_PER_CLUSTER: usize = 13;
const BPB_RESERVED_SECTORS: usize = 14;
const BPB_FAT_COPIES: usize = 16;
const BPB_MEDIA: usize = 21;
const BPB_SECTORS_PER_TRACK: usize = 24;
const BPB_HEADS: usize = 26;
const BPB_HIDDEN_SECTORS: usize = 28;
const BPB_TOTAL_SECTORS_32: usize = 32;
const BPB_FAT_SECTORS_32: usize = 36;
const BPB_ROOT_CLUSTER: usize = 44;
const BPB_FSINFO_SECTOR: usize = 48;
const BPB_BACKUP_BOOT_SECTOR: usize = 50;
const BS_DRIVE_NUMBER: usize = 64;
const BS_BOOT_SIGNATURE: usize = 66;
const BS_VOLUME_SERIAL: usize = 67;
const BS_VOLUME_LABEL: usize = 71;
const BS_FS_TYPE: usize = 82;
const SECTOR_SIGNATURE: usize = 510;

// FSInfo field offsets.
const FSI_LEAD_SIGNATURE: usize = 0;
const FSI_STRUCT_SIGNATURE: usize = 484;
const FSI_FREE_COUNT: usize = 488;
const FSI_NEXT_FREE: usize = 492;
const FSI_TRAIL_SIGNATURE: usize = 508;

// Directory entry field offsets.
const DIR_ATTR: usize = 11;
const DIR_FIRST_CLUSTER_HIGH: usize = 20;
const DIR_FIRST_CLUSTER_LOW: usize = 26;
const DIR_FILE_SIZE: usize = 28;
const LDIR_CHECKSUM: usize = 13;
/// Where the 13 UTF-16 units of a long entry sit: 5, then 6, then 2.
const LDIR_UNIT_OFFSETS: [usize; LONG_ENTRY_UNITS] =
    [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];

// The backup boot record fits in the reserved region, after the primary.
const _: () = assert!(FSINFO_SECTOR < BOOT_RECORD_SECTORS);
const _: () = assert!(BACKUP_BOOT_SECTOR >= BOOT_RECORD_SECTORS);
const _: () = assert!(BACKUP_BOOT_SECTOR + BOOT_RECORD_SECTORS <= RESERVED_SECTORS);

/// The sizes that follow from a volume's sector count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// Sectors in the volume.
    pub total_sectors: u32,
    /// Sectors in each FAT.
    pub fat_sectors: u32,
    /// First sector of the data region — of cluster 2.
    pub first_data_sector: u32,
    /// Clusters in the data region.
    pub clusters: u32,
}

/// The geometry of a volume of `total_sectors` sectors.
///
/// The FAT size is the specification's own computation for FAT32: the sectors
/// after the reserved region, divided (rounding up) by `(256 × sectors per
/// cluster + FAT copies) / 2`. It over-provisions each FAT by a few sectors,
/// which the specification accepts; what matters is that it never
/// under-provisions, and `layout` asserts that for the ESP.
///
/// A volume too small to hold its own reserved region comes back with
/// `clusters == 0`.
#[must_use]
pub const fn geometry(total_sectors: u32) -> Geometry {
    let after_reserved = total_sectors.saturating_sub(RESERVED_SECTORS);
    let divisor = (256 * SECTORS_PER_CLUSTER + FAT_COPIES) / 2;
    let fat_sectors = after_reserved.div_ceil(divisor);
    let first_data_sector = RESERVED_SECTORS + FAT_COPIES * fat_sectors;
    let clusters = total_sectors.saturating_sub(first_data_sector) / SECTORS_PER_CLUSTER;
    Geometry {
        total_sectors,
        fat_sectors,
        first_data_sector,
        clusters,
    }
}

impl Geometry {
    /// Byte offset of `cluster` within the volume.
    fn cluster_offset(&self, cluster: u32) -> usize {
        let sector = self.first_data_sector as usize
            + (cluster - FIRST_DATA_CLUSTER) as usize * SECTORS_PER_CLUSTER as usize;
        sector * BYTES_PER_SECTOR
    }
}

fn put(bytes: &mut [u8], offset: usize, value: &[u8]) {
    bytes[offset..offset + value.len()].copy_from_slice(value);
}

// ---------------------------------------------------------------------------
// Names.
// ---------------------------------------------------------------------------

/// Why `name` cannot be a FAT long name, or `Ok` if it can.
///
/// Stricter than FAT in one respect: names are ASCII. A long name could carry
/// any UTF-16, but a non-ASCII short alias depends on an OEM code page, and
/// nothing a boot partition holds needs one.
fn validate_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("it has an empty component");
    }
    if name == "." || name == ".." {
        return Err("`.` and `..` are not names");
    }
    if name.len() > MAX_NAME_CHARS {
        return Err("a component is longer than 255 characters");
    }
    for c in name.bytes() {
        if !c.is_ascii() {
            return Err("it has a non-ASCII character");
        }
        if c < 0x20 || c == 0x7F {
            return Err("it has a control character");
        }
        if b"\"*/:<>?\\|".contains(&c) {
            return Err("it has a character FAT forbids (one of \" * : < > ? \\ |)");
        }
    }
    if name.starts_with(' ') || name.ends_with(' ') || name.ends_with('.') {
        return Err("a component starts with a space, or ends with a space or a period");
    }
    Ok(())
}

/// Whether `c` may appear in a short (8.3) name: upper-case letters, digits and
/// a fixed set of punctuation.
fn is_short_char(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit() || b"$%'-_@~`!(){}^#&".contains(&c)
}

/// The short-name basis of a long name: what the specification's "basis-name
/// generation" yields before any numeric tail.
struct Basis {
    /// Up to 8 characters.
    base: Vec<u8>,
    /// Up to 3 characters.
    extension: Vec<u8>,
    /// Whether the conversion lost something — a character dropped or replaced,
    /// or either part truncated — so the alias must carry a numeric tail.
    lossy: bool,
}

fn basis(name: &str) -> Basis {
    let mut lossy = false;
    // Leading periods are dropped; the extension is what follows the last
    // period that remains.
    let stripped = name.trim_start_matches('.');
    lossy |= stripped.len() != name.len();
    let (raw_base, raw_extension) = stripped.rsplit_once('.').unwrap_or((stripped, ""));

    let mut convert = |raw: &str, max: usize| {
        let mut out = Vec::new();
        for c in raw.bytes() {
            // Spaces and (embedded) periods are dropped.
            if c == b' ' || c == b'.' {
                lossy = true;
                continue;
            }
            let upper = c.to_ascii_uppercase();
            if is_short_char(upper) {
                out.push(upper);
            } else {
                out.push(b'_');
                lossy = true;
            }
        }
        if out.len() > max {
            out.truncate(max);
            lossy = true;
        }
        out
    };
    let base = convert(raw_base, 8);
    let extension = convert(raw_extension, 3);
    Basis {
        base,
        extension,
        lossy,
    }
}

/// The 11-byte short-name field: base and extension, each space-padded.
fn pack_short(base: &[u8], extension: &[u8]) -> [u8; 11] {
    let mut short = [b' '; 11];
    short[..base.len()].copy_from_slice(base);
    short[8..8 + extension.len()].copy_from_slice(extension);
    short
}

/// `BASE.EXT` as a lookup would spell the short name.
fn display_short(base: &[u8], extension: &[u8]) -> Vec<u8> {
    let mut s = base.to_vec();
    if !extension.is_empty() {
        s.push(b'.');
        s.extend_from_slice(extension);
    }
    s
}

/// The checksum of a short name that each of its long entries carries: for each
/// of the 11 bytes, rotate the sum right one bit and add the byte.
fn short_name_checksum(short: &[u8; 11]) -> u8 {
    let mut sum = 0u8;
    for &c in short {
        sum = ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(c);
    }
    sum
}

// ---------------------------------------------------------------------------
// The directory tree.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Target {
    /// Index into the caller's file list.
    File(usize),
    /// Index into [`Tree::dirs`].
    Dir(usize),
}

struct Entry {
    name: String,
    /// The 11-byte short name; assigned by [`assign_short_names`].
    short: [u8; 11],
    /// Whether the entry needs long-name entries in front of its short entry.
    long: bool,
    target: Target,
}

impl Entry {
    fn new(name: &str, target: Target) -> Self {
        Self {
            name: name.to_string(),
            short: [b' '; 11],
            long: false,
            target,
        }
    }

    /// Long-name entries in front of the short one.
    fn long_entries(&self) -> usize {
        if self.long {
            self.name.encode_utf16().count().div_ceil(LONG_ENTRY_UNITS)
        } else {
            0
        }
    }
}

struct Dir {
    /// Its path, for error messages; empty for the root.
    path: String,
    /// Index of the parent in [`Tree::dirs`]; `None` for the root.
    parent: Option<usize>,
    entries: Vec<Entry>,
}

impl Dir {
    /// 32-byte slots the directory uses: the volume label in the root or `.`
    /// and `..` elsewhere, then each entry's long entries and its short one.
    fn slots(&self) -> usize {
        let fixed = if self.parent.is_none() { 1 } else { 2 };
        fixed
            + self
                .entries
                .iter()
                .map(|e| e.long_entries() + 1)
                .sum::<usize>()
    }
}

/// Every directory of the volume; index 0 is the root.
struct Tree {
    dirs: Vec<Dir>,
}

impl Tree {
    fn new() -> Self {
        Self {
            dirs: vec![Dir {
                path: String::new(),
                parent: None,
                entries: Vec::new(),
            }],
        }
    }

    /// FAT names compare without regard to case.
    fn lookup(&self, dir: usize, name: &str) -> Option<Target> {
        self.dirs[dir]
            .entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))
            .map(|e| e.target)
    }

    /// Add file number `file` at `path`, creating its directories.
    fn insert(&mut self, path: &str, file: usize) -> Result<(), Error> {
        let components: Vec<&str> = path.split('/').collect();
        for component in &components {
            validate_name(component).map_err(|why| Error::InvalidPath {
                path: path.to_string(),
                why,
            })?;
        }
        // `split` yields at least one item, so there is always a leaf.
        let Some((leaf, parents)) = components.split_last() else {
            return Ok(());
        };

        let mut dir = 0;
        for name in parents {
            dir = match self.lookup(dir, name) {
                Some(Target::Dir(existing)) => existing,
                Some(Target::File(_)) => {
                    return Err(Error::PathConflict {
                        path: path.to_string(),
                    });
                }
                None => {
                    let new = self.dirs.len();
                    let parent_path = &self.dirs[dir].path;
                    let new_path = if parent_path.is_empty() {
                        (*name).to_string()
                    } else {
                        format!("{parent_path}/{name}")
                    };
                    self.dirs.push(Dir {
                        path: new_path,
                        parent: Some(dir),
                        entries: Vec::new(),
                    });
                    self.dirs[dir]
                        .entries
                        .push(Entry::new(name, Target::Dir(new)));
                    new
                }
            };
        }
        match self.lookup(dir, leaf) {
            Some(Target::File(_)) => Err(Error::DuplicatePath {
                path: path.to_string(),
            }),
            Some(Target::Dir(_)) => Err(Error::PathConflict {
                path: path.to_string(),
            }),
            None => {
                self.dirs[dir]
                    .entries
                    .push(Entry::new(leaf, Target::File(file)));
                Ok(())
            }
        }
    }
}

/// Give every entry of one directory its short name, unique within the
/// directory. `None` if the numeric tails ran out.
///
/// Two passes, so the result does not depend on the order files were given in
/// any way that could collide: first every name whose upper-cased form *is* its
/// short name claims it (two such names cannot clash — they would be the same
/// name to FAT, which [`Tree::insert`] already refused); then each remaining
/// name takes the lowest numeric tail not yet claimed.
fn assign_short_names(entries: &mut [Entry]) -> Option<()> {
    let mut taken: BTreeSet<[u8; 11]> = BTreeSet::new();
    let bases: Vec<Basis> = entries.iter().map(|e| basis(&e.name)).collect();

    for (entry, b) in entries.iter_mut().zip(&bases) {
        if !b.lossy {
            entry.short = pack_short(&b.base, &b.extension);
            // Only a name that is not already spelled as its short name needs
            // long entries (to keep its lower case).
            entry.long = display_short(&b.base, &b.extension) != entry.name.as_bytes();
            taken.insert(entry.short);
        }
    }
    for (entry, b) in entries.iter_mut().zip(&bases) {
        if !b.lossy {
            continue;
        }
        entry.long = true;
        entry.short = (1..=MAX_NUMERIC_TAIL).find_map(|n| {
            let tail = format!("~{n}");
            // The specification keeps six characters before `~1`; a longer
            // tail takes its room from the base.
            let keep = b.base.len().min(8 - tail.len());
            let mut base = b.base[..keep].to_vec();
            base.extend_from_slice(tail.as_bytes());
            let short = pack_short(&base, &b.extension);
            taken.insert(short).then_some(short)
        })?;
    }
    Some(())
}

// ---------------------------------------------------------------------------
// Directory entries.
// ---------------------------------------------------------------------------

/// A short (8.3) directory entry. Every time and date field stays zero.
fn short_entry(short: &[u8; 11], attr: u8, first_cluster: u32, size: u32) -> [u8; DIR_ENTRY_BYTES] {
    let mut e = [0u8; DIR_ENTRY_BYTES];
    put(&mut e, 0, short);
    e[DIR_ATTR] = attr;
    let cluster = first_cluster.to_le_bytes();
    put(&mut e, DIR_FIRST_CLUSTER_LOW, &cluster[..2]);
    put(&mut e, DIR_FIRST_CLUSTER_HIGH, &cluster[2..]);
    put(&mut e, DIR_FILE_SIZE, &size.to_le_bytes());
    e
}

/// The long-name entries for `name`, in the order they go on disk: the *last*
/// piece of the name first, flagged [`LAST_LONG_ENTRY`], down to sequence
/// number 1 immediately before the short entry.
///
/// The name is UTF-16LE. If it does not fill its last entry it is terminated
/// with `0x0000` and the rest of that entry is `0xFFFF`; if it fills it
/// exactly, there is no terminator.
fn long_entries(name: &str, checksum: u8) -> Vec<[u8; DIR_ENTRY_BYTES]> {
    let mut units: Vec<u16> = name.encode_utf16().collect();
    if !units.len().is_multiple_of(LONG_ENTRY_UNITS) {
        units.push(0x0000);
        while !units.len().is_multiple_of(LONG_ENTRY_UNITS) {
            units.push(0xFFFF);
        }
    }
    let count = units.len() / LONG_ENTRY_UNITS;
    let mut entries = Vec::with_capacity(count);
    for (index, piece) in units
        .as_chunks::<LONG_ENTRY_UNITS>()
        .0
        .iter()
        .enumerate()
        .rev()
    {
        let mut e = [0u8; DIR_ENTRY_BYTES];
        // At most 20 pieces (255 / 13, rounded up), so the sequence number
        // fits beside the flag.
        let mut sequence = (index + 1) as u8;
        if index + 1 == count {
            sequence |= LAST_LONG_ENTRY;
        }
        e[0] = sequence;
        e[DIR_ATTR] = ATTR_LONG_NAME;
        e[LDIR_CHECKSUM] = checksum;
        for (unit, offset) in piece.iter().zip(LDIR_UNIT_OFFSETS) {
            put(&mut e, offset, &unit.to_le_bytes());
        }
        entries.push(e);
    }
    entries
}

// ---------------------------------------------------------------------------
// The volume.
// ---------------------------------------------------------------------------

/// A run of consecutive clusters; `count == 0` is "none" and `first` is then 0,
/// which is exactly what an empty file's directory entry holds.
#[derive(Clone, Copy)]
struct Run {
    first: u32,
    count: u32,
}

/// Format `volume` as FAT32 and store `files` in it — `(path, contents)`
/// pairs, paths `/`-separated and relative to the volume's root. Directories
/// are created as needed. `hidden_sectors` is the volume's first LBA on its
/// disk, which the boot sector records.
///
/// `volume` is overwritten in full.
///
/// # Errors
///
/// - [`Error::VolumeSize`] if `volume` is not a whole number of sectors or
///   would not have a FAT32 cluster count;
/// - [`Error::InvalidPath`], [`Error::DuplicatePath`], [`Error::PathConflict`]
///   for a path that cannot be stored;
/// - [`Error::FileTooLarge`] for a file of 4 GiB or more;
/// - [`Error::DirectoryTooLarge`] for a directory past FAT's limit;
/// - [`Error::EspFull`] if the files and directories need more clusters than
///   the volume has.
pub fn write_volume(
    volume: &mut [u8],
    hidden_sectors: u32,
    files: &[(String, Vec<u8>)],
) -> Result<(), Error> {
    let sectors = volume.len() / BYTES_PER_SECTOR;
    let size_error = Error::VolumeSize {
        bytes: volume.len(),
    };
    if !volume.len().is_multiple_of(BYTES_PER_SECTOR) {
        return Err(size_error);
    }
    let Ok(total_sectors) = u32::try_from(sectors) else {
        return Err(size_error);
    };
    let geo = geometry(total_sectors);
    let fat_entries = u64::from(geo.fat_sectors) * u64::from(FAT_ENTRIES_PER_SECTOR);
    if geo.clusters < MIN_FAT32_CLUSTERS
        || geo.clusters > MAX_FAT32_CLUSTERS
        || fat_entries < u64::from(geo.clusters) + u64::from(FIRST_DATA_CLUSTER)
    {
        return Err(size_error);
    }

    // The tree, and every entry's short name.
    let mut tree = Tree::new();
    for (index, (path, _)) in files.iter().enumerate() {
        tree.insert(path, index)?;
    }
    for dir in &mut tree.dirs {
        let too_large = || Error::DirectoryTooLarge {
            path: dir.path.clone(),
        };
        assign_short_names(&mut dir.entries).ok_or_else(too_large)?;
        if dir.slots() > MAX_DIR_ENTRIES {
            return Err(too_large());
        }
    }

    // How many clusters each directory and file needs. A directory always has
    // at least one, even when empty; an empty file has none.
    let dir_clusters: Vec<u64> = tree
        .dirs
        .iter()
        .map(|d| ((d.slots() * DIR_ENTRY_BYTES).div_ceil(CLUSTER_BYTES)).max(1) as u64)
        .collect();
    let mut file_clusters = Vec::with_capacity(files.len());
    for (path, bytes) in files {
        if u32::try_from(bytes.len()).is_err() {
            return Err(Error::FileTooLarge { path: path.clone() });
        }
        file_clusters.push(bytes.len().div_ceil(CLUSTER_BYTES) as u64);
    }
    let needed = dir_clusters
        .iter()
        .chain(&file_clusters)
        .try_fold(0u64, |sum, &n| sum.checked_add(n))
        .unwrap_or(u64::MAX);
    if needed > u64::from(geo.clusters) {
        return Err(Error::EspFull {
            needed_clusters: needed,
            available_clusters: geo.clusters,
        });
    }

    // Allocate contiguously: the root at cluster 2, the other directories, then
    // the files in the order given. Every count is now known to fit, in total,
    // in `geo.clusters`, so the `u32` arithmetic below cannot overflow.
    let mut next = ROOT_CLUSTER;
    let mut allocate = |count: u64| {
        let count = count as u32;
        let run = Run {
            first: if count == 0 { 0 } else { next },
            count,
        };
        next += count;
        run
    };
    let dir_runs: Vec<Run> = dir_clusters.iter().map(|&n| allocate(n)).collect();
    let file_runs: Vec<Run> = file_clusters.iter().map(|&n| allocate(n)).collect();
    let used = needed as u32;

    volume.fill(0);

    // File contents and directories.
    for ((_, bytes), run) in files.iter().zip(&file_runs) {
        if run.count != 0 {
            put(volume, geo.cluster_offset(run.first), bytes);
        }
    }
    for (index, dir) in tree.dirs.iter().enumerate() {
        let mut data = Vec::with_capacity(dir.slots() * DIR_ENTRY_BYTES);
        match dir.parent {
            None => data.extend_from_slice(&short_entry(&VOLUME_LABEL, ATTR_VOLUME_ID, 0, 0)),
            Some(parent) => {
                // `..` names the parent's first cluster — except that a
                // parent which is the root is written as cluster 0.
                let parent_cluster = if parent == 0 {
                    0
                } else {
                    dir_runs[parent].first
                };
                let own = dir_runs[index].first;
                data.extend_from_slice(&short_entry(b".          ", ATTR_DIRECTORY, own, 0));
                data.extend_from_slice(&short_entry(
                    b"..         ",
                    ATTR_DIRECTORY,
                    parent_cluster,
                    0,
                ));
            }
        }
        for entry in &dir.entries {
            if entry.long {
                for long in long_entries(&entry.name, short_name_checksum(&entry.short)) {
                    data.extend_from_slice(&long);
                }
            }
            let (attr, first, size) = match entry.target {
                Target::Dir(d) => (ATTR_DIRECTORY, dir_runs[d].first, 0),
                // The length was checked against `u32` above.
                Target::File(f) => (ATTR_ARCHIVE, file_runs[f].first, files[f].1.len() as u32),
            };
            data.extend_from_slice(&short_entry(&entry.short, attr, first, size));
        }
        put(volume, geo.cluster_offset(dir_runs[index].first), &data);
    }

    // The first FAT: the two reserved entries, then one chain per run.
    let fat_offset = RESERVED_SECTORS as usize * BYTES_PER_SECTOR;
    let fat_bytes = geo.fat_sectors as usize * BYTES_PER_SECTOR;
    {
        let fat = &mut volume[fat_offset..fat_offset + fat_bytes];
        let mut set = |cluster: u32, value: u32| {
            put(
                fat,
                cluster as usize * FAT_ENTRY_BYTES,
                &value.to_le_bytes(),
            );
        };
        set(0, FAT_ENTRY_MEDIA);
        set(1, END_OF_CHAIN);
        for run in dir_runs.iter().chain(&file_runs) {
            for i in 0..run.count {
                let cluster = run.first + i;
                let last = i + 1 == run.count;
                set(cluster, if last { END_OF_CHAIN } else { cluster + 1 });
            }
        }
    }
    // The second FAT is a copy of the first.
    volume.copy_within(fat_offset..fat_offset + fat_bytes, fat_offset + fat_bytes);

    // The boot record and its backup.
    let free = geo.clusters - used;
    let next_free = if free == 0 { u32::MAX } else { next };
    let record = [
        boot_sector(&geo, hidden_sectors),
        fsinfo(free, next_free),
        signature_sector(),
    ];
    for start in [0, BACKUP_BOOT_SECTOR as usize] {
        for (i, sector) in record.iter().enumerate() {
            put(volume, (start + i) * BYTES_PER_SECTOR, sector);
        }
    }
    Ok(())
}

fn boot_sector(geo: &Geometry, hidden_sectors: u32) -> [u8; BYTES_PER_SECTOR] {
    let mut s = [0u8; BYTES_PER_SECTOR];
    // A short jump over the BPB to where boot code would be (there is none:
    // nothing boots this volume by its boot sector), then a NOP.
    put(&mut s, BS_JUMP, &[0xEB, 0x58, 0x90]);
    put(&mut s, BS_OEM_NAME, &OEM_NAME);
    put(
        &mut s,
        BPB_BYTES_PER_SECTOR,
        &(BYTES_PER_SECTOR as u16).to_le_bytes(),
    );
    s[BPB_SECTORS_PER_CLUSTER] = SECTORS_PER_CLUSTER as u8;
    put(
        &mut s,
        BPB_RESERVED_SECTORS,
        &(RESERVED_SECTORS as u16).to_le_bytes(),
    );
    s[BPB_FAT_COPIES] = FAT_COPIES as u8;
    // Root entry count, 16-bit total sectors and 16-bit FAT size stay zero:
    // that is what says FAT32's 32-bit fields are the ones to read.
    s[BPB_MEDIA] = MEDIA;
    // A nominal CHS geometry; nothing reads the volume by cylinder.
    put(&mut s, BPB_SECTORS_PER_TRACK, &63u16.to_le_bytes());
    put(&mut s, BPB_HEADS, &255u16.to_le_bytes());
    put(&mut s, BPB_HIDDEN_SECTORS, &hidden_sectors.to_le_bytes());
    put(
        &mut s,
        BPB_TOTAL_SECTORS_32,
        &geo.total_sectors.to_le_bytes(),
    );
    put(&mut s, BPB_FAT_SECTORS_32, &geo.fat_sectors.to_le_bytes());
    // Extended flags zero: both FATs are live and mirrored. Version 0.0.
    put(&mut s, BPB_ROOT_CLUSTER, &ROOT_CLUSTER.to_le_bytes());
    put(
        &mut s,
        BPB_FSINFO_SECTOR,
        &(FSINFO_SECTOR as u16).to_le_bytes(),
    );
    put(
        &mut s,
        BPB_BACKUP_BOOT_SECTOR,
        &(BACKUP_BOOT_SECTOR as u16).to_le_bytes(),
    );
    s[BS_DRIVE_NUMBER] = 0x80;
    // 0x29: the serial, label and type fields that follow are present.
    s[BS_BOOT_SIGNATURE] = 0x29;
    put(&mut s, BS_VOLUME_SERIAL, &VOLUME_SERIAL.to_le_bytes());
    put(&mut s, BS_VOLUME_LABEL, &VOLUME_LABEL);
    // Informational only — the cluster count decides the type — but written.
    put(&mut s, BS_FS_TYPE, b"FAT32   ");
    put(&mut s, SECTOR_SIGNATURE, &[0x55, 0xAA]);
    s
}

fn fsinfo(free_clusters: u32, next_free: u32) -> [u8; BYTES_PER_SECTOR] {
    let mut s = [0u8; BYTES_PER_SECTOR];
    put(&mut s, FSI_LEAD_SIGNATURE, &0x4161_5252u32.to_le_bytes());
    put(&mut s, FSI_STRUCT_SIGNATURE, &0x6141_7272u32.to_le_bytes());
    put(&mut s, FSI_FREE_COUNT, &free_clusters.to_le_bytes());
    put(&mut s, FSI_NEXT_FREE, &next_free.to_le_bytes());
    put(&mut s, FSI_TRAIL_SIGNATURE, &0xAA55_0000u32.to_le_bytes());
    s
}

/// The boot record's third sector: empty but for the signature, as a FAT32
/// formatter leaves it.
fn signature_sector() -> [u8; BYTES_PER_SECTOR] {
    let mut s = [0u8; BYTES_PER_SECTOR];
    put(&mut s, FSI_TRAIL_SIGNATURE, &0xAA55_0000u32.to_le_bytes());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_of_a_short_name() {
        // Worked by hand from the specification's `ChkSum` routine for a
        // one-character name: 'A' then ten spaces. After the first byte the
        // sum is 0x41; each space rotates and adds 0x20.
        let mut sum = 0x41u8;
        for _ in 0..10 {
            sum = sum.rotate_right(1).wrapping_add(0x20);
        }
        assert_eq!(short_name_checksum(b"A          "), sum);
        // Different names, different sums; the same name, the same sum.
        assert_ne!(
            short_name_checksum(b"LIMINE~1CON"),
            short_name_checksum(b"LIMINE~2CON")
        );
    }

    #[test]
    fn a_basis_name_follows_the_specification() {
        let cases: [(&str, &[u8], &[u8], bool); 9] = [
            ("KERNEL", b"KERNEL", b"", false),
            ("kernel", b"KERNEL", b"", false),
            ("BOOTAA64.EFI", b"BOOTAA64", b"EFI", false),
            ("limine.conf", b"LIMINE", b"CON", true),
            ("a.b.c", b"AB", b"C", true),
            (".profile", b"PROFILE", b"", true),
            ("two words.txt", b"TWOWORDS", b"TXT", true),
            ("comma,here", b"COMMA_HE", b"", true),
            ("ninechars", b"NINECHAR", b"", true),
        ];
        for (name, base, extension, lossy) in cases {
            let b = basis(name);
            assert_eq!(b.base, base, "{name}: base");
            assert_eq!(b.extension, extension, "{name}: extension");
            assert_eq!(b.lossy, lossy, "{name}: lossy");
        }
    }

    #[test]
    fn a_long_tail_takes_its_room_from_the_base() {
        // Eleven names sharing a six-character prefix: the tenth and eleventh
        // tails are three characters, leaving five of the base.
        let mut entries: Vec<Entry> = (0..11)
            .map(|i| Entry::new(&format!("prefix-number-{i}.x"), Target::File(i)))
            .collect();
        assign_short_names(&mut entries).unwrap();
        assert_eq!(&entries[0].short, b"PREFIX~1X  ");
        assert_eq!(&entries[8].short, b"PREFIX~9X  ");
        assert_eq!(&entries[9].short, b"PREFI~10X  ");
        assert_eq!(&entries[10].short, b"PREFI~11X  ");
    }

    #[test]
    fn long_entries_are_stored_last_piece_first() {
        let entries = long_entries("limine.conf", 0x5A);
        assert_eq!(entries.len(), 1);
        let e = entries[0];
        assert_eq!(e[0], 0x41, "sequence 1, flagged last");
        assert_eq!(e[DIR_ATTR], 0x0F);
        assert_eq!(e[12], 0);
        assert_eq!(e[LDIR_CHECKSUM], 0x5A);
        assert_eq!(&e[1..11], b"l\0i\0m\0i\0n\0");
        assert_eq!(&e[14..26], b"e\0.\0c\0o\0n\0f\0");
        // The terminator, then padding.
        assert_eq!(&e[28..32], &[0x00, 0x00, 0xFF, 0xFF]);

        let two = long_entries("fourteen.chars", 1);
        assert_eq!(two.len(), 2);
        assert_eq!(two[0][0], 0x42);
        assert_eq!(two[1][0], 0x01);
        assert_eq!(
            &two[0][1..5],
            &[b's', 0, 0, 0],
            "the 14th character, then the terminator"
        );
        assert_eq!(&two[1][1..3], b"f\0");
    }

    #[test]
    fn the_fat_size_never_under_provisions() {
        // Across sizes around the FAT32 minimum and well beyond it.
        for total in (60_000u32..200_000).step_by(37).chain([1 << 22, 1 << 26]) {
            let g = geometry(total);
            assert!(
                u64::from(g.fat_sectors) * u64::from(FAT_ENTRIES_PER_SECTOR)
                    >= u64::from(g.clusters) + 2,
                "{total} sectors: {g:?}"
            );
            assert_eq!(
                g.first_data_sector + g.clusters * SECTORS_PER_CLUSTER,
                total
            );
        }
        assert_eq!(geometry(0).clusters, 0);
        assert_eq!(geometry(RESERVED_SECTORS).clusters, 0);
    }

    #[test]
    fn a_volume_that_would_not_be_fat32_is_refused() {
        // 32 MiB at one sector per cluster is below the bound.
        let mut small = vec![0u8; 32 * 1024 * 1024];
        assert_eq!(
            write_volume(&mut small, 0, &[]),
            Err(Error::VolumeSize {
                bytes: 32 * 1024 * 1024
            })
        );
        let mut ragged = vec![0u8; 34 * 1024 * 1024 + 1];
        assert!(matches!(
            write_volume(&mut ragged, 0, &[]),
            Err(Error::VolumeSize { .. })
        ));
    }
}
