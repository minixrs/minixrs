// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! A GUID Partition Table parser (slice 6.4, spec P5).
//!
//! Pure: no I/O, no `unsafe`, no allocation. A block driver reads the first
//! [`TABLE_SECTORS`] sectors of the disk however it reads anything else and
//! hands the bytes here. Sectors are 512 bytes ([`SECTOR_SIZE`]); nothing else
//! is supported.
//!
//! The intended sequence, over a buffer `disk` holding LBA 0 onwards:
//!
//! 1. [`parse_header`] over `disk[HEADER_OFFSET..]`, with the device's capacity;
//! 2. [`check_entries`] over `disk[ENTRIES_OFFSET..]`, the entry array's CRC;
//! 3. [`partition`] for each `n` in `1..=header.entries`, and [`find_type`] for
//!    the partition a type GUID selects.
//!
//! Step 3's functions do **not** re-check the entry array's CRC: call
//! [`check_entries`] first and stop if it refuses.
//!
//! # Partitions are numbered from 1
//!
//! Everywhere in this module a partition number is its **1-based position in
//! the entry array**: the first 128-byte entry is partition 1. It is a slot
//! number, not a count of used entries — if slots 1 and 3 are used and slot 2
//! is empty, the second partition on the disk is still partition 3. `0` is
//! never a valid partition number. (`virtio-blk` serves partition `n` as minor
//! `1 + n`.)
//!
//! # Only the primary header
//!
//! The header at LBA 1 is the only one consulted. A disk whose primary header
//! or primary entry array is damaged has no partitions as far as this module
//! is concerned; the backup copy at the end of the disk is never read, and
//! `AlternateLBA` is not interpreted. Recovering from the backup means
//! deciding which copy to believe and writing the repair — a tool's job.
//!
//! # The order of the header checks
//!
//! [`parse_header`] refuses for the first of these that fails:
//!
//! 1. the slice holds a whole sector ([`GptError::HeaderShort`]);
//! 2. the signature is `"EFI PART"` ([`GptError::Signature`]) — first, so a
//!    disk with no GPT at all reads as that and not as a bad checksum;
//! 3. the revision is 1.0 ([`GptError::Revision`]);
//! 4. the header size is 92..=512 ([`GptError::HeaderSize`]) — before the CRC
//!    because it is the CRC's length;
//! 5. the header CRC-32 ([`GptError::Crc`]) — before any field it protects is
//!    interpreted. (Signature, revision and size are covered by the CRC too,
//!    but they have to be read to know what to checksum and how much.)
//! 6. `MyLBA == 1` ([`GptError::MyLba`]);
//! 7. the entry size is 128 ([`GptError::EntrySize`]);
//! 8. the entry count is 1..=128 ([`GptError::EntryCount`]);
//! 9. the entry array starts at LBA 2 ([`GptError::EntryLba`]);
//! 10. `FirstUsableLBA <= LastUsableLBA < disk_sectors`
//!     ([`GptError::UsableRange`]).
//!
//! All arithmetic is `checked_*`: drivers ship with overflow checks off.

/// Bytes in a sector. The only sector size this parser understands.
pub const SECTOR_SIZE: usize = 512;
/// LBA of the primary GPT header.
pub const HEADER_LBA: u64 = 1;
/// LBA at which the primary partition entry array must start.
pub const ENTRIES_LBA: u64 = 2;
/// Bytes in one partition entry. The only entry size this parser accepts.
pub const ENTRY_SIZE: usize = 128;
/// Most partition entries a header may declare.
pub const MAX_ENTRIES: u32 = 128;
/// Bytes in a full [`MAX_ENTRIES`]-entry array: 128 × 128 = 16 384.
pub const MAX_ENTRIES_BYTES: usize = 16_384;
/// Sectors from the start of the disk that cover the protective MBR, the
/// header and a full entry array: LBA 0..=33, that is 1 + 1 + 32 = 34.
pub const TABLE_SECTORS: u64 = 34;
/// [`TABLE_SECTORS`] in bytes: 34 × 512 = 17 408.
pub const TABLE_BYTES: usize = 17_408;
/// Byte offset of the header in a buffer that starts at LBA 0: 1 × 512.
pub const HEADER_OFFSET: usize = 512;
/// Byte offset of the entry array in a buffer that starts at LBA 0: 2 × 512.
pub const ENTRIES_OFFSET: usize = 1024;

/// The smallest header the UEFI specification defines (revision 1.0).
const HEADER_SIZE_MIN: u32 = 92;
/// `"EFI PART"`.
const SIGNATURE: &[u8; 8] = b"EFI PART";
/// Revision 1.0, the only one there has ever been.
const REVISION_1_0: u32 = 0x0001_0000;
/// The reflected IEEE 802.3 polynomial.
const CRC_POLY: u32 = 0xEDB8_8320;
/// The CRC's initial value, and the value XORed into the result.
const CRC_INIT: u32 = 0xFFFF_FFFF;

// Header field offsets (UEFI 2.x, table "GPT Header").
const H_SIGNATURE: usize = 0;
const H_REVISION: usize = 8;
const H_SIZE: usize = 12;
const H_CRC: usize = 16;
const H_AFTER_CRC: usize = 20;
const H_MY_LBA: usize = 24;
const H_FIRST_USABLE: usize = 40;
const H_LAST_USABLE: usize = 48;
const H_ENTRY_LBA: usize = 72;
const H_ENTRIES: usize = 80;
const H_ENTRY_SIZE: usize = 84;
const H_ENTRIES_CRC: usize = 88;

// Partition entry field offsets (table "GPT Partition Entry").
const E_TYPE_GUID: usize = 0;
const E_FIRST_LBA: usize = 32;
const E_LAST_LBA: usize = 40;

