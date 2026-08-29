// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The write path's policy and allocator — everything about writing a MinixFS
//! file that can be decided without a device (slice 5.10a).
//!
//! `read.rs`'s twin, and split from it for the same reason: there is no I/O here
//! and no borrowed device state, so every rule carries a unit test that needs no
//! fake block driver. `main.rs` is behind `required-features = ["server"]` and
//! therefore invisible to every CI job, which makes "anything with a decision in
//! it lives in the lib" a hard rule rather than a preference.
//!
//! ## Where this differs from the reader, and why
//!
//! [`clamp_write`] consults **no file size**. A read clamps at EOF because there
//! is nothing past it; a write past EOF is how a file grows. The size is not an
//! input to the transfer at all — it is an *output*, computed by [`grow_size`]
//! after the bytes land.
//!
//! [`zone_slot_for_offset`] is [`crate::read::zone_for_offset`]'s allocating
//! twin. The reader asks *what zone is there*, and must distinguish a hole from
//! an unaddressable offset. The writer asks *where a zone would go*, which is a
//! different question with a different answer type — folding them together would
//! mean one of the two callers ignoring half of every result.
//!
//! ## Bit order is not a free choice
//!
//! `byte = bit / 8`, `mask = 1 << (bit % 8)` — identical to `tools/mkfs-mfs`'s
//! `Image::set_bit` and its `verify.rs` reader. The image is written by one and
//! read by the other two; a divergence here corrupts every image silently.

use crate::dirent::{DIRENT_SIZE, DirEntry};
use crate::inode::NR_DIRECT_ZONES;
use crate::read::ptrs_per_block;
use crate::walk::Chunk;
use minixrs_kernel_shared::callnr::FS_MAX_IO;
use minixrs_kernel_shared::error::{EFBIG, EINVAL, EIO, ENOSPC};

/// Where the zone backing a given file offset *would* live.
///
/// Contrast [`crate::read::ZoneLookup`], which reports what is actually there.
/// There is no `Hole` variant: a hole is not a property of the offset, it is a
/// property of the pointer the caller finds at the slot this names.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ZoneSlot {
    /// Slot `i` of the inode's direct zone array (`i < NR_DIRECT_ZONES`).
    Direct(usize),
    /// Slot `i` of the single-indirect block named by
    /// [`SINGLE_INDIRECT_SLOT`](crate::inode::SINGLE_INDIRECT_SLOT) of the
    /// inode's zone array.
    Indirect(usize),
    /// Past what the single-indirect span can address. Double-indirect is not
    /// implemented on either side of this crate.
    OutOfRange,
}

/// Which slot backs byte offset `off`.
pub fn zone_slot_for_offset(off: u64, bs: usize) -> ZoneSlot {
    if bs == 0 {
        return ZoneSlot::OutOfRange;
    }
    let index = off / bs as u64;
    if index < NR_DIRECT_ZONES as u64 {
        return ZoneSlot::Direct(index as usize);
    }
    let slot = index - NR_DIRECT_ZONES as u64;
    if slot >= ptrs_per_block(bs) as u64 {
        return ZoneSlot::OutOfRange;
    }
    ZoneSlot::Indirect(slot as usize)
}

/// What one `FS_WRITE` round may move.
///
/// Three rules, in the order they are applied:
///
/// 1. `len < 0` or `bs == 0` → `EINVAL`, before anything else. A negative length
///    left unchecked widens into a ~16 EiB `u64` byte count on the safecopy.
/// 2. An offset the single-indirect span cannot address → `EFBIG`. This is the
///    file-size limit, and it is reported before any device work happens.
/// 3. The transfer is clamped to [`FS_MAX_IO`] and to the end of the block
///    containing `pos`, so it never straddles two blocks — which is what lets the
///    server stage through one buffer.
///
/// **No size is consulted.** See the module docs.
pub fn clamp_write(pos: u64, len: i32, bs: usize) -> Result<Chunk, i32> {
    if len < 0 || bs == 0 {
        return Err(EINVAL);
    }
    if matches!(zone_slot_for_offset(pos, bs), ZoneSlot::OutOfRange) {
        return Err(EFBIG);
    }
    let off_in_block = (pos % bs as u64) as usize;
    let to_block_end = bs - off_in_block;
    let len = (len as u64).min(FS_MAX_IO as u64).min(to_block_end as u64) as usize;
    Ok(Chunk { len, off_in_block })
}

