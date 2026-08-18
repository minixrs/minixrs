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

use crate::inode::NR_DIRECT_ZONES;
use crate::read::ptrs_per_block;
use crate::walk::Chunk;
use minixrs_kernel_shared::callnr::FS_MAX_IO;
use minixrs_kernel_shared::error::{EFBIG, EINVAL, EIO};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
