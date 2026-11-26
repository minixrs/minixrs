// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Grid cells to pixel stores, and back (slice 6.7, decisions F6, F8, F10).
//!
//! Everything here writes through one trait method, [`Surface::store`]: one
//! pixel, one `u32`, at `(x, y)`. The TTY binary implements it with an aligned
//! volatile store at `FB_VA + y * pitch + x * 4`; the host tests and
//! `tools/fbcheck` implement it over a `Vec<u32>`. Nothing here *reads* a
//! surface — a scroll is repainted from the grid — which is the device-memory
//! rule (H9) made structural: the trait has no load to call.
//!
//! ## Two guarantees the bin relies on
//!
//! * **No store outside `width × height`.** A cell is painted only if it lies
//!   inside [`Format::cols`] × [`Format::rows`], and those are `width / 8` and
//!   `height / 16` rounded *down* — a screen 13 pixels wide has one column and a
//!   5-pixel margin that only [`clear`] ever touches.
//! * **[`Format`] is validated, not trusted.** [`Format::new`] re-checks every
//!   condition the kernel applied to Limine's answer (F2) on the copy that came
//!   back through `GET_FRAMEBUFFER`, with checked arithmetic, so the bin's
//!   `y * pitch + x * 4` cannot leave `byte_len()` for any `(x, y)` this module
//!   stores at.
//!
//! `pitch` is carried but not used here: a `Surface` is addressed in pixels,
//! and turning a pixel into a byte offset is the implementation's business.
//!
//! ## Decoding
//!
//! [`decode_cell`] is the inverse of [`paint_cell`], for the host-side screen
//! check (F10): given a way to read a pixel of a *dump*, it says which byte's
//! glyph is in a cell, pixel-exactly, or that none is.

use crate::font::{CELL_H, CELL_W, FIRST_PRINTABLE, Glyph, LAST_PRINTABLE, glyph};
use crate::grid::{Grid, MAX_COLS, MAX_ROWS};

/// The console's foreground, light grey, as `(red, green, blue)` (F8).
pub const FG: (u8, u8, u8) = (0xAA, 0xAA, 0xAA);
/// The console's background, black.
pub const BG: (u8, u8, u8) = (0x00, 0x00, 0x00);

/// The only pixel size drawn (F2).
pub const BITS_PER_PIXEL: u8 = 32;
/// Bytes per pixel at [`BITS_PER_PIXEL`].
pub const BYTES_PER_PIXEL: u32 = 4;

/// A glyph row's leftmost pixel: bit 7.
const LEFTMOST: u8 = 0x80;

/// Somewhere pixels can be stored. The one write a renderer makes.
pub trait Surface {
    /// Store `pixel` at column `x`, row `y`. Only ever called with
    /// `x < width` and `y < height` of the [`Format`] being painted.
    fn store(&mut self, x: u32, y: u32, pixel: u32);
}

/// Which of F2's conditions a framebuffer description fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    /// `bpp` is not 32.
    Bpp,
    /// A channel shift is not one of 0, 8, 16, 24.
    Shift,
    /// Two channels share a shift.
    ShiftsOverlap,
    /// `width` or `height` is zero.
    ZeroSize,
    /// `pitch` is not a multiple of 4, or is less than `width * 4` (including
    /// a `width * 4` that does not fit a `u32`).
    Pitch,
}

impl FormatError {
    /// A short fixed name, for a `[diag tty] fb FAIL <reason>` line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bpp => "bpp",
            Self::Shift => "shift",
            Self::ShiftsOverlap => "shifts-overlap",
            Self::ZeroSize => "zero-size",
            Self::Pitch => "pitch",
        }
    }
}

/// A validated framebuffer description: geometry, pixel layout, and the text
/// grid that fits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    width: u32,
    height: u32,
    pitch: u32,
    red_shift: u8,
    green_shift: u8,
    blue_shift: u8,
    cols: usize,
    rows: usize,
}