/// First clear bit at or after `from_bit`, below `limit_bits`.
///
/// `limit_bits` is not redundant with the block's length: the zone bitmap is
/// deliberately over-sized (see `layout.rs`'s module docs), so the tail of the
/// last bitmap block describes zones that do not exist and must never be handed
/// out.
pub fn bitmap_find_free(block: &[u8], from_bit: u32, limit_bits: u32) -> Option<u32> {
    let mut bit = from_bit;
    while bit < limit_bits {
        let byte = *block.get((bit / 8) as usize)?;
        if byte == 0xff {
            // Skip to the first bit of the next byte. Cheap, and a full bitmap
            // block is 32768 bits.
            bit = (bit | 7).checked_add(1)?;
            continue;
        }
        if byte & (1 << (bit % 8)) == 0 {
            return Some(bit);
        }
        bit = bit.checked_add(1)?;
    }
    None
}

/// May zone `zone` be **written**?
///
/// [`crate::walk::zone_ok`]'s write-side twin, and deliberately stricter: it adds
/// the lower bound the reader does without.
///
/// The reader can afford to be loose, because the worst a corrupt zone pointer
/// costs it is the wrong bytes. A *write* to the same pointer destroys the
/// filesystem it is pointed at: an inode whose `zone[i]` reads back as `3` would
/// have `do_write` store a data block straight over the zone bitmap, and a
/// `zone[SINGLE_INDIRECT_SLOT]` of `4` would have `place_zone` patch and store a
/// block of the inode table. Neither number is reachable from [`bitmap_find_free`],
/// which cannot return a zone below `first_data_zone` — but a zone number read off
/// the device is whatever the device says, and there is no `fsck` here to undo it.
///
/// Hence: at or above `first_data_zone`, below `blocks`. Zone 0 is excluded by the
/// lower bound rather than by a separate clause, because `first_data_zone` is
/// always at least [`START_BLOCK`](crate::layout::START_BLOCK).
pub fn write_zone_ok(zone: u32, first_data_zone: u32, blocks: u32) -> bool {
    zone >= first_data_zone && zone < blocks
}

/// Mark `bit` allocated. `None` if it lies past the block, which is how a caller
/// that mixed up its bitmap arithmetic finds out rather than by writing into the
/// wrong byte.
pub fn bitmap_set(block: &mut [u8], bit: u32) -> Option<()> {
    let byte = block.get_mut((bit / 8) as usize)?;
    *byte |= 1 << (bit % 8);
    Some(())
}

/// The file's size after `n` bytes land at `pos`.
///
/// `EFBIG` rather than a wrap: MinixFS stores size in a 32-bit field, and
/// wrapping it would report a huge file as a tiny one — a corruption that reads
/// back as truncation. `EIO` for a negative stored size, which is a corrupt
/// inode rather than anything the caller did.
pub fn grow_size(cur: i32, pos: u64, n: usize) -> Result<i32, i32> {
    if cur < 0 {
        return Err(EIO);
    }
    let end = pos.checked_add(n as u64).ok_or(EFBIG)?;
    if end > i32::MAX as u64 {
        return Err(EFBIG);
    }
    Ok((cur as u64).max(end) as i32)
}

/// Mark `bit` free. [`bitmap_set`]'s twin — **same byte, same mask**, because a
/// divergence between the two would free a different object than the caller
/// named.
///
/// `None` if the bit lies past the block, which is how a caller that mixed up its
/// bitmap arithmetic finds out rather than by writing into the wrong byte.
///
/// **Order matters at the call site, in both directions.** Allocation sets the
/// bit *before* anything references the object it names (see [`bitmap_set`]'s
/// callers), so a failure between the two leaks. Freeing runs the other way: the
/// reference is removed first, so this is called only once nothing points at the
/// object. Leak over corruption, stated once and applied both ways.
pub fn bitmap_clear(block: &mut [u8], bit: u32) -> Option<()> {
    let byte = block.get_mut((bit / 8) as usize)?;
    *byte &= !(1 << (bit % 8));
    Some(())
}

/// What one directory block has to say about a name.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DirentSlot {
    /// The name is already here, in slot `.0`.
    Occupied(usize),
    /// The name is not here, and slot `.0` is free.
    Free(usize),
    /// The name is not here and no slot is free.
    Full,
}

