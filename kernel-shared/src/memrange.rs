// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Half-open physical-range arithmetic the kernel cannot host-test itself.
//!
//! `kernel/src/` is bare-metal only and carries no `#[cfg(test)]`, so arithmetic
//! that decides which frames the allocator may hand out lives here, as a pure
//! function with host tests. Slice 6.7 is the first consumer: a Limine
//! framebuffer that overlaps a usable memory-map entry must be removed from the
//! frame allocator before any frame is handed out (phase decision H9, slice
//! decision F5), and [`subtract`] is that removal.
//!
//! Nothing here is an ABI: no value crosses a process boundary, and nothing is
//! emitted into the generated C headers.

/// A half-open range of physical addresses, `(base, end)`: `base` is the first
/// byte in the range and `end` is one past the last. `base == end` is the empty
/// range.
///
/// **Precondition, everywhere a `Range` is taken: `base <= end`.** A range with
/// `base > end` is not a range; [`subtract`] treats one as empty rather than
/// panicking, so a caller that broke the precondition loses memory, never
/// safety.
pub type Range = (u64, u64);

/// Subtracts `hole` from `region`, returning what is left of `region`.
///
/// The result has two fixed slots:
///
/// - `[0]` is the part of `region` **below** the hole — `(region.base,
///   min(region.end, hole.base))` — or `None` when that is empty;
/// - `[1]` is the part of `region` **above** the hole — `(max(region.base,
///   hole.end), region.end)` — or `None` when that is empty.
///
/// So a hole strictly inside the region gives two ranges, a hole over the head
/// or the tail gives one, a hole covering the region gives none, and a hole
/// that misses the region gives the region back whole (in `[1]` when the hole
/// lies below it and in `[0]` when it lies above). Ranges are half-open, so a
/// hole whose `end` equals the region's `base`, or whose `base` equals the
/// region's `end`, is adjacent and removes nothing.
///
/// An **empty hole** removes nothing and never splits: the region comes back
/// whole in `[0]`, wherever the hole's address is. An **empty region** gives
/// `[None, None]`. No returned range is ever empty, and every returned range
/// lies inside `region` and outside `hole`.
///
/// Precondition: `base <= end` for both arguments (see [`Range`]). The function
/// only compares and selects — no addition or subtraction — so it cannot
/// overflow and does not panic for any `u64` inputs, the precondition's
/// violations included.
#[must_use]
pub const fn subtract(region: Range, hole: Range) -> [Option<Range>; 2] {
    let (base, end) = region;
    let (hole_base, hole_end) = hole;
    if base >= end {
        return [None, None];
    }
    // An empty hole removes nothing. It needs its own arm: the general one
    // below would split the region in two at the hole's address.
    if hole_base >= hole_end {
        return [Some(region), None];
    }
    // `u64::min`/`max` are not const-callable through `Ord`, hence the `if`s.
    let below_end = if hole_base < end { hole_base } else { end };
    let above_base = if hole_end > base { hole_end } else { base };
    [
        if base < below_end {
            Some((base, below_end))
        } else {
            None
        },
        if above_base < end {
            Some((above_base, end))
        } else {
            None
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGION: Range = (0x1000, 0x9000);

    #[test]
    fn a_hole_entirely_below_removes_nothing() {
        assert_eq!(subtract(REGION, (0x0, 0x800)), [None, Some(REGION)]);
    }

    #[test]
    fn a_hole_entirely_above_removes_nothing() {
        assert_eq!(subtract(REGION, (0xA000, 0xB000)), [Some(REGION), None]);
    }

    #[test]
    fn a_hole_adjacent_below_removes_nothing() {
        // Half-open: the hole's `end` is the region's `base`, so they share no
        // byte.
        assert_eq!(subtract(REGION, (0x0, 0x1000)), [None, Some(REGION)]);
    }

    #[test]
    fn a_hole_adjacent_above_removes_nothing() {
        assert_eq!(subtract(REGION, (0x9000, 0xA000)), [Some(REGION), None]);
    }

    #[test]
    fn a_hole_covering_the_region_exactly_leaves_nothing() {
        assert_eq!(subtract(REGION, REGION), [None, None]);
    }

    #[test]
    fn a_hole_covering_more_than_the_region_leaves_nothing() {
        assert_eq!(subtract(REGION, (0x0, 0x1_0000)), [None, None]);
        // Wider on one side only, flush on the other.
        assert_eq!(subtract(REGION, (0x0, 0x9000)), [None, None]);
        assert_eq!(subtract(REGION, (0x1000, 0x1_0000)), [None, None]);
        assert_eq!(subtract(REGION, (0, u64::MAX)), [None, None]);
    }

    #[test]
    fn a_hole_at_the_head_leaves_the_tail() {
        assert_eq!(
            subtract(REGION, (0x1000, 0x3000)),
            [None, Some((0x3000, 0x9000))]
        );
        // Starting below the region and ending inside it is the same answer.
        assert_eq!(
            subtract(REGION, (0x0, 0x3000)),
            [None, Some((0x3000, 0x9000))]
        );
    }

    #[test]
    fn a_hole_at_the_tail_leaves_the_head() {
        assert_eq!(
            subtract(REGION, (0x7000, 0x9000)),
            [Some((0x1000, 0x7000)), None]
        );
        // Starting inside the region and ending above it is the same answer.
        assert_eq!(
            subtract(REGION, (0x7000, 0xF000)),
            [Some((0x1000, 0x7000)), None]
        );
    }

    #[test]
    fn a_hole_in_the_middle_leaves_two_ranges() {
        assert_eq!(
            subtract(REGION, (0x4000, 0x5000)),
            [Some((0x1000, 0x4000)), Some((0x5000, 0x9000))]
        );
    }

    #[test]
    fn a_hole_one_byte_inside_each_edge_leaves_one_byte_on_each_side() {
        // The hole starts one byte after the region's base and ends one byte
        // before its end: exactly one byte survives on each side.
        assert_eq!(
            subtract(REGION, (0x1001, 0x8FFF)),
            [Some((0x1000, 0x1001)), Some((0x8FFF, 0x9000))]
        );
    }

    #[test]
    fn a_hole_overlapping_the_region_by_one_byte_removes_that_byte() {
        // One byte over the region's first byte, coming from below.
        assert_eq!(
            subtract(REGION, (0x0, 0x1001)),
            [None, Some((0x1001, 0x9000))]
        );
        // One byte over the region's last byte, running on above.
        assert_eq!(
            subtract(REGION, (0x8FFF, 0xF000)),
            [Some((0x1000, 0x8FFF)), None]
        );
        // A one-byte hole on the first byte, the last byte, and in the middle.
        assert_eq!(
            subtract(REGION, (0x1000, 0x1001)),
            [None, Some((0x1001, 0x9000))]
        );
        assert_eq!(
            subtract(REGION, (0x8FFF, 0x9000)),
            [Some((0x1000, 0x8FFF)), None]
        );
        assert_eq!(
            subtract(REGION, (0x4000, 0x4001)),
            [Some((0x1000, 0x4000)), Some((0x4001, 0x9000))]
        );
    }

    #[test]
    fn an_empty_hole_removes_nothing_and_never_splits() {
        // Below, at the base, inside, at the end, above: the region comes back
        // whole, in slot 0, every time. The inside case is the one a naive
        // min/max would split in two.
        for at in [0x0, 0x1000, 0x4000, 0x9000, 0xF000, u64::MAX] {
            assert_eq!(
                subtract(REGION, (at, at)),
                [Some(REGION), None],
                "empty hole at {at:#x}"
            );
        }
    }

    #[test]
    fn an_empty_region_leaves_nothing() {
        for at in [0x0, 0x1000, 0x4000, 0x9000, u64::MAX] {
            // Whatever the hole: covering it, missing it, or itself empty.
            for hole in [(0x0, 0x800), (0x0, u64::MAX), (at, at), (0xF000, 0xF800)] {
                assert_eq!(
                    subtract((at, at), hole),
                    [None, None],
                    "region ({at:#x}, {at:#x}) minus {hole:#x?}"
                );
            }
        }
    }

    #[test]
    fn the_extremes_of_u64_neither_overflow_nor_panic() {
        let all = (0, u64::MAX);
        assert_eq!(subtract(all, all), [None, None]);
        assert_eq!(subtract(all, (0, 1)), [None, Some((1, u64::MAX))]);
        assert_eq!(
            subtract(all, (u64::MAX - 1, u64::MAX)),
            [Some((0, u64::MAX - 1)), None]
        );
        assert_eq!(
            subtract(all, (1, u64::MAX - 1)),
            [Some((0, 1)), Some((u64::MAX - 1, u64::MAX))]
        );
        assert_eq!(
            subtract((u64::MAX - 1, u64::MAX), (0, u64::MAX - 1)),
            [None, Some((u64::MAX - 1, u64::MAX))]
        );
    }

    #[test]
    fn an_inverted_argument_is_treated_as_empty_not_a_panic() {
        // Outside the precondition; pinned so the total-function claim in the
        // doc comment is a tested one. An inverted hole removes nothing; an
        // inverted region has nothing to keep.
        assert_eq!(subtract(REGION, (0x5000, 0x4000)), [Some(REGION), None]);
        assert_eq!(subtract((0x9000, 0x1000), (0x4000, 0x5000)), [None, None]);
        assert_eq!(subtract((u64::MAX, 0), (u64::MAX, 0)), [None, None]);
    }

    #[test]
    fn every_result_lies_inside_the_region_and_outside_the_hole() {
        // The property the frame allocator needs, swept over every pair of
        // ranges drawn from a small set of interesting addresses: what comes
        // back is non-empty, inside the region, clear of the hole, in ascending
        // order, and together with the removed overlap accounts for every byte
        // of the region.
        let points = [0_u64, 1, 2, 3, 4, 5, 6, u64::MAX - 1, u64::MAX];
        for &rb in &points {
            for &re in points.iter().filter(|&&e| e >= rb) {
                for &hb in &points {
                    for &he in points.iter().filter(|&&e| e >= hb) {
                        let out = subtract((rb, re), (hb, he));
                        let mut kept = 0_u64;
                        for (b, e) in out.into_iter().flatten() {
                            assert!(b < e, "empty result");
                            assert!(rb <= b && e <= re, "result outside the region");
                            // An empty hole contains no byte to overlap.
                            assert!(hb == he || e <= hb || he <= b, "result overlaps the hole");
                            kept += e - b;
                        }
                        if let [Some((_, e0)), Some((b1, _))] = out {
                            assert!(e0 <= b1, "results out of order");
                        }
                        let overlap = re.min(he).saturating_sub(rb.max(hb));
                        assert_eq!(
                            kept,
                            (re - rb) - overlap,
                            "({rb:#x}, {re:#x}) minus ({hb:#x}, {he:#x})"
                        );
                    }
                }
            }
        }
    }
}