impl Format {
    /// Validate a description (F2's conditions, re-checked) and work out the
    /// grid: `cols = min(width / 8, MAX_COLS)`, `rows = min(height / 16,
    /// MAX_ROWS)`.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        width: u32,
        height: u32,
        pitch: u32,
        red_shift: u8,
        green_shift: u8,
        blue_shift: u8,
        bpp: u8,
    ) -> Result<Self, FormatError> {
        if bpp != BITS_PER_PIXEL {
            return Err(FormatError::Bpp);
        }
        if !shift_ok(red_shift) || !shift_ok(green_shift) || !shift_ok(blue_shift) {
            return Err(FormatError::Shift);
        }
        if red_shift == green_shift || green_shift == blue_shift || red_shift == blue_shift {
            return Err(FormatError::ShiftsOverlap);
        }
        if width == 0 || height == 0 {
            return Err(FormatError::ZeroSize);
        }
        // `width * 4` wraps from 2^30 up, and release builds do not check.
        let Some(row_bytes) = width.checked_mul(BYTES_PER_PIXEL) else {
            return Err(FormatError::Pitch);
        };
        if pitch < row_bytes || !pitch.is_multiple_of(BYTES_PER_PIXEL) {
            return Err(FormatError::Pitch);
        }
        Ok(Self {
            width,
            height,
            pitch,
            red_shift,
            green_shift,
            blue_shift,
            cols: clamp_cells(width / CELL_W, MAX_COLS),
            rows: clamp_cells(height / CELL_H, MAX_ROWS),
        })
    }

    /// Width in pixels; never zero.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels; never zero.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Bytes from one row's first pixel to the next row's: a multiple of 4 and
    /// at least `width * 4`. Not used by this module — see the module docs.
    pub const fn pitch(&self) -> u32 {
        self.pitch
    }

    /// Text columns: whole cells across, at most [`crate::grid::MAX_COLS`].
    pub const fn cols(&self) -> usize {
        self.cols
    }

    /// Text rows: whole cells down, at most [`crate::grid::MAX_ROWS`].
    pub const fn rows(&self) -> usize {
        self.rows
    }

    /// The framebuffer's size in bytes, `pitch * height`. Two `u32`s cannot
    /// overflow a `u64`, so this is exact for every `Format`; comparing it with
    /// the mapped window is the bin's check.
    pub const fn byte_len(&self) -> u64 {
        // Lossless widening; `u64::from` is not callable in a `const fn`.
        self.pitch as u64 * self.height as u64
    }

    /// Pack a colour into a pixel through the channel shifts. The fourth byte
    /// is zero.
    pub const fn pack(&self, red: u8, green: u8, blue: u8) -> u32 {
        // Each shift is at most 24 (checked in `new`), so no byte is lost.
        (red as u32) << self.red_shift
            | (green as u32) << self.green_shift
            | (blue as u32) << self.blue_shift
    }

    /// The foreground pixel: [`FG`] packed.
    pub const fn fg(&self) -> u32 {
        self.pack(FG.0, FG.1, FG.2)
    }

    /// The background pixel: [`BG`] packed.
    pub const fn bg(&self) -> u32 {
        self.pack(BG.0, BG.1, BG.2)
    }
}

/// A channel shift this module can pack through: a byte boundary inside a
/// 32-bit pixel.
const fn shift_ok(shift: u8) -> bool {
    matches!(shift, 0 | 8 | 16 | 24)
}

/// `min(cells, max)` as a count of cells. `max` is small, so whenever `cells`
/// would not fit a `usize` the answer is `max` anyway.
const fn clamp_cells(cells: u32, max: usize) -> usize {
    if cells as u64 >= max as u64 {
        max
    } else {
        cells as usize
    }
}

/// The pixel at the top-left of cell `(col, row)`, or `None` if the cell is
/// outside the format's grid.
///
/// Inside the grid, `col < cols <= width / 8`, so `col * 8 + 7 < width` and
/// likewise down the screen: every pixel of the cell is on the surface, and
/// none of the arithmetic below can overflow a `u32`. That is the whole of the
/// "never store outside `width × height`" argument.
fn cell_origin(fmt: &Format, col: usize, row: usize) -> Option<(u32, u32)> {
    if col >= fmt.cols || row >= fmt.rows {
        return None;
    }
    let x = u32::try_from(col).ok()?.checked_mul(CELL_W)?;
    let y = u32::try_from(row).ok()?.checked_mul(CELL_H)?;
    Some((x, y))
}

/// Paint `byte`'s glyph in cell `(col, row)`: exactly 128 stores, every pixel
/// of the cell, foreground or background. A cell outside the format's grid is
/// not painted at all.
pub fn paint_cell<S: Surface + ?Sized>(
    surface: &mut S,
    fmt: &Format,
    col: usize,
    row: usize,
    byte: u8,
) {
    let Some((x0, y0)) = cell_origin(fmt, col, row) else {
        return;
    };
    let (fg, bg) = (fmt.fg(), fmt.bg());
    for (y, bits) in (y0..).zip(glyph(byte)) {
        for (x, bit) in (x0..).zip(0..CELL_W) {
            let lit = bits & (LEFTMOST >> bit) != 0;
            surface.store(x, y, if lit { fg } else { bg });
        }
    }
}

/// Paint every cell of `grid` that fits the format's grid.
pub fn paint_all<S: Surface + ?Sized>(surface: &mut S, fmt: &Format, grid: &Grid) {
    for row in 0..grid.rows().min(fmt.rows) {
        for (col, &byte) in grid.row(row).iter().enumerate().take(fmt.cols) {
            paint_cell(surface, fmt, col, row, byte);
        }
    }
}

/// Store the background in every pixel of `width × height` — including the
/// margins right of the last column and below the last row, which nothing else
/// paints.
pub fn clear<S: Surface + ?Sized>(surface: &mut S, fmt: &Format) {
    let bg = fmt.bg();
    for y in 0..fmt.height {
        for x in 0..fmt.width {
            surface.store(x, y, bg);
        }
    }
}

