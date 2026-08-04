// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Bounds an ELF image must respect before the kernel will map it (slice 5.9,
//! decision D6).
//!
//! Until this slice every image the loader ever saw was produced by
//! `kernel/build.rs` and gated at pack time by `brand::scan_brand`, so the header
//! fields could be trusted to be small: a `p_memsz` of `u64::MAX` was a build bug,
//! not an input. exec-from-FS ends that. The bytes now come out of a filesystem,
//! through a grant, from a process that may have written them itself — so the
//! header's own numbers decide how many frames the kernel allocates, and nothing
//! else bounds them.
//!
//! Two caps close that, and they are separate because they answer different
//! questions:
//!
//! * [`MAX_PHNUM`] bounds the *header walk* — how many program headers the loader
//!   will even look at. Without it a `e_phnum` of 65535 is 65535 chunked reads
//!   through a page-table walk before the first frame is allocated.
//! * [`MAX_IMAGE_PAGES`] bounds the *mapping* — the total pages across every
//!   `PT_LOAD`, accumulated by [`PageBudget`]. Per-segment checking is not enough:
//!   a hundred segments of a hundred pages each is a hundred segments that
//!   individually look reasonable.
//!
//! Everything here is pure and host-tested, which is the whole reason it lives in
//! `kernel-shared` rather than beside the loader: `kernel/src/` has no
//! `#[cfg(test)]` (the crate is bare-metal only), so a predicate that is not here
//! is a predicate with no test. Same carve-out [`crate::message::user_va_ok`] and
//! [`crate::execstack`] already use.

use crate::message::{USER_PAGE_SIZE, USER_VA_TOP};

/// Most program headers the loader will walk.
///
/// Generous against what this repo produces — the SDK `hello` has 4 `PT_LOAD`s
/// plus a `PT_NOTE`, and a Rust server fewer — and small enough that a hostile
/// `e_phnum` cannot turn the header walk into thousands of cross-address-space
/// reads. An image claiming more is [`ImageError::TooManyHeaders`], never
/// silently truncated to the first `MAX_PHNUM`: a loader that mapped some of an
/// image's segments and ignored the rest would produce a process missing its
/// `.data`.
pub const MAX_PHNUM: usize = 64;

/// Most pages one image may map, across every `PT_LOAD` together.
///
/// 4 MiB. The largest thing this repo execs is the musl `hello` at ~200 KB of
/// file plus its BSS, so the cap is two orders of magnitude clear of real use and
/// exists only to bound a malformed header. It is deliberately *not* tuned to the
/// current binaries: a cap that a legitimate build could reach would be
/// rediscovered as a mysterious `ENOEXEC`.
pub const MAX_IMAGE_PAGES: usize = 1024;

/// [`MAX_IMAGE_PAGES`] in bytes — the cap `do_exec` applies to a *granted* image's
/// length before it does anything else with it.
pub const MAX_IMAGE_BYTES: usize = MAX_IMAGE_PAGES * USER_PAGE_SIZE as usize;

/// Why an image was refused. Every variant maps to `ENOEXEC` at the `SYS_EXEC`
/// boundary — "this is not an executable this kernel will run" — rather than to
/// `ENOMEM`, which would blame the machine for the file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ImageError {
    /// `e_phnum` exceeds [`MAX_PHNUM`].
    TooManyHeaders,
    /// The segments together exceed [`MAX_IMAGE_PAGES`], or a page count
    /// overflowed on the way to finding that out.
    TooLarge,
    /// A segment's `[p_vaddr, p_vaddr + p_memsz)` overflows or leaves the user
    /// address range.
    BadSpan,
}

/// Is `e_phnum` small enough to walk?
pub const fn phnum_ok(e_phnum: usize) -> bool {
    e_phnum <= MAX_PHNUM
}

/// The address range a segment will occupy, page-count included, or why it cannot
/// have one.
///
/// Checked throughout: `p_vaddr` and `p_memsz` are attacker-controlled header
/// fields, so `p_vaddr + p_memsz` is exactly the addition that must not wrap. A
/// wrapped end would read as a tiny segment and then have the per-page loop walk
/// off the top of the address space one page at a time.
pub fn segment_end(p_vaddr: u64, p_memsz: usize) -> Result<u64, ImageError> {
    let end = p_vaddr
        .checked_add(p_memsz as u64)
        .ok_or(ImageError::BadSpan)?;
    if end > USER_VA_TOP {
        return Err(ImageError::BadSpan);
    }
    Ok(end)
}

/// Running total of the pages an image's `PT_LOAD`s will map.
///
/// A value the loader threads through its segment loop rather than a free
/// function, because the property under test is *cumulative*: each segment is
/// charged as it is reached, so the first one that pushes the image past
/// [`MAX_IMAGE_PAGES`] fails before its frames are allocated — not after the
/// whole image has been mapped and measured.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct PageBudget {
    pages: usize,
}

impl PageBudget {
    /// A budget with nothing charged against it yet.
    pub const fn new() -> Self {
        PageBudget { pages: 0 }
    }

    /// Charge one segment's `p_memsz` and report the pages *that* segment needs.
    ///
    /// `p_memsz`, not `p_filesz`: the BSS tail is mapped too (as zeroed frames),
    /// so it is the memory size that costs pages. A zero-length segment costs
    /// nothing and is legal — `PT_LOAD`s with `p_memsz == 0` appear in the wild
    /// and mapping zero pages for one is the correct no-op.
    pub fn charge(&mut self, p_memsz: usize) -> Result<usize, ImageError> {
        let n = p_memsz.div_ceil(USER_PAGE_SIZE as usize);
        let total = self.pages.checked_add(n).ok_or(ImageError::TooLarge)?;
        if total > MAX_IMAGE_PAGES {
            return Err(ImageError::TooLarge);
        }
        self.pages = total;
        Ok(n)
    }