// The derived constants above are typed out so a reader sees the numbers;
// these hold them to the ones they derive from.
const _: () = assert!(MAX_ENTRIES_BYTES == MAX_ENTRIES as usize * ENTRY_SIZE);
const _: () = assert!(HEADER_OFFSET == HEADER_LBA as usize * SECTOR_SIZE);
const _: () = assert!(ENTRIES_OFFSET == ENTRIES_LBA as usize * SECTOR_SIZE);
const _: () = assert!(TABLE_BYTES == TABLE_SECTORS as usize * SECTOR_SIZE);
const _: () = assert!(TABLE_BYTES == ENTRIES_OFFSET + MAX_ENTRIES_BYTES);
const _: () = assert!(HEADER_SIZE_MIN as usize <= SECTOR_SIZE);

/// Why a partition table was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GptError {
    /// The header slice is shorter than one sector.
    HeaderShort,
    /// The signature is not `"EFI PART"`.
    Signature,
    /// The revision is not 1.0.
    Revision,
    /// The header size is below 92 or above one sector.
    HeaderSize,
    /// The header's CRC-32 does not match its contents.
    Crc,
    /// `MyLBA` is not 1.
    MyLba,
    /// The partition entry size is not 128.
    EntrySize,
    /// The header declares no entries, or more than [`MAX_ENTRIES`].
    EntryCount,
    /// The partition entry array does not start at LBA 2.
    EntryLba,
    /// Not `FirstUsableLBA <= LastUsableLBA < disk_sectors`.
    UsableRange,
    /// The entry array's CRC-32 does not match its contents.
    EntriesCrc,
    /// The entry-array slice is shorter than the header's entry count needs.
    EntriesShort,
    /// A used entry's range is inverted or leaves the usable area. This
    /// condemns the whole table, not the one entry.
    EntryRange,
    /// A partition number that is 0 or above the header's entry count: a
    /// caller's mistake, not a property of the disk.
    EntryIndex,
}

impl GptError {
    /// A short, stable, lowercase description, for a `root FAIL gpt <why>`
    /// boot marker. These strings are matched by the marker files: changing
    /// one is a marker change.
    #[must_use]
    pub const fn why(self) -> &'static str {
        match self {
            Self::HeaderShort => "header short",
            Self::Signature => "signature",
            Self::Revision => "revision",
            Self::HeaderSize => "header size",
            Self::Crc => "crc",
            Self::MyLba => "my lba",
            Self::EntrySize => "entry size",
            Self::EntryCount => "entry count",
            Self::EntryLba => "entry lba",
            Self::UsableRange => "usable range",
            Self::EntriesCrc => "entries crc",
            Self::EntriesShort => "entries short",
            Self::EntryRange => "entry range",
            Self::EntryIndex => "entry index",
        }
    }
}

/// What [`parse_header`] learned from a header it accepted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
    /// Number of partition entries, used or not: 1..=[`MAX_ENTRIES`]. Valid
    /// partition numbers are `1..=entries`.
    pub entries: u32,
    /// The CRC-32 the header records for the entry array.
    pub entries_crc: u32,
    /// First LBA a partition may occupy.
    pub first_usable: u64,
    /// Last LBA a partition may occupy (inclusive); below the disk's size.
    pub last_usable: u64,
    /// Bytes in the entry array: `entries` × [`ENTRY_SIZE`], at most
    /// [`MAX_ENTRIES_BYTES`].
    pub entries_bytes: usize,
}

/// One used partition entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Partition {
    /// The partition type GUID, as its 16 on-disk bytes (GPT's mixed-endian
    /// encoding) — compare it with bytes in the same encoding.
    pub type_guid: [u8; 16],
    /// First LBA of the partition.
    pub first_lba: u64,
    /// Last LBA of the partition, **inclusive**, as on disk.
    pub last_lba: u64,
}

impl Partition {
    /// The partition's length in sectors, `last_lba - first_lba + 1`; `None`
    /// if the range is inverted or the count does not fit a `u64`. Never
    /// `None` for a `Partition` that [`partition`] returned.
    #[must_use]
    pub const fn sectors(&self) -> Option<u64> {
        match self.last_lba.checked_sub(self.first_lba) {
            Some(span) => span.checked_add(1),
            None => None,
        }
    }
}

/// What [`find_type`] found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lookup {
    /// No used entry carries the type.
    Absent,
    /// Exactly one used entry carries the type.
    Found {
        /// The partition's number: its **1-based** slot in the entry array.
        index: u32,
        /// The entry.
        partition: Partition,
    },
    /// More than one used entry carries the type. Deliberately carries no
    /// partition: there is no "first" one to fall back on.
    Ambiguous,
}

/// CRC-32 as GPT uses it: IEEE 802.3, reflected, polynomial `0xEDB88320`,
/// initial value and final XOR `0xFFFFFFFF`. `crc32(b"123456789")` is
/// `0xCBF43926`.
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    crc_update(CRC_INIT, bytes) ^ CRC_INIT
}

/// Feed `bytes` into a running (not yet inverted) CRC. Bit at a time: the
/// input is at most 16 KiB and is checked once per boot, and a lookup table would
/// cost every driver that links this crate a kilobyte.
fn crc_update(mut crc: u32, bytes: &[u8]) -> u32 {
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ CRC_POLY
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// The little-endian `u32` at `off`, if `bytes` reaches that far.
fn le32(bytes: &[u8], off: usize) -> Option<u32> {
    let raw = bytes.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes(raw.try_into().ok()?))
}

/// The little-endian `u64` at `off`, if `bytes` reaches that far.
fn le64(bytes: &[u8], off: usize) -> Option<u64> {
    let raw = bytes.get(off..off.checked_add(8)?)?;
    Some(u64::from_le_bytes(raw.try_into().ok()?))
}

