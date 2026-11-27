// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The text model: what is on the screen, as bytes (slice 6.7, decision F8).
//!
//! A [`Grid`] is a fixed [`MAX_COLS`] × [`MAX_ROWS`] array of bytes, a cursor,
//! and one operation, [`Grid::put`], which is the whole of F8's table:
//!
//! | byte          | effect                                                        |
//! | ------------- | ------------------------------------------------------------- |
//! | `0x20..=0x7E` | draw at the cursor, advance; at the last column, wrap first   |
//! | `\n`          | column 0 of the next row; past the last row, scroll one row   |
//! | `\r`          | column 0                                                      |
//! | `\t`          | advance to the next multiple of 8, clamped to the last column |
//! | `\b`          | back one column if not at column 0; erases nothing            |
//! | other         | `?` — including `ESC`: there is no escape-sequence terminal   |
//!
//! ## Deferred wrap
//!
//! Writing in the last column does **not** move the cursor to the next row. It
//! leaves the cursor where it is with a wrap *pending*, and the wrap happens
//! when the next drawn byte arrives. So a line of exactly `cols` bytes followed
//! by `\n` takes one row, not two — the `\n` consumes the pending wrap instead
//! of adding to it. Every byte that moves the cursor (`\n`, `\r`, `\b`) cancels
//! a pending wrap; `\t`, which at the last column has nowhere to go, leaves it.
//!
//! ## Damage
//!
//! `put` returns what changed, so the caller paints one cell for one byte and
//! looks at the whole grid only on a scroll. There is no "shift the pixels
//! up": the renderer never reads the framebuffer (H9), so a scroll shifts
//! *this* array and the screen is brought up to date from it — by
//! [`crate::paint::paint_diff`], which stores only the cells whose byte is not
//! the one already on the screen.
//!
//! The grid stores bytes as they will be drawn: an unprintable byte is stored
//! as `?`, so [`Grid::cell`] never returns anything outside `0x20..=0x7E`.

use crate::font::{FIRST_PRINTABLE, LAST_PRINTABLE, REPLACEMENT};

/// The widest grid kept. A wider screen is used up to this many columns from
/// the left; the rest stays background.
pub const MAX_COLS: usize = 160;
/// The tallest grid kept. A taller screen is used up to this many rows from the
/// top.
pub const MAX_ROWS: usize = 64;
/// What an empty cell holds.
pub const BLANK: u8 = 0x20;
/// `\t` advances to the next multiple of this.
pub const TAB_STOP: usize = 8;
/// `\b`, which Rust has no escape for.
const BACKSPACE: u8 = 0x08;

/// What one [`Grid::put`] changed on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Damage {
    /// Nothing visible: the cursor moved, or the grid has no cells.
    None,
    /// Exactly one cell now holds a different byte.
    Cell { col: usize, row: usize },
    /// The grid scrolled; any cell may have changed, and most have not.
    All,
}

/// The screen's text: `cols × rows` bytes and a cursor.
///
/// About 10 KiB. TTY keeps it in `main`'s frame, beside the equally large
/// `paint::Shown`, so mind the 64 KiB driver stack; `new` is `const` should it
/// ever have to move to a `static`.
#[derive(Debug, Clone)]
pub struct Grid {
    cells: [[u8; MAX_COLS]; MAX_ROWS],
    cols: usize,
    rows: usize,
    /// Cursor column, always `< cols` (or 0 in an empty grid).
    col: usize,
    /// Cursor row, always `< rows` (or 0 in an empty grid).
    row: usize,
    /// The cursor's cell was just written and it is the last column: the next
    /// drawn byte wraps first.
    pending: bool,
}

impl Grid {
    /// A blank grid of `cols × rows`, each clamped to [`MAX_COLS`] /
    /// [`MAX_ROWS`], cursor at the top-left.
    pub const fn new(cols: usize, rows: usize) -> Self {
        Self {
            cells: [[BLANK; MAX_COLS]; MAX_ROWS],
            cols: if cols > MAX_COLS { MAX_COLS } else { cols },
            rows: if rows > MAX_ROWS { MAX_ROWS } else { rows },
            col: 0,
            row: 0,
            pending: false,
        }
    }

    /// Columns in use.
    pub const fn cols(&self) -> usize {
        self.cols
    }

    /// Rows in use.
    pub const fn rows(&self) -> usize {
        self.rows
    }