    /// Pages charged so far.
    pub const fn pages(&self) -> usize {
        self.pages
    }
}

// The byte cap must be expressible as a `usize` range on the 64-bit targets this
// kernel supports, and must stay a whole number of pages.
const _: () = assert!(MAX_IMAGE_BYTES.is_multiple_of(USER_PAGE_SIZE as usize));
const _: () = assert!(MAX_IMAGE_BYTES <= i32::MAX as usize);
const _: () = assert!(MAX_PHNUM > 0);

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: usize = USER_PAGE_SIZE as usize;

    #[test]
    fn a_realistic_header_count_is_accepted() {
        // The SDK `hello` has 4 PT_LOADs plus a PT_NOTE; a Rust server has 3 plus
        // a note. The cap must be nowhere near either, or a legitimate build
        // would hit it as a mysterious ENOEXEC.
        for n in [1usize, 4, 8, MAX_PHNUM] {
            assert!(phnum_ok(n), "phnum {n}");
        }
        assert!(!phnum_ok(MAX_PHNUM + 1));
        assert!(!phnum_ok(u16::MAX as usize));
    }

    #[test]
    fn a_segment_costs_the_pages_its_memsz_spans() {
        let mut b = PageBudget::new();
        assert_eq!(b.charge(1), Ok(1), "one byte still needs a whole page");
        assert_eq!(b.charge(PAGE), Ok(1));
        assert_eq!(b.charge(PAGE + 1), Ok(2), "a partial tail page counts");
        assert_eq!(b.pages(), 4);
    }

    #[test]
    fn a_zero_length_segment_is_free_and_legal() {
        // PT_LOADs with p_memsz == 0 do occur; mapping nothing is the right
        // answer, not an error.
        let mut b = PageBudget::new();
        assert_eq!(b.charge(0), Ok(0));
        assert_eq!(b.pages(), 0);
    }

    #[test]
    fn the_budget_is_cumulative_not_per_segment() {
        // The property the whole type exists for: many individually-reasonable
        // segments must still be refused in aggregate.
        let mut b = PageBudget::new();
        let each = MAX_IMAGE_PAGES / 4;
        for _ in 0..4 {
            assert_eq!(b.charge(each * PAGE), Ok(each));
        }
        assert_eq!(b.pages(), MAX_IMAGE_PAGES);
        assert_eq!(b.charge(1), Err(ImageError::TooLarge));
    }

    #[test]
    fn the_cap_is_reachable_exactly() {
        // Off-by-one at the boundary: exactly MAX_IMAGE_PAGES is fine, one more
        // is not — so the cap is a limit rather than an approximation.
        let mut b = PageBudget::new();
        assert_eq!(b.charge(MAX_IMAGE_BYTES), Ok(MAX_IMAGE_PAGES));
        assert_eq!(b.charge(1), Err(ImageError::TooLarge));

        let mut b = PageBudget::new();
        assert_eq!(b.charge(MAX_IMAGE_BYTES + 1), Err(ImageError::TooLarge));
    }

    #[test]
    fn a_hostile_memsz_cannot_overflow_the_page_count() {
        // `usize::MAX.div_ceil(PAGE)` is enormous but finite; the cap catches it
        // before the addition can wrap, and the addition is checked regardless.
        let mut b = PageBudget::new();
        assert_eq!(b.charge(usize::MAX), Err(ImageError::TooLarge));
        assert_eq!(b.pages(), 0, "a refused segment charges nothing");
    }

    #[test]
    fn a_refused_segment_leaves_the_budget_untouched() {
        // So a loader that reported the error and (wrongly) carried on would not
        // also have corrupted the running total.
        let mut b = PageBudget::new();
        assert_eq!(b.charge(PAGE), Ok(1));
        assert_eq!(b.charge(MAX_IMAGE_BYTES), Err(ImageError::TooLarge));
        assert_eq!(b.pages(), 1);
    }

    #[test]
    fn an_ordinary_segment_span_is_accepted() {
        // The load base every `user.ld` in the tree uses.
        assert_eq!(segment_end(0x10_0000, PAGE), Ok(0x10_0000 + PAGE as u64));
        assert_eq!(segment_end(0, 0), Ok(0));
    }

    #[test]
    fn a_span_that_wraps_is_rejected() {
        // The addition that must not be `+`: a wrapped end reads as a tiny
        // segment whose per-page loop then walks off the top of the space.
        assert_eq!(segment_end(u64::MAX, 1), Err(ImageError::BadSpan));
        assert_eq!(segment_end(u64::MAX - 8, 4096), Err(ImageError::BadSpan));
    }

    #[test]
    fn a_span_past_the_user_range_is_rejected() {
        assert_eq!(segment_end(USER_VA_TOP, 1), Err(ImageError::BadSpan));
        assert_eq!(segment_end(USER_VA_TOP - 1, 2), Err(ImageError::BadSpan));
        assert_eq!(
            segment_end(USER_VA_TOP - PAGE as u64, PAGE),
            Ok(USER_VA_TOP),
            "ending exactly at the top is in range"
        );
    }
}
