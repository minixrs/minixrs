// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Fixed-size IPC message.
//!
//! The 104-byte layout matches MINIX 3's `sizeof(message) == 104` assertion
//! for `__x86_64__` (see `include/minix/ipc.h`). Both x86_64 and aarch64 ports
//! of minix.rs adopt the same size so that the message is ABI-portable.
//!
//! Note: MINIX 3 only ever shipped as a 32-bit OS (i386, and 32-bit ARM). The
//! `__x86_64__` layout we anchor on exists in the MINIX 3 source tree, but a
//! working 64-bit MINIX 3 was never an upstream release — it was a personal
//! prototype by this project's author. We treat the 104-byte layout as an ABI
//! *reference*, not a shipped 64-bit MINIX 3.
//!
//! The struct is explicitly 8-aligned. MINIX 3's `message` is a union over
//! sub-structs containing `uint64_t` fields, so its native alignment is 8.
//! minix.rs expresses `payload` as `[u8; 96]` for now, but future typed
//! accessor structs (slice 2.5) will overlay payload regions that contain
//! `u64` fields. Forcing the message itself to 8-align guarantees those
//! overlays are 8-aligned relative to the message base — required for
//! strict-alignment loads on aarch64.
//!
//! Typed access — MINIX 3 had a union over many `mess_*` payload variants
//! (`m1i1`, `m2l1`, etc.). minix.rs will eventually expose strongly-typed
//! accessor structs per call (e.g., `as_vfs_read()`), but those live in the
//! servers/userland crates so the kernel doesn't pull in every protocol
//! definition. For now, `payload` is a raw byte array that callers
//! interpret per-call.

use crate::endpoint::Endpoint;

/// 104-byte fixed-size IPC message. Layout matches MINIX 3 x86_64.
#[repr(C, align(8))]
#[derive(Copy, Clone, Debug)]
pub struct Message {
    /// Endpoint of the sender (set by the kernel on delivery).
    pub m_source: Endpoint,
    /// Call number on send; result code on reply.
    pub m_type: i32,
    /// Raw payload — interpreted per-call by typed accessors.
    pub payload: [u8; 96],
}

// Compile-time guarantee that the layout matches MINIX 3's x86_64 message.
const _: () = assert!(core::mem::size_of::<Message>() == 104);
const _: () = assert!(core::mem::align_of::<Message>() == 8);

/// One past the highest user VA reachable through TTBR0. Limine programs
/// `TCR_EL1.T0SZ = 16`, giving a 48-bit user VA space (`[0, 2^48)`); the
/// kernel's HHDM lives in the very high half via TTBR1 and is unreachable
/// from this range.
pub const USER_VA_TOP: u64 = 1 << 48;

/// Bounds check for a user pointer of length `len` starting at `va`.
///
/// Rejects:
///   - NULL or sub-alignment-of-[`Message`] addresses (a NULL pointer or an
///     unaligned `read_volatile` is UB even before the access faults).
///   - Pointers whose range extends past [`USER_VA_TOP`].
///
/// A pure predicate over the message ABI shape, so it lives here (and is
/// host-testable) rather than in the kernel's copy helpers.
#[inline]
pub fn user_va_ok(va: u64, len: usize) -> bool {
    if va < core::mem::align_of::<Message>() as u64 {
        return false;
    }
    if !va.is_multiple_of(core::mem::align_of::<Message>() as u64) {
        return false;
    }
    match va.checked_add(len as u64) {
        Some(end) => end <= USER_VA_TOP,
        None => false,
    }
}

/// Bounds check for a user *byte buffer* of length `len` starting at `va`.
///
/// The counterpart of [`user_va_ok`] for ranges that carry no alignment
/// requirement — a grant's granted range, or a `SYS_COPY` / `SYS_SAFECOPY`
/// buffer, is a plain byte array and may start anywhere. Only the range itself
/// is checked: non-NULL, no overflow, and entirely below [`USER_VA_TOP`].
///
/// Bounding the range is what keeps [`page_chunks`] from being handed an
/// attacker-chosen 2^64 length and iterating essentially forever; the mapping
/// check is the kernel's page-table walk, not this predicate. A zero length is
/// accepted at any non-NULL address (the copy is a no-op).
#[inline]
pub fn user_range_ok(va: u64, len: u64) -> bool {
    if va == 0 {
        return false;
    }
    match va.checked_add(len) {
        Some(end) => end <= USER_VA_TOP,
        None => false,
    }
}