    /// The cursor as `(col, row)`: where the next drawn byte goes unless a wrap
    /// is pending.
    pub const fn cursor(&self) -> (usize, usize) {
        (self.col, self.row)
    }

    /// Whether the last column was just written, so the next drawn byte wraps
    /// before it draws.
    pub const fn wrap_pending(&self) -> bool {
        self.pending
    }

    /// The byte at `(col, row)`; [`BLANK`] outside the grid.
    pub fn cell(&self, col: usize, row: usize) -> u8 {
        self.row(row).get(col).copied().unwrap_or(BLANK)
    }

    /// Row `r`'s `cols` bytes; empty outside the grid.
    pub fn row(&self, r: usize) -> &[u8] {
        if r >= self.rows {
            return &[];
        }
        self.cells
            .get(r)
            .and_then(|row| row.get(..self.cols))
            .unwrap_or(&[])
    }

    /// Apply one byte (F8) and say what it changed.
    pub fn put(&mut self, byte: u8) -> Damage {
        if self.cols == 0 || self.rows == 0 {
            return Damage::None;
        }
        // Non-zero, so neither subtraction can wrap.
        let last_col = self.cols - 1;
        match byte {
            b'\n' => {
                self.pending = false;
                self.col = 0;
                if self.line_feed() {
                    Damage::All
                } else {
                    Damage::None
                }
            }
            b'\r' => {
                self.pending = false;
                self.col = 0;
                Damage::None
            }
            b'\t' => {
                // `col < cols <= MAX_COLS`, so the next stop is at most
                // `MAX_COLS + TAB_STOP`: nothing to check. A pending wrap is
                // left alone — the cursor is already at the last column.
                let stop = (self.col / TAB_STOP + 1) * TAB_STOP;
                self.col = if stop > last_col { last_col } else { stop };
                Damage::None
            }
            BACKSPACE => {
                self.pending = false;
                self.col = self.col.saturating_sub(1);
                Damage::None
            }
            _ => {
                let drawn = if (FIRST_PRINTABLE..=LAST_PRINTABLE).contains(&byte) {
                    byte
                } else {
                    REPLACEMENT
                };
                let mut scrolled = false;
                if self.pending {
                    self.pending = false;
                    self.col = 0;
                    scrolled = self.line_feed();
                }
                let (col, row) = (self.col, self.row);
                if let Some(cell) = self.cells.get_mut(row).and_then(|r| r.get_mut(col)) {
                    *cell = drawn;
                }
                if col == last_col {
                    self.pending = true;
                } else {
                    self.col = col + 1;
                }
                if scrolled {
                    Damage::All
                } else {
                    Damage::Cell { col, row }
                }
            }
        }
    }