/// Which byte's glyph is in cell `(col, row)` of an image read through
/// `pixels(x, y)`: the byte in `0x20..=0x7E` whose painted cell matches all 128
/// pixels exactly (a blank cell is `0x20`). `None` if a pixel is neither console
/// colour, if no glyph matches, or if the cell is outside the format's grid.
///
/// Reads only the cell's own pixels. The font's printable glyphs are pairwise
/// distinct (a `font` test), so the answer is unique.
pub fn decode_cell(
    pixels: impl Fn(u32, u32) -> u32,
    fmt: &Format,
    col: usize,
    row: usize,
) -> Option<u8> {
    let (x0, y0) = cell_origin(fmt, col, row)?;
    let (fg, bg) = (fmt.fg(), fmt.bg());
    let mut seen: Glyph = [0; CELL_H as usize];
    for (y, bits) in (y0..).zip(seen.iter_mut()) {
        for (x, bit) in (x0..).zip(0..CELL_W) {
            let pixel = pixels(x, y);
            if pixel == fg {
                *bits |= LEFTMOST >> bit;
            } else if pixel != bg {
                return None;
            }
        }
    }
    (FIRST_PRINTABLE..=LAST_PRINTABLE).find(|&byte| *glyph(byte) == seen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Damage;

    /// Task 0's measured geometry: 800×600, pitch 3200, `r=16 g=8 b=0`.
    fn measured() -> Format {
        Format::new(800, 600, 3200, 16, 8, 0, 32).unwrap()
    }

    fn fmt(width: u32, height: u32) -> Format {
        Format::new(width, height, width * 4, 16, 8, 0, 32).unwrap()
    }

    /// A pixel never stored: not a colour any `Format` here can pack.
    const UNTOUCHED: u32 = 0xFFFF_FFFF;

    /// A `width × height` image that records every store and **panics** on one
    /// outside it — so any test that paints is also an out-of-bounds test.
    struct Image {
        width: u32,
        height: u32,
        pixels: Vec<u32>,
        /// Stores per pixel.
        hits: Vec<u32>,
        stores: usize,
    }

    impl Image {
        fn new(f: &Format) -> Self {
            let n = (f.width() * f.height()) as usize;
            Self {
                width: f.width(),
                height: f.height(),
                pixels: vec![UNTOUCHED; n],
                hits: vec![0; n],
                stores: 0,
            }
        }

        fn at(&self, x: u32, y: u32) -> u32 {
            assert!(x < self.width && y < self.height, "read outside the image");
            self.pixels[(y * self.width + x) as usize]
        }

        fn hits_at(&self, x: u32, y: u32) -> u32 {
            self.hits[(y * self.width + x) as usize]
        }
    }

    impl Surface for Image {
        fn store(&mut self, x: u32, y: u32, pixel: u32) {
            assert!(
                x < self.width && y < self.height,
                "store at ({x}, {y}) outside {}x{}",
                self.width,
                self.height
            );
            let i = (y * self.width + x) as usize;
            self.pixels[i] = pixel;
            self.hits[i] += 1;
            self.stores += 1;
        }
    }

    // --- Format: accepting ------------------------------------------------

    #[test]
    fn the_measured_format_is_accepted() {
        let f = measured();
        assert_eq!((f.width(), f.height(), f.pitch()), (800, 600, 3200));
        assert_eq!((f.cols(), f.rows()), (100, 37));
        assert_eq!(f.byte_len(), 1_920_000);
    }

    #[test]
    fn the_grid_is_whole_cells_only() {
        // 600 = 37 * 16 + 8: the bottom 8 pixels are margin.
        assert_eq!(fmt(800, 600).rows(), 37);
        assert_eq!((fmt(13, 21).cols(), fmt(13, 21).rows()), (1, 1));
        assert_eq!((fmt(8, 16).cols(), fmt(8, 16).rows()), (1, 1));
        assert_eq!((fmt(15, 31).cols(), fmt(15, 31).rows()), (1, 1));
        assert_eq!((fmt(16, 32).cols(), fmt(16, 32).rows()), (2, 2));
    }

    #[test]
    fn a_screen_smaller_than_a_cell_is_accepted_with_no_grid() {
        assert_eq!((fmt(7, 15).cols(), fmt(7, 15).rows()), (0, 0));
        assert_eq!((fmt(7, 100).cols(), fmt(7, 100).rows()), (0, 6));
        assert_eq!((fmt(100, 15).cols(), fmt(100, 15).rows()), (12, 0));
        assert_eq!((fmt(1, 1).cols(), fmt(1, 1).rows()), (0, 0));
    }

    #[test]
    fn the_grid_is_clamped_to_the_maximum() {
        let f = fmt(1920, 1080);
        assert_eq!((f.cols(), f.rows()), (MAX_COLS, MAX_ROWS));
        let f = fmt(1280, 1024);
        assert_eq!((f.cols(), f.rows()), (MAX_COLS, MAX_ROWS));
        let f = fmt(1272, 1008);
        assert_eq!((f.cols(), f.rows()), (159, 63));
    }

    #[test]
    fn a_pitch_wider_than_the_row_is_accepted() {
        let f = Format::new(800, 600, 4096, 16, 8, 0, 32).unwrap();
        assert_eq!(f.pitch(), 4096);
        assert_eq!(f.cols(), 100, "columns come from the width, not the pitch");
        assert_eq!(f.byte_len(), 4096 * 600);
        // Exactly width * 4 is the minimum.
        assert!(Format::new(800, 600, 3200, 16, 8, 0, 32).is_ok());
    }

    #[test]
    fn every_arrangement_of_the_four_shifts_is_accepted() {
        let shifts = [0u8, 8, 16, 24];
        let mut accepted = 0;
        for r in shifts {
            for g in shifts {
                for b in shifts {
                    let result = Format::new(8, 16, 32, r, g, b, 32);
                    if r != g && g != b && r != b {
                        assert!(result.is_ok(), "r={r} g={g} b={b}");
                        accepted += 1;
                    } else {
                        assert_eq!(result, Err(FormatError::ShiftsOverlap), "r={r} g={g} b={b}");
                    }
                }
            }
        }
        assert_eq!(accepted, 24);
    }

    // --- Format: refusing (Review Focus 5) ---------------------------------

    #[test]
    fn any_bpp_but_32_is_refused() {
        for bpp in 0..=u8::MAX {
            let result = Format::new(800, 600, 3200, 16, 8, 0, bpp);
            if bpp == 32 {
                assert!(result.is_ok());
            } else {
                assert_eq!(result, Err(FormatError::Bpp), "bpp {bpp}");
            }
        }
    }

    #[test]
    fn a_shift_off_a_byte_boundary_or_past_the_pixel_is_refused() {
        for bad in (0..=u8::MAX).filter(|s| ![0, 8, 16, 24].contains(s)) {
            assert_eq!(
                Format::new(800, 600, 3200, bad, 8, 0, 32),
                Err(FormatError::Shift),
                "red {bad}"
            );
            assert_eq!(
                Format::new(800, 600, 3200, 16, bad, 0, 32),
                Err(FormatError::Shift),
                "green {bad}"
            );
            assert_eq!(
                Format::new(800, 600, 3200, 16, 8, bad, 32),
                Err(FormatError::Shift),
                "blue {bad}"
            );
        }
    }

    #[test]
    fn overlapping_shifts_are_refused() {
        assert_eq!(
            Format::new(800, 600, 3200, 16, 16, 0, 32),
            Err(FormatError::ShiftsOverlap)
        );
        assert_eq!(
            Format::new(800, 600, 3200, 16, 8, 8, 32),
            Err(FormatError::ShiftsOverlap)
        );
        assert_eq!(
            Format::new(800, 600, 3200, 0, 8, 0, 32),
            Err(FormatError::ShiftsOverlap)
        );
        assert_eq!(
            Format::new(800, 600, 3200, 0, 0, 0, 32),
            Err(FormatError::ShiftsOverlap)
        );
    }

    #[test]
    fn a_zero_width_or_height_is_refused() {
        assert_eq!(
            Format::new(0, 600, 3200, 16, 8, 0, 32),
            Err(FormatError::ZeroSize)
        );
        assert_eq!(
            Format::new(800, 0, 3200, 16, 8, 0, 32),
            Err(FormatError::ZeroSize)
        );
        assert_eq!(
            Format::new(0, 0, 0, 16, 8, 0, 32),
            Err(FormatError::ZeroSize)
        );
    }

    #[test]
    fn a_pitch_narrower_than_the_row_is_refused() {
        assert_eq!(
            Format::new(800, 600, 3196, 16, 8, 0, 32),
            Err(FormatError::Pitch),
            "one pixel short"
        );
        assert_eq!(
            Format::new(800, 600, 0, 16, 8, 0, 32),
            Err(FormatError::Pitch)
        );
        assert_eq!(
            Format::new(800, 600, 800, 16, 8, 0, 32),
            Err(FormatError::Pitch),
            "a pitch in pixels, not bytes"
        );
    }

    #[test]
    fn a_pitch_that_is_not_a_multiple_of_four_is_refused() {
        for pitch in [3201, 3202, 3203, 4097] {
            assert_eq!(
                Format::new(800, 600, pitch, 16, 8, 0, 32),
                Err(FormatError::Pitch),
                "pitch {pitch}"
            );
        }
    }

    /// `width * 4` wraps a `u32` from 2^30 up. Unchecked, 0x4000_0000 * 4 is 0
    /// and *any* pitch would pass; the shipped profile has overflow checks off,
    /// so this is the test that keeps the two profiles agreeing.
    #[test]
    fn a_width_whose_row_overflows_is_refused_not_wrapped() {
        assert_eq!(
            Format::new(0x4000_0000, 600, 3200, 16, 8, 0, 32),
            Err(FormatError::Pitch)
        );
        assert_eq!(
            Format::new(0x4000_0001, 600, 4, 16, 8, 0, 32),
            Err(FormatError::Pitch)
        );
        assert_eq!(
            Format::new(u32::MAX, u32::MAX, 0xFFFF_FFFC, 16, 8, 0, 32),
            Err(FormatError::Pitch)
        );
        // The widest row that does fit.
        let f = Format::new(0x3FFF_FFFF, 1, 0xFFFF_FFFC, 16, 8, 0, 32).unwrap();
        assert_eq!(f.cols(), MAX_COLS);
        assert_eq!(f.rows(), 0);
    }

    #[test]
    fn byte_len_is_exact_at_the_extremes() {
        let f = Format::new(1, u32::MAX, 0xFFFF_FFFC, 16, 8, 0, 32).unwrap();
        assert_eq!(f.byte_len(), 0xFFFF_FFFC_u64 * 0xFFFF_FFFF_u64);
        assert_eq!(f.rows(), MAX_ROWS);
        assert_eq!(f.cols(), 0);
    }

    #[test]
    fn every_refusal_has_a_distinct_name() {
        let all = [
            FormatError::Bpp,
            FormatError::Shift,
            FormatError::ShiftsOverlap,
            FormatError::ZeroSize,
            FormatError::Pitch,
        ];
        for (i, a) in all.iter().enumerate() {
            assert!(!a.as_str().is_empty());
            assert!(!a.as_str().contains(' '), "one token on a marker line");
            for b in &all[i + 1..] {
                assert_ne!(a.as_str(), b.as_str());
            }
        }
    }

    // --- colours -------------------------------------------------------------

    #[test]
    fn pack_puts_each_channel_at_its_shift() {
        let xrgb = Format::new(8, 16, 32, 16, 8, 0, 32).unwrap();
        assert_eq!(xrgb.pack(0x11, 0x22, 0x33), 0x0011_2233);
        let xbgr = Format::new(8, 16, 32, 0, 8, 16, 32).unwrap();
        assert_eq!(xbgr.pack(0x11, 0x22, 0x33), 0x0033_2211);
        let rgbx = Format::new(8, 16, 32, 24, 16, 8, 32).unwrap();
        assert_eq!(rgbx.pack(0x11, 0x22, 0x33), 0x1122_3300);
        let odd = Format::new(8, 16, 32, 8, 24, 0, 32).unwrap();
        assert_eq!(odd.pack(0x11, 0x22, 0x33), 0x2200_1133);
        assert_eq!(odd.pack(0xFF, 0xFF, 0xFF), 0xFF00_FFFF);
    }

    #[test]
    fn the_console_is_light_grey_on_black() {
        assert_eq!(FG, (0xAA, 0xAA, 0xAA));
        assert_eq!(BG, (0x00, 0x00, 0x00));
        assert_eq!(measured().fg(), 0x00AA_AAAA);
        assert_eq!(measured().bg(), 0x0000_0000);
        let rgbx = Format::new(8, 16, 32, 24, 16, 8, 32).unwrap();
        assert_eq!(rgbx.fg(), 0xAAAA_AA00);
        assert_eq!(rgbx.bg(), 0);
    }

    // --- paint_cell ------------------------------------------------------------

    /// `A`'s rows again, by hand — the same picture as `font`'s golden glyph —
    /// so this test pins bit order (bit 7 is the *left* pixel) and row order
    /// without going through `glyph()`.
    const A_ROWS: [&str; 16] = [
        "........", "........", ".#####..", "##...##.", "##...##.", "##...##.", "#######.",
        "##...##.", "##...##.", "##...##.", "##...##.", "##...##.", "........", "........",
        "........", "........",
    ];

    #[test]
    fn a_cell_is_128_stores_at_the_right_coordinates() {
        let f = fmt(64, 64);
        let mut img = Image::new(&f);
        paint_cell(&mut img, &f, 3, 2, b'A');
        assert_eq!(img.stores, 128);
        for y in 0..64 {
            for x in 0..64 {
                let inside = (24..32).contains(&x) && (32..48).contains(&y);
                assert_eq!(
                    img.hits_at(x, y),
                    u32::from(inside),
                    "({x}, {y}): each pixel of the cell once, nothing else"
                );
            }
        }
        for (dy, picture) in A_ROWS.iter().enumerate() {
            for (dx, mark) in picture.bytes().enumerate() {
                let want = if mark == b'#' { f.fg() } else { f.bg() };
                assert_eq!(
                    img.at(24 + dx as u32, 32 + dy as u32),
                    want,
                    "row {dy} column {dx}"
                );
            }
        }
    }

    #[test]
    fn a_cell_is_packed_through_the_shifts() {
        // Grey hides a channel swap (mutation 5's honest row), but not a shift
        // of the whole pixel: RGBX puts the zero byte at the bottom.
        let f = Format::new(8, 16, 32, 24, 16, 8, 32).unwrap();
        let mut img = Image::new(&f);
        paint_cell(&mut img, &f, 0, 0, b'A');
        assert_eq!(img.at(1, 2), 0xAAAA_AA00, "a foreground pixel of A");
        assert_eq!(img.at(0, 0), 0x0000_0000, "a background pixel");
    }

    #[test]
    fn a_blank_cell_is_128_background_stores() {
        let f = fmt(16, 16);
        let mut img = Image::new(&f);
        paint_cell(&mut img, &f, 1, 0, b' ');
        assert_eq!(img.stores, 128);
        for y in 0..16 {
            for x in 8..16 {
                assert_eq!(img.at(x, y), f.bg());
            }
            for x in 0..8 {
                assert_eq!(img.at(x, y), UNTOUCHED);
            }
        }
    }

    #[test]
    fn a_cell_outside_the_grid_is_not_painted() {
        let f = fmt(800, 600);
        let mut img = Image::new(&f);
        // (99, 36) is the last cell; row 37 would start at y = 592 and run 8
        // pixels past the bottom.
        paint_cell(&mut img, &f, 100, 0, b'X');
        paint_cell(&mut img, &f, 0, 37, b'X');
        paint_cell(&mut img, &f, usize::MAX, 0, b'X');
        paint_cell(&mut img, &f, 0, usize::MAX, b'X');
        paint_cell(&mut img, &f, usize::MAX, usize::MAX, b'X');
        assert_eq!(img.stores, 0);
        paint_cell(&mut img, &f, 99, 36, b'X');
        assert_eq!(img.stores, 128);
        assert_eq!(img.hits_at(799, 591), 1, "the last cell's last pixel");
        assert_eq!(img.hits_at(799, 592), 0, "the margin row below it");
    }

    #[test]
    fn the_clamp_keeps_cells_past_the_maximum_unpainted() {
        let f = fmt(1920, 1080);
        let mut img = Image::new(&f);
        paint_cell(&mut img, &f, MAX_COLS, 0, b'X');
        paint_cell(&mut img, &f, 0, MAX_ROWS, b'X');
        assert_eq!(img.stores, 0, "on screen, but not in the grid");
        paint_cell(&mut img, &f, MAX_COLS - 1, MAX_ROWS - 1, b'X');
        assert_eq!(img.stores, 128);
    }

    #[test]
    fn an_unprintable_byte_paints_the_question_mark() {
        let f = fmt(16, 16);
        let mut want = Image::new(&f);
        paint_cell(&mut want, &f, 0, 0, b'?');
        for byte in [0x00, 0x1B, 0x7F, 0x80, 0xFF] {
            let mut got = Image::new(&f);
            paint_cell(&mut got, &f, 0, 0, byte);
            assert_eq!(got.pixels, want.pixels, "byte {byte:#04x}");
        }
    }

    // --- Review Focus 6: nothing outside w × h ---------------------------------

    #[test]
    fn nothing_is_stored_outside_a_13_by_21_surface() {
        let f = fmt(13, 21);
        assert_eq!((f.cols(), f.rows()), (1, 1));
        let mut img = Image::new(&f); // panics on any out-of-bounds store

        clear(&mut img, &f);
        assert_eq!(img.stores, 13 * 21);
        assert!(img.hits.iter().all(|&h| h == 1), "every pixel exactly once");
        assert!(img.pixels.iter().all(|&p| p == f.bg()));

        // A grid far bigger than the screen, full of ink.
        let mut grid = Grid::new(MAX_COLS, MAX_ROWS);
        for _ in 0..MAX_COLS * MAX_ROWS {
            grid.put(b'#');
        }
        paint_all(&mut img, &f, &grid);
        assert_eq!(img.stores, 13 * 21 + 128, "one cell fits");

        for col in 0..4 {
            for row in 0..4 {
                paint_cell(&mut img, &f, col, row, b'#');
            }
        }
        assert_eq!(
            img.stores,
            13 * 21 + 2 * 128,
            "only (0, 0) was painted again"
        );

        // The margins — 5 columns right, 5 rows below — are still background,
        // stored once, by `clear`.
        for y in 0..21 {
            for x in 0..13 {
                if x >= 8 || y >= 16 {
                    assert_eq!(img.at(x, y), f.bg(), "margin pixel ({x}, {y})");
                    assert_eq!(img.hits_at(x, y), 1, "margin pixel ({x}, {y})");
                }
            }
        }
        assert_eq!(decode_cell(|x, y| img.at(x, y), &f, 0, 0), Some(b'#'));
    }

    #[test]
    fn a_surface_smaller_than_a_cell_is_cleared_and_never_painted() {
        let f = fmt(7, 15);
        let mut img = Image::new(&f);
        let mut grid = Grid::new(MAX_COLS, MAX_ROWS);
        grid.put(b'#');
        clear(&mut img, &f);
        paint_all(&mut img, &f, &grid);
        paint_cell(&mut img, &f, 0, 0, b'#');
        assert_eq!(img.stores, 7 * 15);
        assert!(img.pixels.iter().all(|&p| p == f.bg()));
    }

    // --- pitch wider than width * 4 --------------------------------------------

    /// A surface addressed the way the bin's is: bytes, `y * pitch + x * 4`.
    struct Bytes {
        pitch: u32,
        bytes: Vec<u8>,
    }

    impl Surface for Bytes {
        fn store(&mut self, x: u32, y: u32, pixel: u32) {
            let at = (y * self.pitch + x * 4) as usize;
            self.bytes[at..at + 4].copy_from_slice(&pixel.to_le_bytes());
        }
    }

    #[test]
    fn a_wide_pitch_leaves_the_padding_alone() {
        // 20 visible pixels (80 bytes) in a 96-byte row: 16 bytes of padding.
        let f = Format::new(20, 35, 96, 16, 8, 0, 32).unwrap();
        assert_eq!((f.cols(), f.rows()), (2, 2));
        assert_eq!(f.byte_len(), 96 * 35);
        const PAD: u8 = 0x5A;
        let mut surface = Bytes {
            pitch: f.pitch(),
            bytes: vec![PAD; f.byte_len() as usize],
        };
        let mut grid = Grid::new(f.cols(), f.rows());
        for &b in b"abcd" {
            grid.put(b);
        }
        clear(&mut surface, &f);
        paint_all(&mut surface, &f, &grid);

        for (y, row) in surface.bytes.chunks(96).enumerate() {
            assert!(
                row[80..].iter().all(|&b| b == PAD),
                "row {y}: the padding past width * 4 was written"
            );
            assert!(
                row[..80].iter().all(|&b| b != PAD),
                "row {y}: a visible pixel was missed"
            );
        }
        let read = |x: u32, y: u32| {
            let at = (y * 96 + x * 4) as usize;
            u32::from_le_bytes(surface.bytes[at..at + 4].try_into().unwrap())
        };
        assert_eq!(decode_cell(read, &f, 0, 0), Some(b'a'));
        assert_eq!(decode_cell(read, &f, 1, 0), Some(b'b'));
        assert_eq!(decode_cell(read, &f, 0, 1), Some(b'c'));
        assert_eq!(decode_cell(read, &f, 1, 1), Some(b'd'));
    }

    // --- clear and paint_all -----------------------------------------------------

    #[test]
    fn clear_stores_the_background_in_every_pixel_once() {
        let f = Format::new(800, 600, 3200, 24, 16, 8, 32).unwrap();
        let mut img = Image::new(&f);
        clear(&mut img, &f);
        assert_eq!(img.stores, 800 * 600);
        assert!(img.hits.iter().all(|&h| h == 1));
        assert!(img.pixels.iter().all(|&p| p == f.bg()));
    }

    #[test]
    fn paint_all_paints_each_grid_cell_with_its_byte() {
        let f = fmt(40, 48);
        let mut grid = Grid::new(f.cols(), f.rows());
        for &b in b"Hello\nwor\tld\n~!" {
            grid.put(b);
        }
        let mut all = Image::new(&f);
        paint_all(&mut all, &f, &grid);
        assert_eq!(all.stores, 5 * 3 * 128);
        assert!(all.hits.iter().all(|&h| h == 1), "each pixel exactly once");

        let mut by_cell = Image::new(&f);
        for row in 0..3 {
            for col in 0..5 {
                paint_cell(&mut by_cell, &f, col, row, grid.cell(col, row));
            }
        }
        assert_eq!(all.pixels, by_cell.pixels);
    }

    #[test]
    fn paint_all_paints_only_the_grids_cells() {
        // A grid smaller than the screen's: the rest is `clear`'s business.
        let f = fmt(80, 80);
        let mut grid = Grid::new(2, 1);
        grid.put(b'a');
        grid.put(b'b');
        let mut img = Image::new(&f);
        paint_all(&mut img, &f, &grid);
        assert_eq!(img.stores, 2 * 128);
        assert_eq!(img.hits_at(15, 15), 1);
        assert_eq!(img.hits_at(16, 0), 0);
        assert_eq!(img.hits_at(0, 16), 0);
    }

    #[test]
    fn paint_all_of_an_empty_grid_stores_nothing() {
        let f = fmt(80, 80);
        let mut img = Image::new(&f);
        paint_all(&mut img, &f, &Grid::new(0, 0));
        assert_eq!(img.stores, 0);
    }

    // --- decode_cell (F10) -------------------------------------------------------

    #[test]
    fn every_printable_byte_decodes_to_itself() {
        let f = Format::new(24, 32, 96, 0, 8, 16, 32).unwrap();
        for byte in 0x20..=0x7Eu8 {
            let mut img = Image::new(&f);
            clear(&mut img, &f);
            paint_cell(&mut img, &f, 2, 1, byte);
            assert_eq!(
                decode_cell(|x, y| img.at(x, y), &f, 2, 1),
                Some(byte),
                "byte {byte:#04x}"
            );
            assert_eq!(decode_cell(|x, y| img.at(x, y), &f, 0, 0), Some(b' '));
        }
    }

    #[test]
    fn one_flipped_pixel_does_not_decode() {
        let f = fmt(8, 16);
        let mut img = Image::new(&f);
        paint_cell(&mut img, &f, 0, 0, b'A');
        // Each pixel in turn, flipped to the other console colour. No glyph in
        // the font is one pixel away from `A`.
        for i in 0..img.pixels.len() {
            let was = img.pixels[i];
            img.pixels[i] = if was == f.fg() { f.bg() } else { f.fg() };
            assert_eq!(
                decode_cell(|x, y| img.at(x, y), &f, 0, 0),
                None,
                "pixel {i}"
            );
            img.pixels[i] = was;
        }
        assert_eq!(decode_cell(|x, y| img.at(x, y), &f, 0, 0), Some(b'A'));
    }

    #[test]
    fn a_pixel_of_a_third_colour_does_not_decode() {
        let f = fmt(8, 16);
        let mut img = Image::new(&f);
        paint_cell(&mut img, &f, 0, 0, b'A');
        for (i, wrong) in [(0, 0x0000_0001), (17, 0x00AA_AAAB), (127, 0xFFAA_AAAA)] {
            let was = img.pixels[i];
            img.pixels[i] = wrong;
            assert_eq!(
                decode_cell(|x, y| img.at(x, y), &f, 0, 0),
                None,
                "pixel {i}"
            );
            img.pixels[i] = was;
        }
    }

    #[test]
    fn a_cell_shifted_by_a_pixel_does_not_decode() {
        // Mutation 2's shape: a renderer that stores at x + 1.
        let f = fmt(24, 16);
        let mut img = Image::new(&f);
        clear(&mut img, &f);
        paint_cell(&mut img, &f, 1, 0, b'M');
        let shifted = |x: u32, y: u32| if x == 0 { f.bg() } else { img.at(x - 1, y) };
        assert_eq!(decode_cell(|x, y| img.at(x, y), &f, 1, 0), Some(b'M'));
        assert_eq!(decode_cell(shifted, &f, 1, 0), None);
    }

    #[test]
    fn decoding_a_cell_outside_the_grid_is_none_and_reads_nothing() {
        let f = fmt(13, 21);
        let never = |_: u32, _: u32| -> u32 { panic!("read outside the grid") };
        assert_eq!(decode_cell(never, &f, 1, 0), None);
        assert_eq!(decode_cell(never, &f, 0, 1), None);
        assert_eq!(decode_cell(never, &f, usize::MAX, usize::MAX), None);
    }

    #[test]
    fn decoding_reads_only_the_cells_128_pixels() {
        let f = fmt(13, 21);
        let mut img = Image::new(&f);
        clear(&mut img, &f);
        let read = |x: u32, y: u32| {
            assert!(x < 8 && y < 16, "read at ({x}, {y}) outside the cell");
            img.at(x, y)
        };
        assert_eq!(decode_cell(read, &f, 0, 0), Some(b' '));
    }

    // --- the round trip ----------------------------------------------------------

    #[test]
    fn a_painted_grid_decodes_back_to_its_text() {
        let f = measured();
        let mut grid = Grid::new(f.cols(), f.rows());
        let mut img = Image::new(&f);
        clear(&mut img, &f);

        // Enough to wrap, tab, backspace and scroll, with every printable byte
        // and some that are not.
        let mut text = Vec::new();
        for i in 0..60u32 {
            text.extend_from_slice(format!("line {i:02}\t|").as_bytes());
            text.extend(0x20..=0x7Eu8);
            text.extend_from_slice(b"\x1b[0m\x08!\r\n");
            if i % 7 == 0 {
                text.extend_from_slice(&[b'='; 100]);
                text.push(b'\n');
            }
        }
        // Paint the way the bin will: by damage.
        for &byte in &text {
            match grid.put(byte) {
                Damage::None => {}
                Damage::Cell { col, row } => {
                    paint_cell(&mut img, &f, col, row, grid.cell(col, row));
                }
                Damage::All => paint_all(&mut img, &f, &grid),
            }
        }

        let mut non_blank = 0;
        for row in 0..f.rows() {
            for col in 0..f.cols() {
                let got = decode_cell(|x, y| img.at(x, y), &f, col, row);
                assert_eq!(got, Some(grid.cell(col, row)), "cell ({col}, {row})");
                non_blank += usize::from(got != Some(b' '));
            }
        }
        assert!(
            non_blank > 1000,
            "the screen has text on it: {non_blank} cells"
        );

        // Painting by damage and painting from scratch give the same picture.
        let mut fresh = Image::new(&f);
        clear(&mut fresh, &f);
        paint_all(&mut fresh, &f, &grid);
        assert_eq!(img.pixels, fresh.pixels);

        // The 8-pixel margin under row 36 is background.
        for y in 592..600 {
            for x in 0..800 {
                assert_eq!(img.at(x, y), f.bg());
            }
        }
    }
}