/// User page granule — the 4 KiB stage-1 granule the kernel's page-table
/// walker uses. Declared here (rather than pulled from the kernel's `mmu`
/// module) so [`page_chunks`] stays a pure, host-testable helper.
pub const USER_PAGE_SIZE: u64 = 4096;

/// One page-bounded slice of a user buffer, produced by [`page_chunks`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PageChunk {
    /// Page-aligned base VA of the page holding this chunk.
    pub page_va: u64,
    /// Byte offset of the chunk within that page.
    pub offset: usize,
    /// Chunk length. Never crosses the page end, never zero.
    pub len: usize,
}

/// Split the user range `[va, va + len)` into ascending per-page chunks.
///
/// The kernel's cross-address-space copy engine walks one page-table leaf per
/// chunk, so it needs the range decomposed at page boundaries. A 104-byte
/// [`Message`] is only 8-aligned, so it genuinely can straddle two pages —
/// this is the arithmetic that gets that right.
///
/// A pure geometry helper over the message ABI shape, so it lives here (and is
/// host-testable) rather than in the kernel's copy helpers, alongside
/// [`user_va_ok`]. Yields nothing for `len == 0`.
#[inline]
pub fn page_chunks(va: u64, len: usize) -> PageChunks {
    PageChunks { va, remaining: len }
}

/// Iterator returned by [`page_chunks`].
#[derive(Copy, Clone, Debug)]
pub struct PageChunks {
    va: u64,
    remaining: usize,
}

impl Iterator for PageChunks {
    type Item = PageChunk;

    fn next(&mut self) -> Option<PageChunk> {
        if self.remaining == 0 {
            return None;
        }
        let page_va = self.va & !(USER_PAGE_SIZE - 1);
        let offset = (self.va - page_va) as usize;
        let to_page_end = USER_PAGE_SIZE as usize - offset;
        let len = if to_page_end < self.remaining {
            to_page_end
        } else {
            self.remaining
        };
        self.va += len as u64;
        self.remaining -= len;
        Some(PageChunk {
            page_va,
            offset,
            len,
        })
    }
}

/// One page-bounded slice of a *cross-address-space* copy, produced by
/// [`dual_page_chunks`]. Bounded by whichever side's page ends first, so a
/// single chunk is resolvable by exactly one page-table walk per side.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DualPageChunk {
    /// Page-aligned base VA of the source page.
    pub src_page_va: u64,
    /// Byte offset of the chunk within the source page.
    pub src_offset: usize,
    /// Page-aligned base VA of the destination page.
    pub dst_page_va: u64,
    /// Byte offset of the chunk within the destination page.
    pub dst_offset: usize,
    /// Chunk length. Crosses neither page end, never zero.
    pub len: usize,
}

/// Split a copy of `len` bytes from `src_va` to `dst_va` into chunks that stay
/// within one page **on both sides**.
///
/// The cross-address-space copy engine walks one leaf per side per chunk, so a
/// chunk must not straddle a page boundary in either address space. The step is
/// `min(src_to_page_end, dst_to_page_end, remaining)` — with unequal page
/// offsets on the two sides, each boundary splits the copy independently, so a
/// range spanning one page can still take three chunks.
///
/// A pure geometry helper like [`page_chunks`], and here for the same reason:
/// the kernel crate has no `#[cfg(test)]`, and this is where the off-by-one
/// risk lives. Yields nothing for `len == 0`.
#[inline]
pub fn dual_page_chunks(src_va: u64, dst_va: u64, len: usize) -> DualPageChunks {
    DualPageChunks {
        src_va,
        dst_va,
        remaining: len,
    }
}