    /// Move down one row, scrolling if the cursor is on the last. Returns
    /// whether it scrolled. Only called on a grid with at least one row.
    fn line_feed(&mut self) -> bool {
        if self.row + 1 < self.rows {
            self.row += 1;
            return false;
        }
        // Shift the rows in use up by one and blank the last. Whole array rows
        // move, so the columns past `cols` — always blank — stay blank.
        let rows = self.rows;
        self.cells.copy_within(1..rows, 0);
        if let Some(last) = self.cells.get_mut(rows - 1) {
            *last = [BLANK; MAX_COLS];
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(g: &mut Grid, bytes: &[u8]) -> Vec<Damage> {
        bytes.iter().map(|&b| g.put(b)).collect()
    }

    /// Row `r` as text, trailing blanks trimmed.
    fn line(g: &Grid, r: usize) -> String {
        String::from_utf8(g.row(r).to_vec())
            .unwrap()
            .trim_end()
            .to_string()
    }

    fn lines(g: &Grid) -> Vec<String> {
        (0..g.rows()).map(|r| line(g, r)).collect()
    }

    // --- construction ---------------------------------------------------

    #[test]
    fn new_keeps_a_size_that_fits() {
        let g = Grid::new(100, 37);
        assert_eq!((g.cols(), g.rows()), (100, 37));
        assert_eq!(g.cursor(), (0, 0));
        assert!(!g.wrap_pending());
    }

    #[test]
    fn new_clamps_to_the_maximum() {
        let g = Grid::new(1000, 1000);
        assert_eq!((g.cols(), g.rows()), (MAX_COLS, MAX_ROWS));
        let g = Grid::new(usize::MAX, 3);
        assert_eq!((g.cols(), g.rows()), (MAX_COLS, 3));
        let g = Grid::new(3, usize::MAX);
        assert_eq!((g.cols(), g.rows()), (3, MAX_ROWS));
        let g = Grid::new(MAX_COLS, MAX_ROWS);
        assert_eq!((g.cols(), g.rows()), (160, 64));
    }

    #[test]
    fn a_new_grid_is_blank() {
        let g = Grid::new(MAX_COLS, MAX_ROWS);
        for r in 0..MAX_ROWS {
            assert_eq!(g.row(r).len(), MAX_COLS);
            assert!(g.row(r).iter().all(|&b| b == BLANK));
        }
    }

    #[test]
    fn cell_and_row_outside_the_grid_are_blank_and_empty() {
        let mut g = Grid::new(4, 2);
        feed(&mut g, b"abcd");
        assert_eq!(g.cell(3, 0), b'd');
        assert_eq!(g.cell(4, 0), BLANK);
        assert_eq!(g.cell(0, 2), BLANK);
        assert_eq!(g.cell(usize::MAX, usize::MAX), BLANK);
        assert_eq!(g.row(0), b"abcd");
        assert_eq!(g.row(2), b"");
        assert_eq!(g.row(usize::MAX), b"");
    }

    // --- F8, row 1: printable bytes ---------------------------------------

    #[test]
    fn a_printable_byte_draws_at_the_cursor_and_advances() {
        let mut g = Grid::new(10, 3);
        assert_eq!(g.put(b'h'), Damage::Cell { col: 0, row: 0 });
        assert_eq!(g.cursor(), (1, 0));
        assert_eq!(g.put(b'i'), Damage::Cell { col: 1, row: 0 });
        assert_eq!(g.cursor(), (2, 0));
        assert_eq!(g.cell(0, 0), b'h');
        assert_eq!(g.cell(1, 0), b'i');
        assert_eq!(lines(&g), ["hi", "", ""]);
    }

    #[test]
    fn every_printable_byte_is_stored_as_itself() {
        for byte in 0x20..=0x7Eu8 {
            let mut g = Grid::new(4, 2);
            g.put(b'x');
            assert_eq!(g.put(byte), Damage::Cell { col: 1, row: 0 });
            assert_eq!(g.cell(1, 0), byte);
            assert_eq!(g.cursor(), (2, 0));
        }
    }

    // --- F8, row 2: newline ------------------------------------------------

    #[test]
    fn newline_goes_to_column_zero_of_the_next_row() {
        let mut g = Grid::new(10, 3);
        let d = feed(&mut g, b"ab\ncd");
        assert_eq!(
            d[2],
            Damage::None,
            "a newline that does not scroll paints nothing"
        );
        assert_eq!(lines(&g), ["ab", "cd", ""]);
        assert_eq!(g.cursor(), (2, 1));
        assert_eq!(d[3], Damage::Cell { col: 0, row: 1 });
    }

    #[test]
    fn newline_past_the_last_row_scrolls_one_row() {
        let mut g = Grid::new(10, 3);
        let d = feed(&mut g, b"one\ntwo\nthree");
        assert!(!d.contains(&Damage::All));
        assert_eq!(g.cursor(), (5, 2));
        assert_eq!(g.put(b'\n'), Damage::All);
        assert_eq!(lines(&g), ["two", "three", ""]);
        assert_eq!(g.cursor(), (0, 2), "the cursor stays on the last row");
        feed(&mut g, b"four");
        assert_eq!(lines(&g), ["two", "three", "four"]);
    }

    #[test]
    fn scrolling_keeps_rows_in_order_and_blanks_the_last() {
        let mut g = Grid::new(8, 4);
        feed(&mut g, b"r0\nr1\nr2\nr3");
        assert_eq!(lines(&g), ["r0", "r1", "r2", "r3"]);
        assert_eq!(g.put(b'\n'), Damage::All);
        assert_eq!(lines(&g), ["r1", "r2", "r3", ""]);
        assert_eq!(g.put(b'\n'), Damage::All);
        assert_eq!(lines(&g), ["r2", "r3", "", ""]);
        assert!(g.row(3).iter().all(|&b| b == BLANK));
    }

    #[test]
    fn scrolling_many_times_never_leaves_the_grid() {
        let mut g = Grid::new(MAX_COLS, MAX_ROWS);
        for i in 0..1000u32 {
            feed(&mut g, format!("line {i}\n").as_bytes());
        }
        assert_eq!(g.cursor(), (0, MAX_ROWS - 1));
        assert_eq!(line(&g, MAX_ROWS - 2), "line 999");
        assert_eq!(line(&g, 0), format!("line {}", 1000 - (MAX_ROWS - 1)));
        assert_eq!(line(&g, MAX_ROWS - 1), "");
    }

    // --- F8, row 3: carriage return ----------------------------------------

    #[test]
    fn carriage_return_goes_to_column_zero_and_erases_nothing() {
        let mut g = Grid::new(10, 3);
        let d = feed(&mut g, b"abc\r");
        assert_eq!(d[3], Damage::None);
        assert_eq!(g.cursor(), (0, 0));
        assert_eq!(lines(&g), ["abc", "", ""]);
        g.put(b'X');
        assert_eq!(lines(&g), ["Xbc", "", ""]);
    }

    #[test]
    fn crlf_is_one_line_break() {
        let mut g = Grid::new(10, 3);
        feed(&mut g, b"ab\r\ncd\r\n");
        assert_eq!(lines(&g), ["ab", "cd", ""]);
        assert_eq!(g.cursor(), (0, 2));
    }

    // --- F8, row 4: tab ------------------------------------------------------

    #[test]
    fn tab_advances_to_the_next_multiple_of_eight() {
        let mut g = Grid::new(40, 3);
        assert_eq!(g.put(b'\t'), Damage::None);
        assert_eq!(g.cursor(), (8, 0), "from 0");
        g.put(b'\t');
        assert_eq!(g.cursor(), (16, 0), "from a stop, to the next one");
        feed(&mut g, b"abc");
        assert_eq!(g.cursor(), (19, 0));
        g.put(b'\t');
        assert_eq!(g.cursor(), (24, 0), "from between stops");
        feed(&mut g, b"abcdefg");
        assert_eq!(g.cursor(), (31, 0));
        g.put(b'\t');
        assert_eq!(g.cursor(), (32, 0), "from one short of a stop");
    }

    #[test]
    fn tab_is_clamped_to_the_last_column() {
        let mut g = Grid::new(20, 3);
        feed(&mut g, b"\t\ta");
        assert_eq!(g.cursor(), (17, 0));
        g.put(b'\t');
        assert_eq!(g.cursor(), (19, 0), "24 is past the last column");
        g.put(b'\t');
        assert_eq!(g.cursor(), (19, 0), "and it stays there");
        assert_eq!(g.cursor().1, 0, "a tab never changes row");

        let mut narrow = Grid::new(5, 2);
        narrow.put(b'\t');
        assert_eq!(narrow.cursor(), (4, 0));
    }

    #[test]
    fn tab_erases_nothing() {
        let mut g = Grid::new(20, 2);
        feed(&mut g, b"0123456789\r\t");
        assert_eq!(g.cursor(), (8, 0));
        assert_eq!(lines(&g), ["0123456789", ""]);
    }

    #[test]
    fn tab_to_the_last_column_does_not_wrap_by_itself() {
        // The tab clamps there without writing, so the next byte draws *in* the
        // last column and only then is a wrap pending.
        let mut g = Grid::new(12, 2);
        feed(&mut g, b"\t\t");
        assert_eq!(g.cursor(), (11, 0));
        assert!(!g.wrap_pending());
        assert_eq!(g.put(b'Z'), Damage::Cell { col: 11, row: 0 });
        assert!(g.wrap_pending());
        assert_eq!(g.cursor(), (11, 0));
    }

    // --- F8, row 5: backspace ----------------------------------------------

    #[test]
    fn backspace_moves_back_one_column_and_erases_nothing() {
        let mut g = Grid::new(10, 2);
        let d = feed(&mut g, b"ab\x08");
        assert_eq!(d[2], Damage::None);
        assert_eq!(g.cursor(), (1, 0));
        assert_eq!(lines(&g), ["ab", ""], "the b is still there");
        g.put(b'X');
        assert_eq!(lines(&g), ["aX", ""]);
    }

    #[test]
    fn backspace_at_column_zero_does_nothing() {
        let mut g = Grid::new(10, 3);
        feed(&mut g, b"ab\n");
        assert_eq!(g.cursor(), (0, 1));
        assert_eq!(g.put(0x08), Damage::None);
        assert_eq!(g.cursor(), (0, 1), "not back onto the previous row");
        let mut fresh = Grid::new(10, 3);
        assert_eq!(fresh.put(0x08), Damage::None);
        assert_eq!(fresh.cursor(), (0, 0));
    }

    // --- F8, row 6: everything else ------------------------------------------

    #[test]
    fn every_other_byte_draws_a_question_mark() {
        for byte in 0..=u8::MAX {
            let handled = matches!(byte, 0x20..=0x7E | b'\n' | b'\r' | b'\t' | 0x08);
            if handled {
                continue;
            }
            let mut g = Grid::new(4, 2);
            assert_eq!(
                g.put(byte),
                Damage::Cell { col: 0, row: 0 },
                "byte {byte:#04x}"
            );
            assert_eq!(g.cell(0, 0), b'?', "byte {byte:#04x}");
            assert_eq!(g.cursor(), (1, 0), "byte {byte:#04x}");
        }
    }

    #[test]
    fn an_escape_sequence_is_drawn_not_obeyed() {
        let mut g = Grid::new(10, 2);
        feed(&mut g, b"\x1b[2Jx");
        assert_eq!(lines(&g), ["?[2Jx", ""]);
    }

    // --- deferred wrap -------------------------------------------------------

    #[test]
    fn writing_the_last_column_leaves_the_wrap_pending() {
        let mut g = Grid::new(4, 3);
        let d = feed(&mut g, b"abcd");
        assert_eq!(d[3], Damage::Cell { col: 3, row: 0 });
        assert_eq!(g.cursor(), (3, 0), "the cursor has not left the row");
        assert!(g.wrap_pending());
        assert_eq!(lines(&g), ["abcd", "", ""]);
    }

    #[test]
    fn a_full_line_then_newline_takes_one_row() {
        let mut g = Grid::new(4, 3);
        feed(&mut g, b"abcd\n");
        assert_eq!(g.cursor(), (0, 1), "one row down, not two");
        assert!(!g.wrap_pending());
        feed(&mut g, b"e");
        assert_eq!(lines(&g), ["abcd", "e", ""]);
    }

    #[test]
    fn a_full_line_at_the_maximum_width_then_newline_takes_one_row() {
        let mut g = Grid::new(MAX_COLS, MAX_ROWS);
        feed(&mut g, &[b'#'; MAX_COLS]);
        assert_eq!(g.cursor(), (MAX_COLS - 1, 0));
        g.put(b'\n');
        assert_eq!(g.cursor(), (0, 1));
        assert_eq!(g.row(0), [b'#'; MAX_COLS]);
        assert!(g.row(1).iter().all(|&b| b == BLANK));
    }

    #[test]
    fn the_next_printable_byte_wraps_first() {
        let mut g = Grid::new(4, 3);
        let d = feed(&mut g, b"abcde");
        assert_eq!(d[4], Damage::Cell { col: 0, row: 1 });
        assert_eq!(lines(&g), ["abcd", "e", ""]);
        assert_eq!(g.cursor(), (1, 1));
        assert!(!g.wrap_pending());
    }

    #[test]
    fn an_unprintable_byte_wraps_like_a_printable_one() {
        let mut g = Grid::new(4, 3);
        feed(&mut g, b"abcd\x00");
        assert_eq!(lines(&g), ["abcd", "?", ""]);
    }

    #[test]
    fn two_full_lines_fill_two_rows() {
        let mut g = Grid::new(4, 3);
        feed(&mut g, b"abcdefgh");
        assert_eq!(lines(&g), ["abcd", "efgh", ""]);
        assert_eq!(g.cursor(), (3, 1));
        assert!(g.wrap_pending());
    }

    #[test]
    fn carriage_return_cancels_a_pending_wrap() {
        let mut g = Grid::new(4, 3);
        feed(&mut g, b"abcd\rX");
        assert_eq!(lines(&g), ["Xbcd", "", ""]);
        assert_eq!(g.cursor(), (1, 0));
    }

    #[test]
    fn backspace_cancels_a_pending_wrap_and_moves_back() {
        let mut g = Grid::new(4, 3);
        feed(&mut g, b"abcd\x08");
        assert!(!g.wrap_pending());
        assert_eq!(g.cursor(), (2, 0));
        g.put(b'X');
        assert_eq!(lines(&g), ["abXd", "", ""]);
    }

    #[test]
    fn tab_keeps_a_pending_wrap() {
        let mut g = Grid::new(4, 3);
        feed(&mut g, b"abcd\t");
        assert!(g.wrap_pending());
        assert_eq!(g.cursor(), (3, 0));
        g.put(b'e');
        assert_eq!(lines(&g), ["abcd", "e", ""], "the d was not overwritten");
    }

    // --- wrap and scroll together --------------------------------------------

    #[test]
    fn a_wrap_on_the_last_row_scrolls() {
        let mut g = Grid::new(3, 2);
        let d = feed(&mut g, b"abcdef");
        assert!(
            !d.contains(&Damage::All),
            "filling the last row does not scroll"
        );
        assert_eq!(lines(&g), ["abc", "def"]);
        assert_eq!(g.cursor(), (2, 1));
        assert_eq!(g.put(b'g'), Damage::All);
        assert_eq!(lines(&g), ["def", "g"]);
        assert_eq!(g.cursor(), (1, 1));
        assert_eq!(g.put(b'h'), Damage::Cell { col: 1, row: 1 });
    }

    #[test]
    fn a_full_last_row_then_newline_scrolls_exactly_once() {
        let mut g = Grid::new(3, 2);
        feed(&mut g, b"abcdef");
        assert_eq!(g.put(b'\n'), Damage::All);
        assert_eq!(lines(&g), ["def", ""], "one scroll, not two");
        assert_eq!(g.cursor(), (0, 1));
    }

    #[test]
    fn a_scroll_does_not_touch_columns_or_rows_outside_the_grid() {
        // A grid smaller than the array: what scrolls is `cols × rows`.
        let mut g = Grid::new(3, 2);
        for _ in 0..10 {
            feed(&mut g, b"abc\n");
        }
        assert_eq!(lines(&g), ["abc", ""]);
        assert_eq!(g.row(0).len(), 3);
        assert_eq!(g.cell(3, 0), BLANK);
        assert_eq!(g.cell(0, 2), BLANK);
    }

    // --- degenerate sizes ------------------------------------------------------

    #[test]
    fn a_zero_sized_grid_accepts_bytes_and_does_nothing() {
        for (cols, rows) in [(0, 0), (0, 5), (5, 0)] {
            let mut g = Grid::new(cols, rows);
            for byte in 0..=u8::MAX {
                assert_eq!(g.put(byte), Damage::None, "{cols}x{rows} byte {byte:#04x}");
            }
            for _ in 0..1000 {
                assert_eq!(g.put(b'x'), Damage::None);
                assert_eq!(g.put(b'\n'), Damage::None);
            }
            assert_eq!(g.cursor(), (0, 0));
            assert!(!g.wrap_pending());
            assert_eq!(g.cell(0, 0), BLANK);
            assert_eq!(g.row(0).len(), if rows == 0 { 0 } else { cols });
        }
    }

    #[test]
    fn a_one_cell_grid_scrolls_on_every_second_byte() {
        let mut g = Grid::new(1, 1);
        assert_eq!(g.put(b'a'), Damage::Cell { col: 0, row: 0 });
        assert!(g.wrap_pending());
        assert_eq!(g.put(b'b'), Damage::All);
        assert_eq!(g.cell(0, 0), b'b');
        assert_eq!(g.put(b'\t'), Damage::None);
        assert_eq!(g.put(0x08), Damage::None);
        assert_eq!(g.cursor(), (0, 0));
        assert_eq!(g.put(b'\n'), Damage::All);
        assert_eq!(g.cell(0, 0), BLANK);
    }

    #[test]
    fn a_one_column_grid_puts_each_byte_on_its_own_row() {
        let mut g = Grid::new(1, 3);
        feed(&mut g, b"abc");
        assert_eq!(lines(&g), ["a", "b", "c"]);
        assert_eq!(g.put(b'd'), Damage::All);
        assert_eq!(lines(&g), ["b", "c", "d"]);
    }

    /// The grid never holds a byte the font would have to replace: what `cell`
    /// returns is what is drawn.
    #[test]
    fn cells_only_ever_hold_printable_bytes() {
        let mut g = Grid::new(7, 5);
        for i in 0..5000u32 {
            g.put((i.wrapping_mul(2_654_435_761) >> 13) as u8);
            let (col, row) = g.cursor();
            assert!(col < 7 && row < 5, "cursor left the grid at byte {i}");
        }
        for r in 0..5 {
            assert!(g.row(r).iter().all(|b| (0x20..=0x7E).contains(b)));
        }
    }
}