/// Scan one directory block for `want`, and for the first free slot.
///
/// **One pass**, because the create path needs both answers and this server has
/// exactly one block buffer — a second scan would be a second fetch.
///
/// **`Occupied` wins over `Free`, whatever the indices.** If a free slot
/// short-circuited the scan, a create could insert a duplicate entry *before* the
/// real one — and [`crate::walk::find_in_block`] stops at the first match, so the
/// original would be shadowed silently. That is why the free slot is remembered
/// and the whole block scanned anyway.
///
/// A trailing partial entry is ignored rather than half-decoded ([`crate::dirent`]'s
/// rule), so a short block cannot synthesize a free slot out of whatever followed
/// it.
pub fn dirent_slot(block: &[u8], want: &str) -> DirentSlot {
    let mut free: Option<usize> = None;
    for (i, chunk) in block.as_chunks::<DIRENT_SIZE>().0.iter().enumerate() {
        let Some(e) = DirEntry::from_le_bytes(chunk) else {
            continue;
        };
        if e.ino == 0 {
            if free.is_none() {
                free = Some(i);
            }
            continue;
        }
        // A name that is not valid UTF-8 decodes to "", which cannot equal any
        // component `parse_path` accepted -- so it cannot be matched by accident.
        if e.name_str() == want {
            return DirentSlot::Occupied(i);
        }
    }
    match free {
        Some(i) => DirentSlot::Free(i),
        None => DirentSlot::Full,
    }
}

/// Byte offset at which an appended directory entry goes, given the directory's
/// current size.
///
/// Used when no slot in any existing block is free: the entry lands at the end
/// and the directory grows by one entry — through exactly the allocator a file
/// grows through, which is why growth needs no second code path.
///
/// `EIO` for a size that is negative, past [`crate::walk::MAX_DIR_BYTES`], or not
/// a whole number of entries — all three are a corrupt directory inode, and
/// appending at a misaligned offset would splice an entry across two others.
/// `ENOSPC` when the appended entry would not fit under the cap, which is a
/// *full* directory rather than a corrupt one and is a different thing to tell a
/// caller.
pub fn dir_append_offset(size: i32) -> Result<u64, i32> {
    let size = crate::walk::dir_size(size)?;
    if !size.is_multiple_of(DIRENT_SIZE) {
        return Err(EIO);
    }
    let end = size.checked_add(DIRENT_SIZE).ok_or(EIO)?;
    if end > crate::walk::MAX_DIR_BYTES {
        return Err(ENOSPC);
    }
    Ok(size as u64)
}