/// Iterator returned by [`dual_page_chunks`].
#[derive(Copy, Clone, Debug)]
pub struct DualPageChunks {
    src_va: u64,
    dst_va: u64,
    remaining: usize,
}

impl Iterator for DualPageChunks {
    type Item = DualPageChunk;

    fn next(&mut self) -> Option<DualPageChunk> {
        if self.remaining == 0 {
            return None;
        }
        let src_page_va = self.src_va & !(USER_PAGE_SIZE - 1);
        let dst_page_va = self.dst_va & !(USER_PAGE_SIZE - 1);
        let src_offset = (self.src_va - src_page_va) as usize;
        let dst_offset = (self.dst_va - dst_page_va) as usize;
        let page = USER_PAGE_SIZE as usize;
        let mut len = self.remaining;
        if page - src_offset < len {
            len = page - src_offset;
        }
        if page - dst_offset < len {
            len = page - dst_offset;
        }
        self.src_va += len as u64;
        self.dst_va += len as u64;
        self.remaining -= len;
        Some(DualPageChunk {
            src_page_va,
            src_offset,
            dst_page_va,
            dst_offset,
            len,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MSG: usize = core::mem::size_of::<Message>();
    const PAGE: u64 = USER_PAGE_SIZE;

    #[test]
    fn message_is_104_bytes() {
        assert_eq!(core::mem::size_of::<Message>(), 104);
    }

    #[test]
    fn message_align_is_8() {
        assert_eq!(core::mem::align_of::<Message>(), 8);
    }

    #[test]
    fn message_payload_offset_is_8() {
        let m = Message {
            m_source: 0,
            m_type: 0,
            payload: [0; 96],
        };
        let base = &m as *const _ as usize;
        let payload = m.payload.as_ptr() as usize;
        assert_eq!(payload - base, 8);
    }

    #[test]
    fn user_va_ok_rejects_null() {
        assert!(!user_va_ok(0, core::mem::size_of::<Message>()));
    }

    #[test]
    fn user_va_ok_rejects_unaligned() {
        assert!(!user_va_ok(0x10001, core::mem::size_of::<Message>()));
    }

    #[test]
    fn user_va_ok_rejects_at_top() {
        assert!(!user_va_ok(USER_VA_TOP, core::mem::size_of::<Message>()));
    }

    #[test]
    fn user_va_ok_rejects_overflow() {
        assert!(!user_va_ok(u64::MAX - 7, core::mem::size_of::<Message>()));
    }

    #[test]
    fn user_va_ok_accepts_aligned_in_range() {
        // 8 MiB — where the boot stubs' stacks live.
        assert!(user_va_ok(0x80_0000, core::mem::size_of::<Message>()));
    }

    #[test]
    fn page_chunks_empty_for_zero_len() {
        assert_eq!(page_chunks(0x80_0000, 0).count(), 0);
    }

    /// Collect at most `N` chunks into a fixed array — `kernel-shared` is
    /// unconditionally `no_std`, so tests cannot use `Vec`.
    fn collect_chunks<const N: usize>(va: u64, len: usize) -> ([PageChunk; N], usize) {
        let empty = PageChunk {
            page_va: 0,
            offset: 0,
            len: 0,
        };
        let mut out = [empty; N];
        let mut n = 0;
        for c in page_chunks(va, len) {
            assert!(n < N, "more chunks than the test expected");
            out[n] = c;
            n += 1;
        }
        (out, n)
    }

    #[test]
    fn page_chunks_single_chunk_when_page_aligned() {
        let (c, n) = collect_chunks::<2>(0x80_0000, MSG);
        assert_eq!(n, 1);
        assert_eq!(
            c[0],
            PageChunk {
                page_va: 0x80_0000,
                offset: 0,
                len: MSG,
            }
        );
    }

    #[test]
    fn page_chunks_straddles_a_page_boundary() {
        // 8 bytes in the first page, the remaining 96 in the next — the
        // layout stub A's deliberate straddle probe produces.
        let va = 0x80_0000 + PAGE - 8;
        let (c, n) = collect_chunks::<3>(va, MSG);
        assert_eq!(n, 2);
        assert_eq!(
            c[0],
            PageChunk {
                page_va: 0x80_0000,
                offset: (PAGE - 8) as usize,
                len: 8,
            }
        );
        assert_eq!(
            c[1],
            PageChunk {
                page_va: 0x80_0000 + PAGE,
                offset: 0,
                len: MSG - 8,
            }
        );
    }

    #[test]
    fn page_chunks_exactly_fills_the_page_tail() {
        let va = 0x80_0000 + PAGE - MSG as u64;
        let (c, n) = collect_chunks::<2>(va, MSG);
        assert_eq!(n, 1);
        assert_eq!(c[0].offset + c[0].len, PAGE as usize);
    }

    #[test]
    fn page_chunks_spans_three_pages() {
        // Start mid-page, run past two full pages: partial, full, partial.
        let va = 0x80_0000 + 0x800;
        let len = (2 * PAGE) as usize;
        let (c, n) = collect_chunks::<4>(va, len);
        assert_eq!(n, 3);
        assert_eq!(c[0].len, 0x800);
        assert_eq!(c[1].len, PAGE as usize);
        assert_eq!(c[1].offset, 0);
        assert_eq!(c[2].len, 0x800);
        assert_eq!(c[2].offset, 0);
    }

    #[test]
    fn user_range_ok_rejects_null_and_overflow() {
        assert!(!user_range_ok(0, 64));
        assert!(!user_range_ok(0, 0));
        assert!(!user_range_ok(u64::MAX, 2));
    }

    #[test]
    fn user_range_ok_rejects_past_the_user_top() {
        assert!(!user_range_ok(USER_VA_TOP, 1));
        assert!(!user_range_ok(USER_VA_TOP - 4, 8));
        // Ending exactly at the top is in range.
        assert!(user_range_ok(USER_VA_TOP - 8, 8));
    }

    #[test]
    fn user_range_ok_accepts_unaligned_byte_buffers() {
        // The whole point of this predicate versus `user_va_ok`: a granted
        // range is a byte array and need not be 8-aligned.
        assert!(user_range_ok(0x80_0001, 63));
        assert!(!user_va_ok(0x80_0001, 63));
    }

    /// Collect at most `N` dual chunks — no `Vec` in this `no_std` crate.
    fn collect_dual<const N: usize>(src: u64, dst: u64, len: usize) -> ([DualPageChunk; N], usize) {
        let empty = DualPageChunk {
            src_page_va: 0,
            src_offset: 0,
            dst_page_va: 0,
            dst_offset: 0,
            len: 0,
        };
        let mut out = [empty; N];
        let mut n = 0;
        for c in dual_page_chunks(src, dst, len) {
            assert!(n < N, "more chunks than the test expected");
            out[n] = c;
            n += 1;
        }
        (out, n)
    }

    #[test]
    fn dual_page_chunks_empty_for_zero_len() {
        assert_eq!(dual_page_chunks(0x80_0000, 0x90_0000, 0).count(), 0);
    }

    #[test]
    fn dual_page_chunks_single_chunk_when_both_sides_fit() {
        let (c, n) = collect_dual::<2>(0x80_0000, 0x90_0100, 64);
        assert_eq!(n, 1);
        assert_eq!(
            c[0],
            DualPageChunk {
                src_page_va: 0x80_0000,
                src_offset: 0,
                dst_page_va: 0x90_0000,
                dst_offset: 0x100,
                len: 64,
            }
        );
    }

    #[test]
    fn dual_page_chunks_splits_when_only_the_source_straddles() {
        // Source starts 8 bytes before a page end; destination is page-aligned.
        let src = 0x80_0000 + PAGE - 8;
        let (c, n) = collect_dual::<3>(src, 0x90_0000, MSG);
        assert_eq!(n, 2);
        assert_eq!(c[0].len, 8);
        assert_eq!(c[0].src_offset, (PAGE - 8) as usize);
        assert_eq!(c[0].dst_offset, 0);
        assert_eq!(c[1].src_page_va, 0x80_0000 + PAGE);
        assert_eq!(c[1].src_offset, 0);
        assert_eq!(c[1].dst_offset, 8);
        assert_eq!(c[1].len, MSG - 8);
    }

    #[test]
    fn dual_page_chunks_splits_when_only_the_destination_straddles() {
        let dst = 0x90_0000 + PAGE - 8;
        let (c, n) = collect_dual::<3>(0x80_0000, dst, MSG);
        assert_eq!(n, 2);
        assert_eq!(c[0].len, 8);
        assert_eq!(c[0].dst_offset, (PAGE - 8) as usize);
        assert_eq!(c[1].dst_page_va, 0x90_0000 + PAGE);
        assert_eq!(c[1].dst_offset, 0);
        assert_eq!(c[1].src_offset, 8);
    }

    #[test]
    fn dual_page_chunks_splits_at_both_boundaries_independently() {
        // Source 16 bytes from its page end, destination 40 from its own: the
        // 128-byte copy has to break at 16, then at 24 more, then the rest.
        let src = 0x80_0000 + PAGE - 16;
        let dst = 0x90_0000 + PAGE - 40;
        let (c, n) = collect_dual::<4>(src, dst, 128);
        assert_eq!(n, 3);
        assert_eq!(c[0].len, 16);
        assert_eq!(c[1].len, 24);
        assert_eq!(c[2].len, 128 - 40);
        // Every chunk stays inside one page on both sides.
        for chunk in c.iter().take(n) {
            assert!(chunk.src_offset + chunk.len <= PAGE as usize);
            assert!(chunk.dst_offset + chunk.len <= PAGE as usize);
        }
    }

    #[test]
    fn dual_page_chunks_are_contiguous_and_total_len() {
        let src = 0x80_0000 + 0x123;
        let dst = 0x90_0000 + 0xF01;
        let len = 3 * PAGE as usize + 17;
        let mut next_src = src;
        let mut next_dst = dst;
        let mut total = 0usize;
        for c in dual_page_chunks(src, dst, len) {
            assert!(c.len > 0);
            assert_eq!(c.src_page_va % PAGE, 0);
            assert_eq!(c.dst_page_va % PAGE, 0);
            assert_eq!(c.src_page_va + c.src_offset as u64, next_src);
            assert_eq!(c.dst_page_va + c.dst_offset as u64, next_dst);
            assert!(c.src_offset + c.len <= PAGE as usize);
            assert!(c.dst_offset + c.len <= PAGE as usize);
            next_src += c.len as u64;
            next_dst += c.len as u64;
            total += c.len;
        }
        assert_eq!(total, len);
        assert_eq!(next_src, src + len as u64);
        assert_eq!(next_dst, dst + len as u64);
    }

    #[test]
    fn dual_page_chunks_degenerates_to_page_chunks_when_offsets_agree() {
        // Equal page offsets on both sides means neither boundary splits
        // before the other, so the chunking must match the single-sided form.
        let src = 0x80_0000 + 0x800;
        let dst = 0x90_0000 + 0x800;
        let len = 2 * PAGE as usize + 5;
        let mut single = page_chunks(src, len);
        for c in dual_page_chunks(src, dst, len) {
            let s = single.next().expect("same chunk count");
            assert_eq!(c.len, s.len);
            assert_eq!(c.src_offset, s.offset);
            assert_eq!(c.dst_offset, s.offset);
        }
        assert!(single.next().is_none());
    }

    #[test]
    fn page_chunks_are_contiguous_and_total_len() {
        let va = 0x80_0000 + 0x123 * 8;
        let len = 3 * PAGE as usize + 17;
        let mut next_va = va;
        let mut total = 0usize;
        for c in page_chunks(va, len) {
            assert!(c.len > 0);
            assert_eq!(c.page_va % PAGE, 0);
            assert_eq!(c.page_va + c.offset as u64, next_va);
            assert!(c.offset + c.len <= PAGE as usize);
            next_va += c.len as u64;
            total += c.len;
        }
        assert_eq!(total, len);
        assert_eq!(next_va, va + len as u64);
    }
}