/// Check the primary GPT header in `sector`, the 512 bytes of LBA 1 (a longer
/// slice is accepted and only its first 512 bytes are read). `disk_sectors` is
/// the device's capacity in 512-byte sectors.
///
/// The checks and their order are in the module documentation.
///
/// # Errors
///
/// The [`GptError`] of the first check that fails.
pub fn parse_header(sector: &[u8], disk_sectors: u64) -> Result<Header, GptError> {
    // Every read below is inside `s`, so a `None` cannot happen; mapping it to
    // `HeaderShort` keeps the function free of indexing all the same.
    const SHORT: GptError = GptError::HeaderShort;
    let s = sector.get(..SECTOR_SIZE).ok_or(SHORT)?;

    if s.get(H_SIGNATURE..H_REVISION) != Some(SIGNATURE.as_slice()) {
        return Err(GptError::Signature);
    }
    if le32(s, H_REVISION).ok_or(SHORT)? != REVISION_1_0 {
        return Err(GptError::Revision);
    }
    let size = le32(s, H_SIZE).ok_or(SHORT)?;
    if size < HEADER_SIZE_MIN {
        return Err(GptError::HeaderSize);
    }
    let size = usize::try_from(size).map_err(|_| GptError::HeaderSize)?;
    if size > SECTOR_SIZE {
        return Err(GptError::HeaderSize);
    }

    // The CRC covers `size` bytes with its own field taken as zero.
    let before = s.get(..H_CRC).ok_or(SHORT)?;
    let after = s.get(H_AFTER_CRC..size).ok_or(SHORT)?;
    let crc = crc_update(crc_update(crc_update(CRC_INIT, before), &[0; 4]), after) ^ CRC_INIT;
    if crc != le32(s, H_CRC).ok_or(SHORT)? {
        return Err(GptError::Crc);
    }

    if le64(s, H_MY_LBA).ok_or(SHORT)? != HEADER_LBA {
        return Err(GptError::MyLba);
    }
    let entry_size = le32(s, H_ENTRY_SIZE).ok_or(SHORT)?;
    if usize::try_from(entry_size) != Ok(ENTRY_SIZE) {
        return Err(GptError::EntrySize);
    }
    let entries = le32(s, H_ENTRIES).ok_or(SHORT)?;
    if entries == 0 || entries > MAX_ENTRIES {
        return Err(GptError::EntryCount);
    }
    let entries_bytes = usize::try_from(entries)
        .ok()
        .and_then(|n| n.checked_mul(ENTRY_SIZE))
        .ok_or(GptError::EntryCount)?;
    if le64(s, H_ENTRY_LBA).ok_or(SHORT)? != ENTRIES_LBA {
        return Err(GptError::EntryLba);
    }
    let first_usable = le64(s, H_FIRST_USABLE).ok_or(SHORT)?;
    let last_usable = le64(s, H_LAST_USABLE).ok_or(SHORT)?;
    if first_usable > last_usable || last_usable >= disk_sectors {
        return Err(GptError::UsableRange);
    }

    Ok(Header {
        entries,
        entries_crc: le32(s, H_ENTRIES_CRC).ok_or(SHORT)?,
        first_usable,
        last_usable,
        entries_bytes,
    })
}

/// Check the partition entry array's CRC-32. `bytes` starts at LBA 2 and holds
/// at least `header.entries_bytes` bytes; anything after that is ignored.
///
/// # Errors
///
/// [`GptError::EntriesShort`] if `bytes` is too short, [`GptError::EntriesCrc`]
/// if the checksum does not match the header's.
pub fn check_entries(bytes: &[u8], header: &Header) -> Result<(), GptError> {
    let array = bytes
        .get(..header.entries_bytes)
        .ok_or(GptError::EntriesShort)?;
    if crc32(array) != header.entries_crc {
        return Err(GptError::EntriesCrc);
    }
    Ok(())
}

/// Decode partition `n`, **counted from 1**: `n == 1` is the first entry of
/// the array. `bytes` is the entry array, as for [`check_entries`] (which this
/// does not repeat).
///
/// `Ok(None)` is an unused slot — an all-zero type GUID — whose other fields
/// are not looked at.
///
/// # Errors
///
/// - [`GptError::EntryIndex`] if `n` is 0 or above `header.entries`;
/// - [`GptError::EntriesShort`] if `bytes` is shorter than the array;
/// - [`GptError::EntryRange`] if the entry is used and its range is inverted
///   (`first > last`) or not inside `[first_usable, last_usable]`. The caller
///   treats that as a refusal of the whole table.
pub fn partition(bytes: &[u8], header: &Header, n: u32) -> Result<Option<Partition>, GptError> {
    const SHORT: GptError = GptError::EntriesShort;
    if n == 0 || n > header.entries {
        return Err(GptError::EntryIndex);
    }
    // The whole array has to be there, not just this entry: a caller that
    // skipped `check_entries` on a short read hears about it here.
    let array = bytes.get(..header.entries_bytes).ok_or(SHORT)?;
    let slot = usize::try_from(n)
        .ok()
        .and_then(|n| n.checked_sub(1))
        .ok_or(SHORT)?;
    let start = slot.checked_mul(ENTRY_SIZE).ok_or(SHORT)?;
    let end = start.checked_add(ENTRY_SIZE).ok_or(SHORT)?;
    let entry = array.get(start..end).ok_or(SHORT)?;

    let type_guid: [u8; 16] = entry
        .get(E_TYPE_GUID..E_TYPE_GUID + 16)
        .and_then(|g| g.try_into().ok())
        .ok_or(SHORT)?;
    if type_guid == [0; 16] {
        return Ok(None);
    }
    let first_lba = le64(entry, E_FIRST_LBA).ok_or(SHORT)?;
    let last_lba = le64(entry, E_LAST_LBA).ok_or(SHORT)?;
    if first_lba > last_lba || first_lba < header.first_usable || last_lba > header.last_usable {
        return Err(GptError::EntryRange);
    }
    Ok(Some(Partition {
        type_guid,
        first_lba,
        last_lba,
    }))
}