/// How many single-indirect slots a file of `size` bytes reaches.
///
/// `0` for a file inside the direct zones. **This is what bounds truncate's slot
/// scan** (C8): a 32 KiB file examines two slots rather than the block's 1024.
/// Capped at [`ptrs_per_block`] anyway, because every device-derived loop in this
/// crate carries a cap and a corrupt size must not walk past the block.
///
/// `EIO` for a negative size — a corrupt inode rather than a caller error,
/// [`grow_size`]'s split.
pub fn indirect_slots_used(size: i32, bs: usize) -> Result<usize, i32> {
    if size < 0 || bs == 0 {
        return Err(EIO);
    }
    let blocks = (size as usize).div_ceil(bs);
    Ok(blocks
        .saturating_sub(NR_DIRECT_ZONES)
        .min(ptrs_per_block(bs)))
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::format;
    use std::string::String;
    use std::vec::Vec;

    const BS: usize = crate::MFS_BLOCK_SIZE;
    /// First byte the single-indirect region covers: seven direct zones in.
    const SEAM: u64 = (NR_DIRECT_ZONES * crate::MFS_BLOCK_SIZE) as u64;
    /// One past the last byte the single-indirect span can address.
    const SPAN_END: u64 = SEAM + (BS / 4 * BS) as u64;

    // ----- zone_slot_for_offset ---------------------------------------------

    #[test]
    fn offset_zero_is_direct_slot_zero() {
        assert_eq!(zone_slot_for_offset(0, BS), ZoneSlot::Direct(0));
    }

    #[test]
    fn the_last_direct_byte_and_the_first_indirect_byte_are_adjacent() {
        assert_eq!(zone_slot_for_offset(SEAM - 1, BS), ZoneSlot::Direct(6));
        assert_eq!(zone_slot_for_offset(SEAM, BS), ZoneSlot::Indirect(0));
    }

    #[test]
    fn the_last_addressable_byte_is_the_last_indirect_slot() {
        assert_eq!(
            zone_slot_for_offset(SPAN_END - 1, BS),
            ZoneSlot::Indirect(BS / 4 - 1)
        );
        assert_eq!(zone_slot_for_offset(SPAN_END, BS), ZoneSlot::OutOfRange);
    }

    #[test]
    fn a_zero_block_size_is_out_of_range_not_a_division_by_zero() {
        assert_eq!(zone_slot_for_offset(0, 0), ZoneSlot::OutOfRange);
    }

    // ----- clamp_write ------------------------------------------------------

    #[test]
    fn a_write_stops_at_the_end_of_its_block() {
        // W2: one call moves at most one block, so the server stages through its
        // single buffer. The caller re-sends for the rest.
        let c = clamp_write(4000, 4096, BS).unwrap();
        assert_eq!(c.off_in_block, 4000);
        assert_eq!(c.len, 96);
    }

    #[test]
    fn a_write_starting_on_a_block_boundary_may_fill_it() {
        let c = clamp_write(BS as u64, 4096, BS).unwrap();
        assert_eq!(c.off_in_block, 0);
        assert_eq!(c.len, BS);
    }

    #[test]
    fn a_write_is_capped_at_one_transfer() {
        // Pins the shipped configuration's transfer size, not the cap itself:
        // `MFS_BLOCK_SIZE == BDEV_BLOCK_SIZE == FS_MAX_IO` (lib.rs's `const _`), so
        // at `pos = 0` this block size makes `to_block_end` and `FS_MAX_IO` the
        // same number and this test cannot tell which clamp produced `c.len`. See
        // `the_transfer_cap_binds_independently_of_the_block_end` for the cap's
        // own test.
        let c = clamp_write(0, i32::MAX, BS).unwrap();
        assert_eq!(c.len, FS_MAX_IO);
    }

    #[test]
    fn the_transfer_cap_binds_independently_of_the_block_end() {
        // `MFS_BLOCK_SIZE == BDEV_BLOCK_SIZE == FS_MAX_IO` (lib.rs's `const _`), so at
        // `pos = 0` a 4 KiB block makes `to_block_end` and `FS_MAX_IO` the same number
        // and neither clamp can be observed alone. Passing a larger block separates
        // them: the cap is then strictly the smaller, so this fails if
        // `.min(FS_MAX_IO)` is ever dropped from `clamp_write`.
        let c = clamp_write(0, i32::MAX, 2 * FS_MAX_IO).unwrap();
        assert_eq!(c.len, FS_MAX_IO);
        assert_eq!(c.off_in_block, 0);
    }

    #[test]
    fn a_write_past_end_of_file_is_allowed_because_that_is_how_a_file_grows() {
        // The one way clamp_write differs from clamp_read: a read clamps at EOF,
        // a write does not. No size is consulted here at all.
        let c = clamp_write(SEAM, 100, BS).unwrap();
        assert_eq!(c.len, 100);
        assert_eq!(c.off_in_block, 0);
    }

    #[test]
    fn a_write_past_the_single_indirect_span_is_efbig() {
        assert_eq!(clamp_write(SPAN_END, 1, BS), Err(EFBIG));
    }

    #[test]
    fn a_negative_length_is_einval() {
        // Left unchecked it would widen into a ~16 EiB u64 on the safecopy.
        assert_eq!(clamp_write(0, -1, BS), Err(EINVAL));
    }

    #[test]
    fn a_zero_block_size_is_einval() {
        assert_eq!(clamp_write(0, 1, 0), Err(EINVAL));
    }

    #[test]
    fn a_zero_length_write_is_ok_not_an_error() {
        assert_eq!(
            clamp_write(0, 0, BS),
            Ok(Chunk {
                len: 0,
                off_in_block: 0
            })
        );
    }

    // ----- bitmap -----------------------------------------------------------

    #[test]
    fn a_free_bit_is_found_at_the_first_zero() {
        let mut b = [0u8; 8];
        b[0] = 0b0000_0111;
        assert_eq!(bitmap_find_free(&b, 0, 64), Some(3));
    }

    #[test]
    fn the_search_starts_where_it_is_told() {
        let mut b = [0u8; 8];
        b[0] = 0b0000_0111;
        assert_eq!(bitmap_find_free(&b, 5, 64), Some(5));
    }

    #[test]
    fn a_full_byte_is_skipped_and_the_next_free_bit_found() {
        let mut b = [0u8; 8];
        b[0] = 0xff;
        b[1] = 0b0000_0001;
        assert_eq!(bitmap_find_free(&b, 0, 64), Some(9));
    }

    #[test]
    fn a_full_bitmap_has_no_free_bit() {
        let b = [0xffu8; 8];
        assert_eq!(bitmap_find_free(&b, 0, 64), None);
    }

    #[test]
    fn the_limit_is_respected_even_when_the_block_is_longer() {
        // The zone bitmap is deliberately over-sized (layout.rs's module docs),
        // so bits past the real zone count must never be handed out.
        let b = [0u8; 8];
        assert_eq!(bitmap_find_free(&b, 0, 5), Some(0));
        assert_eq!(bitmap_find_free(&b, 5, 5), None);
    }

    #[test]
    fn setting_a_bit_uses_minix_ordering() {
        // byte = bit/8, mask = 1 << (bit%8) -- matches mkfs's Image::set_bit and
        // verify.rs's bit_set. Diverging silently corrupts every image.
        let mut b = [0u8; 8];
        assert_eq!(bitmap_set(&mut b, 9), Some(()));
        assert_eq!(b[1], 0b0000_0010);
    }

    #[test]
    fn setting_a_bit_past_the_block_is_none_not_a_panic() {
        let mut b = [0u8; 8];
        assert_eq!(bitmap_set(&mut b, 64), None);
    }

    // ----- write_zone_ok ----------------------------------------------------

    /// A plausible small image: metadata below zone 12, 256 blocks in total.
    const FDZ: u32 = 12;
    const BLOCKS: u32 = 256;

    #[test]
    fn a_metadata_zone_may_not_be_written() {
        // The whole point of the lower bound. Zone 3 is inside the bitmaps and
        // zone 4 inside the inode table on an image shaped like this one, and
        // `walk::zone_ok` accepts both — a write there destroys the filesystem.
        assert!(!write_zone_ok(3, FDZ, BLOCKS));
        assert!(!write_zone_ok(4, FDZ, BLOCKS));
        assert!(!write_zone_ok(FDZ - 1, FDZ, BLOCKS), "the boundary itself");
        // ... and the reader really is looser, which is the asymmetry this
        // function exists to state.
        assert!(crate::walk::zone_ok(3, BLOCKS));
    }

    #[test]
    fn the_first_data_zone_may_be_written() {
        assert!(write_zone_ok(FDZ, FDZ, BLOCKS));
    }

    #[test]
    fn the_last_zone_may_be_written_and_the_one_past_it_may_not() {
        assert!(write_zone_ok(BLOCKS - 1, FDZ, BLOCKS));
        assert!(!write_zone_ok(BLOCKS, FDZ, BLOCKS));
        assert!(!write_zone_ok(u32::MAX, FDZ, BLOCKS));
    }

    #[test]
    fn zone_zero_may_never_be_written() {
        // Excluded by the lower bound rather than a clause of its own, because
        // `first_data_zone >= START_BLOCK`. Checked with a degenerate
        // `first_data_zone` too, so the property does not rest on that alone.
        assert!(!write_zone_ok(0, FDZ, BLOCKS));
        assert!(!write_zone_ok(0, 1, BLOCKS));
    }

    // ----- grow_size --------------------------------------------------------

    #[test]
    fn a_write_inside_the_file_does_not_shrink_it() {
        assert_eq!(grow_size(1000, 0, 10), Ok(1000));
    }

    #[test]
    fn a_write_past_the_end_extends_the_file() {
        assert_eq!(grow_size(1000, 990, 100), Ok(1090));
    }

    #[test]
    fn a_size_that_would_not_fit_the_on_disk_field_is_efbig() {
        // MinixFS stores size as a 32-bit field; wrapping it would report a huge
        // file as a tiny one.
        assert_eq!(grow_size(0, i32::MAX as u64, 1), Err(EFBIG));
        assert_eq!(grow_size(0, u64::MAX, 1), Err(EFBIG));
    }

    #[test]
    fn a_negative_stored_size_is_eio() {
        // A corrupt inode, not a caller error.
        assert_eq!(grow_size(-1, 0, 1), Err(EIO));
    }

    // ----- bitmap_clear -----------------------------------------------------

    #[test]
    fn clearing_a_bit_uses_the_same_ordering_as_setting_one() {
        // Same byte, same mask. A divergence between the two would free a
        // different zone than the one the caller named -- silent corruption.
        let mut b = [0u8; 8];
        assert_eq!(bitmap_set(&mut b, 9), Some(()));
        assert_eq!(b[1], 0b0000_0010);
        assert_eq!(bitmap_clear(&mut b, 9), Some(()));
        assert_eq!(b[1], 0);
    }

    #[test]
    fn clearing_a_bit_leaves_its_neighbours_alone() {
        let mut b = [0xffu8; 8];
        assert_eq!(bitmap_clear(&mut b, 9), Some(()));
        assert_eq!(b[1], 0b1111_1101);
        assert_eq!(b[0], 0xff);
        assert_eq!(b[2], 0xff);
    }

    #[test]
    fn clearing_an_already_free_bit_is_a_no_op_not_an_error() {
        // Truncate walks a file's zone array, which may hold holes.
        let mut b = [0u8; 8];
        assert_eq!(bitmap_clear(&mut b, 3), Some(()));
        assert_eq!(b, [0u8; 8]);
    }

    #[test]
    fn clearing_a_bit_past_the_block_is_none_not_a_panic() {
        // `bitmap_set`'s rule: a caller that mixed up its bitmap arithmetic finds
        // out, rather than writing into the wrong byte.
        let mut b = [0u8; 8];
        assert_eq!(bitmap_clear(&mut b, 64), None);
        assert_eq!(bitmap_clear(&mut b, u32::MAX), None);
    }

    // ----- dirent_slot ------------------------------------------------------

    /// One directory block: `.`, `..`, then whatever `names` says, and free slots
    /// for the rest.
    fn dir_block(names: &[(u32, &str)]) -> [u8; BS] {
        let mut b = [0u8; BS];
        let mut at = 0usize;
        for (ino, name) in names {
            let e = crate::dirent::DirEntry::new(*ino, name.as_bytes()).unwrap();
            b[at..at + crate::dirent::DIRENT_SIZE].copy_from_slice(&e.to_le_bytes());
            at += crate::dirent::DIRENT_SIZE;
        }
        b
    }

    #[test]
    fn an_existing_name_is_occupied_at_its_own_slot() {
        let b = dir_block(&[(1, "."), (1, ".."), (7, "motd")]);
        assert_eq!(dirent_slot(&b, "motd"), DirentSlot::Occupied(2));
    }

    #[test]
    fn a_missing_name_reports_the_first_free_slot() {
        let b = dir_block(&[(1, "."), (1, ".."), (7, "motd")]);
        assert_eq!(dirent_slot(&b, "new"), DirentSlot::Free(3));
    }

    #[test]
    fn a_freed_slot_in_the_middle_is_the_one_reported() {
        // Directories are not compacted, so a removed entry leaves a zeroed slot
        // behind and a create should reuse it rather than growing the directory.
        let mut b = dir_block(&[(1, "."), (1, ".."), (7, "motd"), (8, "pattern")]);
        b[2 * crate::dirent::DIRENT_SIZE..3 * crate::dirent::DIRENT_SIZE].fill(0);
        assert_eq!(dirent_slot(&b, "new"), DirentSlot::Free(2));
    }

    #[test]
    fn an_existing_name_wins_over_an_earlier_free_slot() {
        // The one ordering that matters. If `Free` short-circuited, a create
        // would insert a duplicate entry *before* the real one -- and the reader
        // stops at the first match, so the original would be shadowed silently.
        let mut b = dir_block(&[(1, "."), (1, ".."), (7, "motd"), (8, "keep")]);
        b[2 * crate::dirent::DIRENT_SIZE..3 * crate::dirent::DIRENT_SIZE].fill(0);
        assert_eq!(dirent_slot(&b, "keep"), DirentSlot::Occupied(3));
    }

    #[test]
    fn a_block_with_every_slot_used_is_full() {
        let names: Vec<(u32, String)> = (0..BS / crate::dirent::DIRENT_SIZE)
            .map(|i| (i as u32 + 1, format!("f{i:02}")))
            .collect();
        let refs: Vec<(u32, &str)> = names.iter().map(|(i, n)| (*i, n.as_str())).collect();
        let b = dir_block(&refs);
        assert_eq!(dirent_slot(&b, "new"), DirentSlot::Full);
        // ...and a name that *is* there is still found in a full block.
        assert_eq!(dirent_slot(&b, "f00"), DirentSlot::Occupied(0));
    }

    #[test]
    fn a_short_block_decodes_only_whole_entries() {
        // A trailing partial entry is ignored rather than half-decoded, so a
        // short read cannot synthesize a free slot out of whatever followed it.
        let b = dir_block(&[(1, "."), (1, "..")]);
        assert_eq!(
            dirent_slot(&b[..2 * crate::dirent::DIRENT_SIZE], "new"),
            DirentSlot::Full
        );
        assert_eq!(
            dirent_slot(&b[..2 * crate::dirent::DIRENT_SIZE + 8], "new"),
            DirentSlot::Full
        );
        assert_eq!(dirent_slot(&[], "new"), DirentSlot::Full);
    }

    // ----- dir_append_offset ------------------------------------------------

    #[test]
    fn an_appended_entry_lands_at_the_directorys_current_end() {
        assert_eq!(dir_append_offset(0), Ok(0));
        assert_eq!(dir_append_offset(BS as i32), Ok(BS as u64));
    }

    #[test]
    fn a_size_that_is_not_a_whole_number_of_entries_is_eio() {
        // A corrupt directory inode. Appending at a misaligned offset would
        // splice an entry across two others.
        assert_eq!(dir_append_offset(1), Err(EIO));
        assert_eq!(
            dir_append_offset(crate::dirent::DIRENT_SIZE as i32 - 1),
            Err(EIO)
        );
    }

    #[test]
    fn a_negative_or_oversized_directory_is_eio() {
        // `dir_size`'s rules, inherited: a corrupt inode, not a caller error.
        assert_eq!(dir_append_offset(-1), Err(EIO));
        assert_eq!(
            dir_append_offset(crate::walk::MAX_DIR_BYTES as i32 + 1),
            Err(EIO)
        );
    }

    #[test]
    fn a_directory_at_the_cap_cannot_grow_and_is_enospc() {
        // Distinct from EIO: the directory is well-formed, it is simply full.
        // `MAX_DIR_BYTES` is a whole number of blocks and therefore of entries,
        // so this is exactly the boundary.
        let cap = crate::walk::MAX_DIR_BYTES as i32;
        assert_eq!(dir_append_offset(cap), Err(ENOSPC));
        assert_eq!(
            dir_append_offset(cap - crate::dirent::DIRENT_SIZE as i32),
            Ok((cap - crate::dirent::DIRENT_SIZE as i32) as u64),
            "one entry short of the cap still fits"
        );
    }

    // ----- indirect_slots_used ----------------------------------------------

    #[test]
    fn a_file_inside_the_direct_zones_reaches_no_indirect_slot() {
        assert_eq!(indirect_slots_used(0, BS), Ok(0));
        assert_eq!(indirect_slots_used(SEAM as i32, BS), Ok(0));
    }

    #[test]
    fn a_file_past_the_seam_reaches_one_slot_per_block_past_it() {
        assert_eq!(indirect_slots_used(SEAM as i32 + 1, BS), Ok(1));
        assert_eq!(indirect_slots_used(SEAM as i32 + BS as i32, BS), Ok(1));
        assert_eq!(indirect_slots_used(SEAM as i32 + BS as i32 + 1, BS), Ok(2));
        // 32 KiB -- what init's write proof produces -- is exactly SEAM + BS
        // (seven direct zones plus one whole indirect-addressed block), so it
        // examines one slot, not the block's 1024. That bound is C8, and it is
        // what lets truncate work with a single block buffer.
        assert_eq!(indirect_slots_used(32 * 1024, BS), Ok(1));
    }

    #[test]
    fn the_slot_count_is_capped_at_the_blocks_own_pointers() {
        // A corrupt size must not walk past the indirect block.
        assert_eq!(indirect_slots_used(i32::MAX, BS), Ok(BS / 4));
    }

    #[test]
    fn a_negative_size_or_zero_block_is_eio() {
        assert_eq!(indirect_slots_used(-1, BS), Err(EIO));
        assert_eq!(indirect_slots_used(0, 0), Err(EIO));
    }
}