/// Find the one partition whose type GUID is `guid` (16 on-disk bytes).
///
/// Every entry is decoded, so a malformed entry anywhere in the table is an
/// error even if the wanted partition was already seen. Unused slots never
/// match, so looking up the all-zero GUID is [`Lookup::Absent`].
///
/// # Errors
///
/// Whatever [`partition`] returns for any entry.
pub fn find_type(bytes: &[u8], header: &Header, guid: &[u8; 16]) -> Result<Lookup, GptError> {
    let mut found = Lookup::Absent;
    let mut n = 1u32;
    while n <= header.entries {
        if let Some(p) = partition(bytes, header, n)?
            && p.type_guid == *guid
        {
            found = match found {
                Lookup::Absent => Lookup::Found {
                    index: n,
                    partition: p,
                },
                Lookup::Found { .. } | Lookup::Ambiguous => Lookup::Ambiguous,
            };
        }
        // `entries == u32::MAX` (a hand-made `Header`) would otherwise wrap.
        match n.checked_add(1) {
            Some(next) => n = next,
            None => break,
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The disk the slice's plan describes: 75 776 sectors (37 MiB).
    const DISK: u64 = 75_776;
    /// LBA 0..=33 hold the MBR, header and entries, so 34 is the first free.
    const FIRST_USABLE: u64 = 34;
    /// The backup entries take 32 sectors and the backup header 1, at the end:
    /// 75 776 - 33 = 75 743 is the first of them, so 75 742 is the last usable.
    const LAST_USABLE: u64 = 75_742;

    const ESP: [u8; 16] = [0xC1; 16];
    const ROOT: [u8; 16] = [0x52; 16];
    const OTHER: [u8; 16] = [0x07; 16];

    /// A second CRC-32, table-driven, so the test builder does not seal its
    /// headers with the code under test.
    fn ref_crc(bytes: &[u8]) -> u32 {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut c = u32::try_from(i).unwrap();
            for _ in 0..8 {
                c = if c & 1 == 1 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        let mut crc = 0xFFFF_FFFFu32;
        for &b in bytes {
            let idx = usize::try_from((crc ^ u32::from(b)) & 0xFF).unwrap();
            crc = table[idx] ^ (crc >> 8);
        }
        crc ^ 0xFFFF_FFFF
    }

    /// Every header field, by hand, at the UEFI specification's offsets.
    #[derive(Clone)]
    struct Hdr {
        signature: [u8; 8],
        revision: u32,
        size: u32,
        my_lba: u64,
        alternate_lba: u64,
        first_usable: u64,
        last_usable: u64,
        disk_guid: [u8; 16],
        entry_lba: u64,
        entries: u32,
        entry_size: u32,
        entries_crc: u32,
        /// Byte 92, reserved; inside the CRC only when `size > 92`.
        reserved_92: u8,
    }

    impl Hdr {
        fn good() -> Self {
            Self {
                signature: *b"EFI PART",
                revision: 0x0001_0000,
                size: 92,
                my_lba: 1,
                alternate_lba: DISK - 1,
                first_usable: FIRST_USABLE,
                last_usable: LAST_USABLE,
                disk_guid: [0xD1; 16],
                entry_lba: 2,
                entries: 128,
                entry_size: 128,
                entries_crc: 0,
                reserved_92: 0,
            }
        }

        /// A good header describing `array`.
        fn over(array: &[u8]) -> Self {
            let mut h = Self::good();
            h.entries = u32::try_from(array.len() / 128).unwrap();
            h.entries_crc = ref_crc(array);
            h
        }

        /// The sector with the CRC field left zero.
        fn unsealed(&self) -> [u8; 512] {
            let mut s = [0u8; 512];
            s[0..8].copy_from_slice(&self.signature);
            s[8..12].copy_from_slice(&self.revision.to_le_bytes());
            s[12..16].copy_from_slice(&self.size.to_le_bytes());
            // 16..20 header CRC, 20..24 reserved.
            s[24..32].copy_from_slice(&self.my_lba.to_le_bytes());
            s[32..40].copy_from_slice(&self.alternate_lba.to_le_bytes());
            s[40..48].copy_from_slice(&self.first_usable.to_le_bytes());
            s[48..56].copy_from_slice(&self.last_usable.to_le_bytes());
            s[56..72].copy_from_slice(&self.disk_guid);
            s[72..80].copy_from_slice(&self.entry_lba.to_le_bytes());
            s[80..84].copy_from_slice(&self.entries.to_le_bytes());
            s[84..88].copy_from_slice(&self.entry_size.to_le_bytes());
            s[88..92].copy_from_slice(&self.entries_crc.to_le_bytes());
            s[92] = self.reserved_92;
            s
        }

        /// The sector with a **correct** CRC over `size` bytes (clamped to the
        /// sector, for the one test whose size is too large to checksum).
        fn sector(&self) -> [u8; 512] {
            let mut s = self.unsealed();
            let n = usize::try_from(self.size).unwrap().min(512);
            let crc = ref_crc(&s[..n]);
            s[16..20].copy_from_slice(&crc.to_le_bytes());
            s
        }

        fn parse(&self) -> Result<Header, GptError> {
            parse_header(&self.sector(), DISK)
        }
    }

    /// An array of `slots` entries; each `(n, type, first, last)` fills the
    /// 1-based slot `n`.
    fn array(slots: usize, used: &[(usize, [u8; 16], u64, u64)]) -> Vec<u8> {
        let mut a = vec![0u8; slots * 128];
        for &(n, guid, first, last) in used {
            let e = &mut a[(n - 1) * 128..n * 128];
            e[0..16].copy_from_slice(&guid);
            e[16..32].copy_from_slice(&[0xEE; 16]); // unique GUID, not read
            e[32..40].copy_from_slice(&first.to_le_bytes());
            e[40..48].copy_from_slice(&last.to_le_bytes());
        }
        a
    }

    fn parsed(array: &[u8]) -> Header {
        Hdr::over(array).parse().unwrap()
    }

    // ---- CRC-32 ------------------------------------------------------------

    #[test]
    fn crc32_published_vectors() {
        // The CRC catalogue's check value for CRC-32/ISO-HDLC.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    #[test]
    fn the_test_builders_crc_is_a_crc32_too() {
        assert_eq!(ref_crc(b"123456789"), 0xCBF4_3926);
    }

    // ---- constants ---------------------------------------------------------

    #[test]
    fn constants_cover_lba_0_to_33() {
        // 128 entries × 128 bytes = 16 384 bytes = 32 sectors; with the MBR
        // and the header that is 34 sectors, 34 × 512 = 17 408 bytes.
        for (name, got, want) in [
            ("sectors", usize::try_from(TABLE_SECTORS).unwrap(), 34),
            ("bytes", TABLE_BYTES, 17_408),
            ("entries", MAX_ENTRIES_BYTES, 16_384),
            ("header at", HEADER_OFFSET, 512),
            ("entries at", ENTRIES_OFFSET, 1024),
        ] {
            assert_eq!(got, want, "{name}");
        }
    }

    // ---- header ------------------------------------------------------------

    #[test]
    fn a_good_header_parses() {
        let mut h = Hdr::good();
        h.entries_crc = 0xDEAD_BEEF;
        assert_eq!(
            h.parse(),
            Ok(Header {
                entries: 128,
                entries_crc: 0xDEAD_BEEF,
                first_usable: 34,
                last_usable: 75_742,
                entries_bytes: 16_384, // 128 × 128
            })
        );
    }

    #[test]
    fn a_longer_slice_is_read_as_its_first_sector() {
        let mut buf = vec![0xFFu8; 1024];
        buf[..512].copy_from_slice(&Hdr::good().sector());
        assert!(parse_header(&buf, DISK).is_ok());
    }

    #[test]
    fn a_short_slice_is_header_short() {
        let s = Hdr::good().sector();
        assert_eq!(parse_header(&s[..511], DISK), Err(GptError::HeaderShort));
        assert_eq!(parse_header(&[], DISK), Err(GptError::HeaderShort));
    }

    #[test]
    fn refuses_signature() {
        let mut h = Hdr::good();
        h.signature = *b"EFI PARU";
        assert_eq!(h.parse(), Err(GptError::Signature));
    }

    #[test]
    fn refuses_revision() {
        let mut h = Hdr::good();
        h.revision = 0x0001_0001;
        assert_eq!(h.parse(), Err(GptError::Revision));
    }

    #[test]
    fn refuses_header_size_below_92() {
        let mut h = Hdr::good();
        h.size = 91;
        assert_eq!(h.parse(), Err(GptError::HeaderSize));
    }

    #[test]
    fn refuses_header_size_above_a_sector() {
        let mut h = Hdr::good();
        h.size = 513;
        assert_eq!(h.parse(), Err(GptError::HeaderSize));
        h.size = u32::MAX;
        assert_eq!(h.parse(), Err(GptError::HeaderSize));
    }

    #[test]
    fn a_whole_sector_header_is_checksummed_whole() {
        let mut h = Hdr::good();
        h.size = 512;
        h.reserved_92 = 0x5A;
        assert!(h.parse().is_ok());
        // Byte 92 is inside a 512-byte header's CRC: changing it alone, with
        // the CRC left as it was, is a refusal...
        let mut s = h.sector();
        s[92] ^= 1;
        assert_eq!(parse_header(&s, DISK), Err(GptError::Crc));
        // ...and outside a 92-byte header's.
        h.size = 92;
        let mut s = h.sector();
        s[92] ^= 1;
        assert!(parse_header(&s, DISK).is_ok());
    }

    #[test]
    fn refuses_a_stale_crc_alone() {
        // Every field is acceptable; the disk GUID changed after sealing.
        let mut s = Hdr::good().sector();
        s[56] ^= 0x01;
        assert_eq!(parse_header(&s, DISK), Err(GptError::Crc));
    }

    #[test]
    fn refuses_a_wrong_crc_field() {
        let mut s = Hdr::good().sector();
        s[19] ^= 0x80;
        assert_eq!(parse_header(&s, DISK), Err(GptError::Crc));
    }

    #[test]
    fn the_crc_field_is_zeroed_for_the_checksum_not_skipped() {
        // Sealing over the 88 bytes that remain when the field is cut out,
        // rather than over 92 with it zeroed, must not be accepted.
        let h = Hdr::good();
        let mut s = h.unsealed();
        let mut cut = Vec::new();
        cut.extend_from_slice(&s[..16]);
        cut.extend_from_slice(&s[20..92]);
        s[16..20].copy_from_slice(&ref_crc(&cut).to_le_bytes());
        assert_eq!(parse_header(&s, DISK), Err(GptError::Crc));
    }

    #[test]
    fn refuses_my_lba() {
        let mut h = Hdr::good();
        h.my_lba = 2;
        assert_eq!(h.parse(), Err(GptError::MyLba));
        // The backup header's own MyLBA: a backup copied to LBA 1 is refused.
        h.my_lba = DISK - 1;
        assert_eq!(h.parse(), Err(GptError::MyLba));
    }

    #[test]
    fn refuses_entry_size() {
        let mut h = Hdr::good();
        h.entry_size = 256;
        assert_eq!(h.parse(), Err(GptError::EntrySize));
        h.entry_size = 0;
        assert_eq!(h.parse(), Err(GptError::EntrySize));
    }

    #[test]
    fn refuses_entry_count() {
        let mut h = Hdr::good();
        h.entries = 129;
        assert_eq!(h.parse(), Err(GptError::EntryCount));
        h.entries = 0;
        assert_eq!(h.parse(), Err(GptError::EntryCount));
        h.entries = u32::MAX;
        assert_eq!(h.parse(), Err(GptError::EntryCount));
    }

    #[test]
    fn accepts_the_entry_count_bounds() {
        let mut h = Hdr::good();
        h.entries = 1;
        assert_eq!(h.parse().unwrap().entries_bytes, 128);
        h.entries = 128;
        assert_eq!(h.parse().unwrap().entries_bytes, 16_384);
    }

    #[test]
    fn refuses_entry_lba() {
        let mut h = Hdr::good();
        h.entry_lba = 3;
        assert_eq!(h.parse(), Err(GptError::EntryLba));
        // Where the backup array lives (75 776 - 33).
        h.entry_lba = 75_743;
        assert_eq!(h.parse(), Err(GptError::EntryLba));
    }

    #[test]
    fn refuses_an_inverted_usable_range() {
        let mut h = Hdr::good();
        h.first_usable = LAST_USABLE + 1;
        assert_eq!(h.parse(), Err(GptError::UsableRange));
    }

    #[test]
    fn refuses_a_usable_range_past_the_disk() {
        let mut h = Hdr::good();
        // The last sector of the disk is DISK - 1; DISK itself is one past.
        h.last_usable = DISK;
        assert_eq!(h.parse(), Err(GptError::UsableRange));
        h.last_usable = u64::MAX;
        assert_eq!(h.parse(), Err(GptError::UsableRange));
        h.first_usable = u64::MAX;
        assert_eq!(h.parse(), Err(GptError::UsableRange));
    }

    #[test]
    fn accepts_the_usable_range_bounds() {
        let mut h = Hdr::good();
        h.last_usable = DISK - 1;
        assert_eq!(h.parse().unwrap().last_usable, 75_775);
        h.first_usable = DISK - 1; // a one-sector usable area
        assert_eq!(h.parse().unwrap().first_usable, 75_775);
    }

    #[test]
    fn the_same_header_is_refused_on_a_smaller_disk() {
        let s = Hdr::good().sector();
        // last_usable is 75 742: a disk of 75 743 sectors just holds it.
        assert!(parse_header(&s, 75_743).is_ok());
        assert_eq!(parse_header(&s, 75_742), Err(GptError::UsableRange));
        assert_eq!(parse_header(&s, 0), Err(GptError::UsableRange));
    }

    #[test]
    fn signature_is_reported_ahead_of_a_bad_crc() {
        let mut s = Hdr::good().sector();
        s[0] = b'X'; // also makes the CRC stale
        assert_eq!(parse_header(&s, DISK), Err(GptError::Signature));
        assert_eq!(parse_header(&[0u8; 512], DISK), Err(GptError::Signature));
    }

    #[test]
    fn a_bad_field_under_a_stale_crc_is_crc() {
        // The CRC is checked before the fields it protects are interpreted.
        for at in [24usize, 40, 48, 72, 80, 84] {
            let mut s = Hdr::good().sector();
            s[at] ^= 0xFF;
            assert_eq!(parse_header(&s, DISK), Err(GptError::Crc), "offset {at}");
        }
    }

    #[test]
    fn alternate_lba_is_not_interpreted() {
        let mut h = Hdr::good();
        h.alternate_lba = u64::MAX;
        assert!(h.parse().is_ok());
    }

    // ---- entry array -------------------------------------------------------

    #[test]
    fn check_entries_accepts_a_matching_crc() {
        let a = array(4, &[(1, ESP, 2048, 71_679)]);
        assert_eq!(check_entries(&a, &parsed(&a)), Ok(()));
    }

    #[test]
    fn check_entries_ignores_bytes_after_the_array() {
        let a = array(4, &[(1, ESP, 2048, 71_679)]);
        let h = parsed(&a);
        let mut long = a.clone();
        long.extend_from_slice(&[0xFF; 300]);
        assert_eq!(check_entries(&long, &h), Ok(()));
    }

    #[test]
    fn check_entries_refuses_a_crc_mismatch() {
        let mut a = array(4, &[(1, ESP, 2048, 71_679)]);
        let h = parsed(&a);
        a[4 * 128 - 1] ^= 1; // last byte of the last entry
        assert_eq!(check_entries(&a, &h), Err(GptError::EntriesCrc));
    }

    #[test]
    fn check_entries_refuses_a_short_slice() {
        let a = array(4, &[(1, ESP, 2048, 71_679)]);
        let h = parsed(&a);
        // 4 × 128 = 512 bytes are needed.
        assert_eq!(check_entries(&a[..511], &h), Err(GptError::EntriesShort));
        assert_eq!(check_entries(&[], &h), Err(GptError::EntriesShort));
    }

    #[test]
    fn check_entries_over_a_full_array() {
        let a = array(128, &[(1, ESP, 2048, 71_679), (128, ROOT, 71_680, 73_727)]);
        assert_eq!(a.len(), 16_384);
        assert_eq!(check_entries(&a, &parsed(&a)), Ok(()));
    }

    // ---- partition ---------------------------------------------------------

    #[test]
    fn partition_decodes_a_used_entry_and_counts_from_one() {
        let a = array(4, &[(1, ESP, 2048, 71_679), (2, ROOT, 71_680, 73_727)]);
        let h = parsed(&a);
        assert_eq!(
            partition(&a, &h, 1),
            Ok(Some(Partition {
                type_guid: ESP,
                first_lba: 2048,
                last_lba: 71_679
            }))
        );
        assert_eq!(
            partition(&a, &h, 2),
            Ok(Some(Partition {
                type_guid: ROOT,
                first_lba: 71_680,
                last_lba: 73_727
            }))
        );
    }

    #[test]
    fn partition_zero_and_past_the_count_are_entry_index() {
        let a = array(4, &[(1, ESP, 2048, 71_679)]);
        let h = parsed(&a);
        assert_eq!(partition(&a, &h, 0), Err(GptError::EntryIndex));
        assert_eq!(partition(&a, &h, 4), Ok(None));
        assert_eq!(partition(&a, &h, 5), Err(GptError::EntryIndex));
        assert_eq!(partition(&a, &h, u32::MAX), Err(GptError::EntryIndex));
    }

    #[test]
    fn partition_past_the_count_is_refused_even_if_the_bytes_are_there() {
        // The header says 2 entries; the buffer happens to hold more.
        let a = array(2, &[(1, ESP, 2048, 71_679)]);
        let h = parsed(&a);
        let mut long = a.clone();
        long.extend_from_slice(&array(1, &[(1, ROOT, 71_680, 73_727)]));
        assert_eq!(partition(&long, &h, 3), Err(GptError::EntryIndex));
    }

    #[test]
    fn partition_on_a_short_slice_is_entries_short() {
        let a = array(4, &[(1, ESP, 2048, 71_679)]);
        let h = parsed(&a);
        // Entry 1 is all there, but the array is not.
        assert_eq!(partition(&a[..511], &h, 1), Err(GptError::EntriesShort));
    }

    #[test]
    fn an_unused_slot_is_none_whatever_else_it_holds() {
        // A zero type GUID with a nonsense range: unused, and not an error.
        let a = array(4, &[(2, [0; 16], 9, 3)]);
        let h = parsed(&a);
        assert_eq!(partition(&a, &h, 1), Ok(None));
        assert_eq!(partition(&a, &h, 2), Ok(None));
    }

    #[test]
    fn a_type_guid_with_one_nonzero_byte_is_used() {
        let mut guid = [0u8; 16];
        guid[15] = 1;
        let a = array(2, &[(1, guid, 2048, 2048)]);
        let h = parsed(&a);
        assert_eq!(
            partition(&a, &h, 1),
            Ok(Some(Partition {
                type_guid: guid,
                first_lba: 2048,
                last_lba: 2048
            }))
        );
    }

    #[test]
    fn an_inverted_entry_is_entry_range() {
        let a = array(4, &[(1, ESP, 2049, 2048)]);
        assert_eq!(partition(&a, &parsed(&a), 1), Err(GptError::EntryRange));
    }

    #[test]
    fn an_entry_outside_the_usable_area_is_entry_range() {
        // Starts one before the first usable LBA (34).
        let a = array(4, &[(1, ESP, 33, 2048)]);
        assert_eq!(partition(&a, &parsed(&a), 1), Err(GptError::EntryRange));
        // Ends one after the last usable LBA (75 742).
        let a = array(4, &[(1, ESP, 2048, 75_743)]);
        assert_eq!(partition(&a, &parsed(&a), 1), Err(GptError::EntryRange));
        // Wholly beyond it.
        let a = array(4, &[(1, ESP, 80_000, 90_000)]);
        assert_eq!(partition(&a, &parsed(&a), 1), Err(GptError::EntryRange));
    }

    #[test]
    fn an_entry_filling_the_usable_area_is_accepted() {
        let a = array(4, &[(1, ESP, 34, 75_742)]);
        let p = partition(&a, &parsed(&a), 1).unwrap().unwrap();
        // 75 742 - 34 + 1 = 75 709.
        assert_eq!(p.sectors(), Some(75_709));
    }

    #[test]
    fn extreme_lbas_are_refused_without_overflow() {
        for (first, last) in [
            (2048, u64::MAX),
            (u64::MAX, u64::MAX),
            (0, u64::MAX),
            (u64::MAX, 0),
            (0, 0),
        ] {
            let a = array(4, &[(1, ESP, first, last)]);
            assert_eq!(
                partition(&a, &parsed(&a), 1),
                Err(GptError::EntryRange),
                "{first}..={last}"
            );
        }
    }

    // ---- sectors -----------------------------------------------------------

    #[test]
    fn sectors_is_inclusive_and_checked() {
        let p = |first_lba, last_lba| Partition {
            type_guid: ESP,
            first_lba,
            last_lba,
        };
        assert_eq!(p(2048, 2048).sectors(), Some(1));
        // The ESP: 71 679 - 2048 + 1 = 69 632 (34 MiB).
        assert_eq!(p(2048, 71_679).sectors(), Some(69_632));
        // The root: 73 727 - 71 680 + 1 = 2048.
        assert_eq!(p(71_680, 73_727).sectors(), Some(2048));
        // Inverted.
        assert_eq!(p(2, 1).sectors(), None);
        // u64::MAX - 0 + 1 does not fit.
        assert_eq!(p(0, u64::MAX).sectors(), None);
        // u64::MAX - 1 + 1 just does.
        assert_eq!(p(1, u64::MAX).sectors(), Some(u64::MAX));
        assert_eq!(p(u64::MAX, u64::MAX).sectors(), Some(1));
    }

    // ---- find_type ---------------------------------------------------------

    #[test]
    fn find_type_finds_the_one_match() {
        let a = array(8, &[(1, ESP, 2048, 71_679), (2, ROOT, 71_680, 73_727)]);
        let h = parsed(&a);
        assert_eq!(
            find_type(&a, &h, &ROOT),
            Ok(Lookup::Found {
                index: 2,
                partition: Partition {
                    type_guid: ROOT,
                    first_lba: 71_680,
                    last_lba: 73_727
                },
            })
        );
        assert_eq!(
            find_type(&a, &h, &ESP),
            Ok(Lookup::Found {
                index: 1,
                partition: Partition {
                    type_guid: ESP,
                    first_lba: 2048,
                    last_lba: 71_679
                },
            })
        );
    }

    #[test]
    fn find_type_reports_the_slot_not_the_count_of_used_entries() {
        // Slots 1 and 3 used, slot 2 empty: the root is the second partition
        // on the disk and is partition 3.
        let a = array(8, &[(1, ESP, 2048, 71_679), (3, ROOT, 71_680, 73_727)]);
        let h = parsed(&a);
        assert_eq!(partition(&a, &h, 2), Ok(None));
        assert_eq!(
            find_type(&a, &h, &ROOT),
            Ok(Lookup::Found {
                index: 3,
                partition: Partition {
                    type_guid: ROOT,
                    first_lba: 71_680,
                    last_lba: 73_727
                },
            })
        );
    }

    #[test]
    fn find_type_reaches_the_last_slot() {
        let a = array(128, &[(128, ROOT, 71_680, 73_727)]);
        let h = parsed(&a);
        assert!(matches!(
            find_type(&a, &h, &ROOT),
            Ok(Lookup::Found { index: 128, .. })
        ));
    }

    #[test]
    fn find_type_absent() {
        let a = array(8, &[(1, ESP, 2048, 71_679)]);
        let h = parsed(&a);
        assert_eq!(find_type(&a, &h, &ROOT), Ok(Lookup::Absent));
        // An empty table.
        let a = array(8, &[]);
        assert_eq!(find_type(&a, &parsed(&a), &ROOT), Ok(Lookup::Absent));
    }

    #[test]
    fn find_type_never_matches_unused_slots() {
        let a = array(8, &[(1, ESP, 2048, 71_679)]);
        assert_eq!(find_type(&a, &parsed(&a), &[0; 16]), Ok(Lookup::Absent));
    }

    #[test]
    fn find_type_ambiguous() {
        let a = array(
            8,
            &[
                (1, ESP, 2048, 71_679),
                (2, ROOT, 71_680, 73_727),
                (5, ROOT, 73_728, 73_735),
            ],
        );
        let h = parsed(&a);
        assert_eq!(find_type(&a, &h, &ROOT), Ok(Lookup::Ambiguous));
        // The other type is still unambiguous.
        assert!(matches!(
            find_type(&a, &h, &ESP),
            Ok(Lookup::Found { index: 1, .. })
        ));
    }

    #[test]
    fn find_type_three_matches_are_still_ambiguous() {
        let a = array(
            8,
            &[
                (1, ROOT, 2048, 2055),
                (2, ROOT, 2056, 2063),
                (3, ROOT, 2064, 2071),
            ],
        );
        assert_eq!(find_type(&a, &parsed(&a), &ROOT), Ok(Lookup::Ambiguous));
    }

    #[test]
    fn find_type_propagates_a_bad_entry_of_another_type() {
        // The root is fine and comes first; a later, unrelated entry is
        // inverted. The table is refused, not searched around.
        let a = array(8, &[(1, ROOT, 71_680, 73_727), (4, OTHER, 5000, 4000)]);
        let h = parsed(&a);
        assert_eq!(find_type(&a, &h, &ROOT), Err(GptError::EntryRange));
        assert_eq!(find_type(&a, &h, &ESP), Err(GptError::EntryRange));
    }

    #[test]
    fn find_type_a_bad_entry_outranks_ambiguity() {
        let a = array(
            8,
            &[
                (1, ROOT, 2048, 2055),
                (2, ROOT, 2056, 2063),
                (3, OTHER, 2064, u64::MAX),
            ],
        );
        assert_eq!(find_type(&a, &parsed(&a), &ROOT), Err(GptError::EntryRange));
    }

    #[test]
    fn find_type_on_a_short_slice_is_entries_short() {
        let a = array(8, &[(1, ROOT, 71_680, 73_727)]);
        let h = parsed(&a);
        assert_eq!(
            find_type(&a[..1023], &h, &ROOT),
            Err(GptError::EntriesShort)
        );
    }

    // ---- a forged Header ---------------------------------------------------

    #[test]
    fn a_hand_made_header_cannot_cause_a_panic() {
        // `Header`'s fields are public; nothing here may trust them.
        let h = Header {
            entries: u32::MAX,
            entries_crc: 0,
            first_usable: 0,
            last_usable: u64::MAX,
            entries_bytes: usize::MAX,
        };
        let a = array(4, &[(1, ROOT, 2048, 4095)]);
        assert_eq!(check_entries(&a, &h), Err(GptError::EntriesShort));
        assert_eq!(partition(&a, &h, 1), Err(GptError::EntriesShort));
        assert_eq!(partition(&a, &h, u32::MAX), Err(GptError::EntriesShort));
        assert_eq!(find_type(&a, &h, &ROOT), Err(GptError::EntriesShort));
        // The count and the byte length disagreeing the other way.
        let h = Header {
            entries: 8,
            entries_bytes: 128,
            ..h
        };
        assert_eq!(partition(&a, &h, 8), Err(GptError::EntriesShort));
    }

    // ---- whole table, as the driver sees it --------------------------------

    #[test]
    fn a_disk_buffer_from_lba_0_parses_end_to_end() {
        let a = array(128, &[(1, ESP, 2048, 71_679), (2, ROOT, 71_680, 73_727)]);
        let mut disk = vec![0u8; TABLE_BYTES];
        disk[HEADER_OFFSET..HEADER_OFFSET + 512].copy_from_slice(&Hdr::over(&a).sector());
        disk[ENTRIES_OFFSET..].copy_from_slice(&a);

        let h = parse_header(&disk[HEADER_OFFSET..], DISK).unwrap();
        let entries = &disk[ENTRIES_OFFSET..];
        assert_eq!(check_entries(entries, &h), Ok(()));
        let used: Vec<u32> = (1..=h.entries)
            .filter(|&n| partition(entries, &h, n).unwrap().is_some())
            .collect();
        assert_eq!(used, [1, 2]);
        assert!(matches!(
            find_type(entries, &h, &ROOT),
            Ok(Lookup::Found { index: 2, .. })
        ));
    }

    // ---- error text --------------------------------------------------------

    #[test]
    fn why_strings_are_the_marker_text() {
        let all = [
            (GptError::HeaderShort, "header short"),
            (GptError::Signature, "signature"),
            (GptError::Revision, "revision"),
            (GptError::HeaderSize, "header size"),
            (GptError::Crc, "crc"),
            (GptError::MyLba, "my lba"),
            (GptError::EntrySize, "entry size"),
            (GptError::EntryCount, "entry count"),
            (GptError::EntryLba, "entry lba"),
            (GptError::UsableRange, "usable range"),
            (GptError::EntriesCrc, "entries crc"),
            (GptError::EntriesShort, "entries short"),
            (GptError::EntryRange, "entry range"),
            (GptError::EntryIndex, "entry index"),
        ];
        for (i, (e, text)) in all.iter().enumerate() {
            assert_eq!(e.why(), *text);
            // No two errors share a description, and none is "ambiguous",
            // which the driver prints for `Lookup::Ambiguous`.
            assert_ne!(*text, "ambiguous");
            for (_, other) in &all[i + 1..] {
                assert_ne!(text, other);
            }
        }
    }
}
