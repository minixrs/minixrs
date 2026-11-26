// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The screen check (slice 6.7, decision F10): is this dump a console TTY
//! painted, and is its text the text this boot wrote to its serial line?
//!
//! [`check`] takes a dump and the boot's serial log, both as bytes, and
//! applies three steps in order. The first that fails is the answer.
//!
//! ## 1. Geometry
//!
//! TTY prints `[diag tty] fb ok w=<w> h=<h> cols=<c> rows=<r>` once when it
//! finds its framebuffer. The **last** line of the log holding it is the one
//! used: a log holds one per TTY start, and the last TTY to start is the one
//! whose screen can be dumped. The line must be complete — all four numbers,
//! nothing after them, and a line end (`\n` or `\r`) — or the check fails
//! rather than read half a number from a line still being written. It is
//! looked for in the log as written, before step 3 takes the kernel's lines
//! out (it is one of them), and may itself hold a tick byte or two.
//!
//! The dump must be exactly `w × h`, and the grid [`Format::new`] derives from
//! that size must be exactly `c × r`. So TTY's own arithmetic is checked
//! against the library's, on the host.
//!
//! ## 2. Every cell is a glyph
//!
//! A PPM is plain red, green, blue, so the pixel layout used here is this
//! module's own choice ([`RED_SHIFT`], [`GREEN_SHIFT`], [`BLUE_SHIFT`]) and
//! says nothing about the guest's. Each cell of the grid must decode through
//! [`decode_cell`] — all 128 pixels one of the two console colours, forming
//! one printable glyph — and every pixel outside the grid, the margins right
//! of the last column and below the last row, must be background. The decoded
//! text is then put into a fresh [`Grid`], painted with [`paint_all`] onto a
//! cleared canvas, and the canvas must equal the dump pixel for pixel.
//!
//! That last comparison cannot fail while `decode_cell` is the exact inverse
//! of `paint_cell`; it is there so that the day they stop agreeing, this
//! check says so instead of passing on the decoder's word alone.
//!
//! ## 3. The text is the boot's
//!
//! At least [`MIN_ROWS`] rows must be non-blank, and **every non-blank row,
//! with its trailing blanks removed, must be found in the console's part of
//! the serial log.** The log is bytes, as `grep -a` reads it: it need not be
//! UTF-8.
//!
//! ### What the serial log really is
//!
//! Measured on a fed disk boot (slice 6.7; 54 MB of log, of which 80 KB is
//! console): the serial line has three writers, and only one is the console.
//!
//! * **TTY**, writing the console's bytes; its lines end `\r\n`.
//! * **The kernel, a line at a time.** `[ipc 1414500] caller=17 …\r\n`,
//!   `[ksys SYS_FORK] …`, `[diag vfs] …`, `[pf …`, `[as …`, `[devmap …` and
//!   the rest. Each is written whole — nothing lands inside one — but it can
//!   begin **in the middle of a console line**, which carries on after it:
//!   `…-----[ksys 1412800] caller=17 call=0 result=0\r\n-----…`. A million of
//!   them in that boot.
//! * **The kernel, a byte at a time.** One byte per 10 ms timer tick while a
//!   process is running: the first byte of its name (`kernel/src/clock.rs`).
//!   They come singly (`minix.rs initt: dev.null ok`) and, when nothing else
//!   prints between ticks, in **runs** (`minix.rs hello: iov CCCCCCCCCC....`),
//!   anywhere, including inside a console line.
//!
//! So the log is read in two steps.
//!
//! **First the kernel's lines are taken out** ([`console_stream`]). A kernel
//! line starts at a `[` followed by one or more of `a`–`z`, `_`, `.`, `-` and
//! then a space or a `]` — `[ipc `, `[ksys `, `[diag tty]`, `[efault ` — and
//! runs through the next `\n`, or to the end of the log if it has none yet.
//! Everything else is kept, in order. Console text that itself contained such
//! a prefix would be taken out with the rest of its line, and its row then
//! not found: a false failure, never a false pass. Nothing the boot prints to
//! the console does.
//!
//! **Then each row is looked for in what is left**, allowing tick bytes
//! inside it. A row's text is found if the stream holds its bytes, in order,
//! with nothing between them but **stray bytes**, where:
//!
//! * a stray byte is an ASCII letter or `?` (what a tick prints: a name's
//!   first byte, or `?` for a process with none) — never a digit, a space,
//!   punctuation, or a `\r` or `\n`, so text on either side of a line end is
//!   never one row;
//! * strays may come in runs of any length, in any number of places, but a
//!   row's strays total at most [`MAX_STRAY`];
//! * nothing else is forgiven: no byte of the row may be missing from the
//!   log or differ in it.
//!
//! An exact occurrence is looked for first and counts as no strays.
//! Otherwise every position where the row's first byte occurs is tried, and
//! from each one every placement of the strays is followed at once, so a
//! match cannot be missed by committing early; the row's count is the fewest
//! strays any occurrence needs. [`Report::stray`] is the number of rows that
//! needed any, and [`Report::worst`] the most one row needed — the figure to
//! watch against [`MAX_STRAY`].
//!
//! [`MAX_STRAY`] is 32, from that boot: the two rows that needed strays
//! needed 5 and 10, and the longest run inside any console line of the whole
//! log was 30. Runs *before* a line are far longer (4930 there) and do not
//! count: they are not between two bytes of a row.
//!
//! **What the tolerance costs.** A screen row that has *lost* letters still
//! passes: `minix.rs it: stdin ok` is found in `minix.rs init: stdin ok`,
//! with `ni` taken for tick bytes. Up to [`MAX_STRAY`] letters per row can
//! go missing that way. A wrong, extra, or moved character cannot, nor a
//! missing digit, space or punctuation mark.
//!
//! ### Wrapped lines
//!
//! A line longer than the screen is wide takes several rows, each but the
//! last filled to the last column. Its rows need no special treatment to be
//! found: each is a piece of one line of the log, and a piece of something
//! that is in the log is in the log. The same goes for a top row that is the
//! tail of a line whose head has scrolled away.
//!
//! The spec's first text worded this step as "a row exactly `cols` wide is joined with the
//! row below it before the search". Requiring the join would refuse an honest
//! screen: a line of exactly `cols` bytes followed by a newline fills its row
//! and is *not* continued below (F8's deferred wrap), so joined to the next
//! row it is text nobody wrote. So the join is accepted when it is found and
//! the row alone is accepted when it is not — and since the joined text can
//! only be found (strays or no strays) where both rows are found, that rule
//! accepts exactly the screens the row-by-row rule above accepts. The
//! row-by-row rule is what is implemented, for chains of full-width rows as
//! for one.
//!
//! The join is still *looked for*, and counted: [`Report::wraps`] is the
//! number of rows that end in the last column and whose text, followed
//! directly by the next row's, is in the log. It refuses nothing. It is there
//! so a reader of the verdict can see that long lines were on the screen and
//! came out in order.
//!
//! ### What step 3 does not see
//!
//! * **Order.** Rows are looked up one at a time; nothing requires them to be
//!   in the log in screen order.
//! * **Short rows prove little.** A row holding `$` is in any log.
//! * **Lost letters.** As above: up to [`MAX_STRAY`] letters the log has and
//!   the row does not are forgiven, whoever wrote them.
//! * **Bytes the screen draws differently.** A tab is drawn as a move of the
//!   cursor and an unprintable byte as `?` (F8), so a row made from either is
//!   not in the log as drawn and is refused. Nothing in the default boot
//!   writes one to the console.

use std::fmt;

use minixrs_tty::font::{CELL_H, CELL_W};
use minixrs_tty::grid::{BLANK, Grid};
use minixrs_tty::paint::{
    BITS_PER_PIXEL, BYTES_PER_PIXEL, Format, FormatError, Surface, clear, decode_cell, paint_all,
};

use crate::ppm::{self, Ppm};

/// The fewest non-blank rows a passing screen has (F10, step 3).
pub const MIN_ROWS: usize = 8;

/// The most stray bytes — the kernel's tick bytes — tolerated inside one
/// row's text in the log. Chosen from a measured boot; see the module docs.
pub const MAX_STRAY: usize = 32;

// The matcher keeps one bit per stray count in a `u64`.
const _: () = assert!(MAX_STRAY < 64);

/// The most stray bytes tolerated among the numbers of the `fb ok` line. It
/// is short and printed once, so a run of ticks inside it is not expected;
/// one or two are.
const GEOMETRY_STRAY: usize = 2;

/// Where red sits in the pixels this module packs a dump into. A PPM has no
/// shifts of its own; these three are the checker's choice.
pub const RED_SHIFT: u8 = 16;
/// Where green sits.
pub const GREEN_SHIFT: u8 = 8;
/// Where blue sits.
pub const BLUE_SHIFT: u8 = 0;

/// What TTY's verdict line starts with (`drivers/tty/src/main.rs`).
const FB_OK: &[u8] = b"[diag tty] fb ok";

/// A header number longer than this is not a screen size.
const MAX_DIGITS: usize = 9;

/// A pixel no [`Format`] of this module packs: its top byte is set.
const NO_PIXEL: u32 = u32::MAX;

/// What TTY said about its framebuffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub cols: usize,
    pub rows: usize,
}

/// A passing screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The geometry the log and the dump agree on.
    pub geometry: Geometry,
    /// Rows holding something other than blanks.
    pub non_blank_rows: usize,
    /// Rows ending in the last column whose text runs on into the next row's
    /// in the log: wrapped lines seen whole. Informational.
    pub wraps: usize,
    /// Rows found in the log only by allowing stray bytes inside them.
    /// Informational; a large number says the tolerance is doing real work.
    pub stray: usize,
    /// The most stray bytes any one row needed; at most [`MAX_STRAY`].
    pub worst: usize,
    /// Every row's text, top to bottom, trailing blanks removed.
    pub text: Vec<String>,
}

impl fmt::Display for Report {
    /// The one-line verdict:
    /// `ok 800x600 100x37 rows=<non-blank> wraps=<n> stray=<n> worst=<n>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let g = &self.geometry;
        write!(
            f,
            "ok {}x{} {}x{} rows={} wraps={} stray={} worst={}",
            g.width,
            g.height,
            g.cols,
            g.rows,
            self.non_blank_rows,
            self.wraps,
            self.stray,
            self.worst
        )
    }
}

/// Why a cell did not decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellFault {
    /// The pixel at `(x, y)` is neither console colour. `rgb` is `0xRRGGBB`.
    Colour { x: u32, y: u32, rgb: u32 },
    /// All 128 pixels are console colours, but the shape — one byte per pixel
    /// row, bit 7 leftmost — is no printable glyph.
    Shape([u8; CELL_H as usize]),
}

/// Why a dump was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckError {
    /// Step 1: the log has no `[diag tty] fb ok` line.
    NoGeometryLine,
    /// Step 1: the log's last such line is incomplete or malformed.
    BadGeometryLine(String),
    /// Step 1: the dump is not the size the log reports.
    SizeMismatch { dump: (u32, u32), log: (u32, u32) },
    /// Step 1: the size is not one the renderer accepts.
    Format(FormatError),
    /// Step 1: the renderer's grid for this size is not the one TTY reported.
    GridMismatch {
        derived: (usize, usize),
        log: (usize, usize),
    },
    /// Step 2: a cell of the grid is not a glyph.
    Cell {
        col: usize,
        row: usize,
        fault: CellFault,
    },
    /// Step 2: a pixel outside the grid is not background. `rgb` is
    /// `0xRRGGBB`.
    Margin { x: u32, y: u32, rgb: u32 },
    /// Step 2: the decoded text, painted again, differs from the dump at
    /// `(x, y)`.
    Rerender {
        x: u32,
        y: u32,
        dump: u32,
        painted: u32,
    },
    /// Step 3: fewer than [`MIN_ROWS`] rows hold text.
    TooFewRows { found: usize },
    /// Step 3: a row's text is nowhere in the serial log, stray bytes allowed
    /// for.
    RowNotInLog { row: usize, text: String },
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoGeometryLine => {
                write!(f, "the serial log has no `[diag tty] fb ok` line")
            }
            Self::BadGeometryLine(line) => write!(
                f,
                "the serial log's last `[diag tty] fb ok` line is incomplete or malformed: {line:?}"
            ),
            Self::SizeMismatch { dump, log } => write!(
                f,
                "the dump is {}x{} pixels but the log's `fb ok` line says {}x{}",
                dump.0, dump.1, log.0, log.1
            ),
            Self::Format(e) => write!(
                f,
                "the renderer refuses a screen of this size ({})",
                e.as_str()
            ),
            Self::GridMismatch { derived, log } => write!(
                f,
                "a screen this size holds {}x{} cells but the log's `fb ok` line says cols={} rows={}",
                derived.0, derived.1, log.0, log.1
            ),
            Self::Cell { col, row, fault } => {
                write!(f, "the cell at col={col} row={row} is not a glyph: ")?;
                match fault {
                    CellFault::Colour { x, y, rgb } => {
                        write!(f, "pixel ({x}, {y}) is #{rgb:06x}, neither console colour")
                    }
                    CellFault::Shape(bits) => {
                        write!(f, "its pixel rows")?;
                        for b in bits {
                            write!(f, " {b:02x}")?;
                        }
                        write!(f, " are no printable character (caught half-painted?)")
                    }
                }
            }
            Self::Margin { x, y, rgb } => write!(
                f,
                "pixel ({x}, {y}), outside the text grid, is #{rgb:06x}, not background"
            ),
            Self::Rerender {
                x,
                y,
                dump,
                painted,
            } => write!(
                f,
                "the decoded text painted again differs from the dump at pixel ({x}, {y}): \
                 dump #{dump:06x}, painted #{painted:06x}"
            ),
            Self::TooFewRows { found } => write!(
                f,
                "only {found} non-blank rows on the screen; at least {MIN_ROWS} are needed"
            ),
            Self::RowNotInLog { row, text } => {
                write!(
                    f,
                    "screen row {row} is not in the serial log's console text, even allowing {MAX_STRAY} tick bytes inside it: {text:?}"
                )
            }
        }
    }
}

/// A `width × height` image of packed pixels: the host's [`Surface`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

impl Canvas {
    /// A canvas of the format's size with every pixel `fill`.
    pub fn new(fmt: &Format, fill: u32) -> Self {
        let n = (fmt.width() as usize).saturating_mul(fmt.height() as usize);
        Self {
            width: fmt.width(),
            height: fmt.height(),
            pixels: vec![fill; n],
        }
    }

    /// A dump's pixels, packed through `fmt`.
    fn from_ppm(image: &Ppm, fmt: &Format) -> Self {
        Self {
            width: image.width(),
            height: image.height(),
            pixels: image.pixels().map(|(r, g, b)| fmt.pack(r, g, b)).collect(),
        }
    }

    fn index(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        (y as usize)
            .checked_mul(self.width as usize)?
            .checked_add(x as usize)
    }

    /// The pixel at `(x, y)`; `None` outside the canvas.
    pub fn get(&self, x: u32, y: u32) -> Option<u32> {
        self.pixels.get(self.index(x, y)?).copied()
    }

    /// The canvas as the PPM file QEMU would write for it.
    pub fn to_ppm(&self) -> Vec<u8> {
        let mut rgb = Vec::with_capacity(self.pixels.len().saturating_mul(3));
        for &pixel in &self.pixels {
            rgb.extend_from_slice(&channels(pixel));
        }
        ppm::encode(self.width, self.height, &rgb)
    }
}

impl Surface for Canvas {
    /// A store outside the canvas is dropped; the renderer makes none.
    fn store(&mut self, x: u32, y: u32, pixel: u32) {
        if let Some(slot) = self.index(x, y).and_then(|i| self.pixels.get_mut(i)) {
            *slot = pixel;
        }
    }
}

/// A packed pixel's `[red, green, blue]`.
fn channels(pixel: u32) -> [u8; 3] {
    // Truncation is the point: each channel is the byte at its shift.
    [RED_SHIFT, GREEN_SHIFT, BLUE_SHIFT].map(|shift| (pixel >> shift) as u8)
}

/// A packed pixel as `0xRRGGBB`, for a message.
fn rgb_of(pixel: u32) -> u32 {
    let [r, g, b] = channels(pixel);
    u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)
}

/// The format a `width × height` dump is read in: this module's shifts, no
/// padding between rows.
pub fn dump_format(width: u32, height: u32) -> Result<Format, FormatError> {
    let pitch = width
        .checked_mul(BYTES_PER_PIXEL)
        .ok_or(FormatError::Pitch)?;
    Format::new(
        width,
        height,
        pitch,
        RED_SHIFT,
        GREEN_SHIFT,
        BLUE_SHIFT,
        BITS_PER_PIXEL,
    )
}

/// What the screen shows for `grid`: a cleared canvas with every cell
/// painted — the renderer's own two calls.
pub fn render(fmt: &Format, grid: &Grid) -> Canvas {
    let mut canvas = Canvas::new(fmt, NO_PIXEL);
    clear(&mut canvas, fmt);
    paint_all(&mut canvas, fmt, grid);
    canvas
}

/// Apply F10 to a dump and the serial log of the boot it was taken from.
pub fn check(image: &Ppm, log: &[u8]) -> Result<Report, CheckError> {
    // Step 1.
    let geometry = geometry_from_log(log)?;
    let dump = (image.width(), image.height());
    if dump != (geometry.width, geometry.height) {
        return Err(CheckError::SizeMismatch {
            dump,
            log: (geometry.width, geometry.height),
        });
    }
    let fmt = dump_format(geometry.width, geometry.height).map_err(CheckError::Format)?;
    if (fmt.cols(), fmt.rows()) != (geometry.cols, geometry.rows) {
        return Err(CheckError::GridMismatch {
            derived: (fmt.cols(), fmt.rows()),
            log: (geometry.cols, geometry.rows),
        });
    }

    // Step 2.
    let canvas = Canvas::from_ppm(image, &fmt);
    let rows = decode(&canvas, &fmt)?;

    // Step 3.
    let found = rows_in_log(&rows, &console_stream(log))?;
    Ok(Report {
        geometry,
        non_blank_rows: found.non_blank,
        wraps: found.wraps,
        stray: found.stray,
        worst: found.worst,
        text: rows.iter().map(|row| lossy(trimmed(row))).collect(),
    })
}

/// Step 1's reading of the log: the last line holding `[diag tty] fb ok`.
///
/// Read from the log **as written** — this is one of the kernel's lines, and
/// [`console_stream`] would take it out. Lines end at a `\n` or a `\r`. A
/// tick byte can land in this line as in any other, so the marker is looked
/// for with step 3's tolerance and up to [`GEOMETRY_STRAY`] stray bytes may
/// sit among the numbers. An exact reading is preferred to a tolerant one.
pub fn geometry_from_log(log: &[u8]) -> Result<Geometry, CheckError> {
    let mut lines = log.split(|&b| b == b'\n' || b == b'\r');
    // What follows the last line end has none of its own yet.
    let unended = lines.next_back().unwrap_or(&[]);
    if find_stray(unended, FB_OK).is_some() {
        // Still being written: "rows=10" may be the start of "rows=100".
        return Err(CheckError::BadGeometryLine(lossy(unended)));
    }
    let (line, found) = lines
        .rev()
        .find_map(|line| Some((line, find_stray(line, FB_OK)?)))
        .ok_or(CheckError::NoGeometryLine)?;
    let rest = line.get(found.end..).unwrap_or(&[]);
    parse_geometry(rest).ok_or_else(|| CheckError::BadGeometryLine(lossy(line)))
}

/// ` w=<w> h=<h> cols=<c> rows=<r>` and nothing after it — exactly, or else
/// with one or two ([`GEOMETRY_STRAY`]) stray bytes taken out. Only a byte
/// that can be a stray is ever taken out: never a digit.
fn parse_geometry(rest: &[u8]) -> Option<Geometry> {
    if let Some(geometry) = parse_geometry_exact(rest) {
        return Some(geometry);
    }
    let strays: Vec<usize> = (0..rest.len())
        .filter(|&i| rest.get(i).copied().is_some_and(may_be_stray))
        .collect();
    let without = |skip: &[usize]| -> Vec<u8> {
        rest.iter()
            .enumerate()
            .filter(|(i, _)| !skip.contains(i))
            .map(|(_, &b)| b)
            .collect()
    };
    for (n, &i) in strays.iter().enumerate() {
        if let Some(geometry) = parse_geometry_exact(&without(&[i])) {
            return Some(geometry);
        }
        if GEOMETRY_STRAY >= 2 {
            for &j in strays.iter().skip(n + 1) {
                if let Some(geometry) = parse_geometry_exact(&without(&[i, j])) {
                    return Some(geometry);
                }
            }
        }
    }
    None
}

/// Take `label` and the decimal number after it off the front of `rest`.
fn number(rest: &mut &[u8], label: &[u8]) -> Option<u32> {
    let after = rest.strip_prefix(label)?;
    let digits = after.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || digits > MAX_DIGITS {
        return None;
    }
    let (number, tail) = after.split_at_checked(digits)?;
    *rest = tail;
    // At most nine digits: inside a `u32`.
    Some(
        number
            .iter()
            .fold(0u32, |n, b| n * 10 + u32::from(b - b'0')),
    )
}

fn parse_geometry_exact(mut rest: &[u8]) -> Option<Geometry> {
    let width = number(&mut rest, b" w=")?;
    let height = number(&mut rest, b" h=")?;
    let cols = number(&mut rest, b" cols=")?;
    let rows = number(&mut rest, b" rows=")?;
    if !rest.is_empty() {
        return None;
    }
    Some(Geometry {
        width,
        height,
        cols: usize::try_from(cols).ok()?,
        rows: usize::try_from(rows).ok()?,
    })
}

/// Step 2: the grid's text, row by row, or the first thing on the dump that
/// the renderer did not put there.
fn decode(canvas: &Canvas, fmt: &Format) -> Result<Vec<Vec<u8>>, CheckError> {
    let pixel = |x, y| canvas.get(x, y).unwrap_or(NO_PIXEL);
    let (fg, bg) = (fmt.fg(), fmt.bg());

    let mut rows = Vec::with_capacity(fmt.rows());
    for row in 0..fmt.rows() {
        let mut text = Vec::with_capacity(fmt.cols());
        for col in 0..fmt.cols() {
            match decode_cell(pixel, fmt, col, row) {
                Some(byte) => text.push(byte),
                None => {
                    return Err(CheckError::Cell {
                        col,
                        row,
                        fault: cell_fault(&pixel, col, row, fg, bg),
                    });
                }
            }
        }
        rows.push(text);
    }

    // The margins: everything right of the last column or below the last row.
    // `cols <= width / 8` and `rows <= height / 16`, so neither product can
    // leave a `u32`; saturating keeps that a property of `Format`, not of
    // this line.
    let grid_w = u32::try_from(fmt.cols())
        .unwrap_or(u32::MAX)
        .saturating_mul(CELL_W);
    let grid_h = u32::try_from(fmt.rows())
        .unwrap_or(u32::MAX)
        .saturating_mul(CELL_H);
    for y in 0..fmt.height() {
        for x in 0..fmt.width() {
            if (x >= grid_w || y >= grid_h) && pixel(x, y) != bg {
                return Err(CheckError::Margin {
                    x,
                    y,
                    rgb: rgb_of(pixel(x, y)),
                });
            }
        }
    }

    // Paint the decoded text again. Fed in reading order with no newline, the
    // bytes land cell for cell: writing the last column leaves the wrap
    // pending (F8), and the next byte takes it to the next row's first cell.
    let mut grid = Grid::new(fmt.cols(), fmt.rows());
    for &byte in rows.iter().flatten() {
        grid.put(byte);
    }
    let painted = render(fmt, &grid);
    for y in 0..fmt.height() {
        for x in 0..fmt.width() {
            let (dump, again) = (pixel(x, y), painted.get(x, y).unwrap_or(NO_PIXEL));
            if dump != again {
                return Err(CheckError::Rerender {
                    x,
                    y,
                    dump: rgb_of(dump),
                    painted: rgb_of(again),
                });
            }
        }
    }
    Ok(rows)
}

/// Say why a cell `decode_cell` refused is not a glyph: its first pixel of
/// another colour, or else its shape.
fn cell_fault(
    pixel: &impl Fn(u32, u32) -> u32,
    col: usize,
    row: usize,
    fg: u32,
    bg: u32,
) -> CellFault {
    // The cell is inside the grid (the caller is walking it), so these fit.
    let x0 = u32::try_from(col)
        .unwrap_or(u32::MAX)
        .saturating_mul(CELL_W);
    let y0 = u32::try_from(row)
        .unwrap_or(u32::MAX)
        .saturating_mul(CELL_H);
    let mut shape = [0u8; CELL_H as usize];
    for (y, bits) in (y0..).zip(shape.iter_mut()) {
        for (x, bit) in (x0..).zip(0..CELL_W) {
            let p = pixel(x, y);
            if p == fg {
                *bits |= 0x80 >> bit;
            } else if p != bg {
                return CellFault::Colour {
                    x,
                    y,
                    rgb: rgb_of(p),
                };
            }
        }
    }
    CellFault::Shape(shape)
}

/// The console's part of a serial log: `log` with every kernel line taken
/// out, from its `[name ` or `[name]` through its line end. See the module
/// docs for what that is and why.
pub fn console_stream(log: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    // `log[kept..]` has not been copied or dropped yet; `at` is where the
    // search for the next `[` resumes.
    let (mut kept, mut at) = (0, 0);
    while let Some(open) = log
        .get(at..)
        .and_then(|rest| rest.iter().position(|&b| b == b'['))
        .map(|offset| at + offset)
    {
        let line = log.get(open..).unwrap_or(&[]);
        if !starts_kernel_line(line) {
            at = open + 1;
            continue;
        }
        out.extend_from_slice(log.get(kept..open).unwrap_or(&[]));
        // Through the line end; a line with none yet is still being written
        // and goes too.
        at = line
            .iter()
            .position(|&b| b == b'\n')
            .map_or(log.len(), |end| open + end + 1);
        kept = at;
    }
    out.extend_from_slice(log.get(kept..).unwrap_or(&[]));
    out
}

/// Does `text` begin like a kernel line: `[`, a name of `a`–`z`, `_`, `.`,
/// `-`, then a space or `]`?
fn starts_kernel_line(text: &[u8]) -> bool {
    let Some((b'[', rest)) = text.split_first() else {
        return false;
    };
    let name = rest
        .iter()
        .take_while(|&&b| b.is_ascii_lowercase() || matches!(b, b'_' | b'.' | b'-'))
        .count();
    name > 0 && matches!(rest.get(name), Some(b' ' | b']'))
}

/// What step 3 counted.
struct RowsFound {
    non_blank: usize,
    wraps: usize,
    stray: usize,
    worst: usize,
}

/// Step 3: every non-blank row is in the console stream, and there are
/// enough of them.
fn rows_in_log(rows: &[Vec<u8>], console: &[u8]) -> Result<RowsFound, CheckError> {
    let mut found = RowsFound {
        non_blank: 0,
        wraps: 0,
        stray: 0,
        worst: 0,
    };
    for (r, row) in rows.iter().enumerate() {
        let text = trimmed(row);
        if text.is_empty() {
            continue;
        }
        found.non_blank += 1;
        let Some(at) = find_stray(console, text) else {
            return Err(CheckError::RowNotInLog {
                row: r,
                text: lossy(text),
            });
        };
        if at.strays > 0 {
            found.stray += 1;
            found.worst = found.worst.max(at.strays);
        }
        // A row that ends in the last column may be a line that wrapped. If
        // its text runs straight on into the next row's in the log, it was.
        // Not finding that refuses nothing: see the module docs.
        let below = rows.get(r + 1).map_or(&[][..], |next| trimmed(next));
        if row.last().is_some_and(|&b| b != BLANK) && !below.is_empty() {
            let joined = [row.as_slice(), below].concat();
            if find_stray(console, &joined).is_some() {
                found.wraps += 1;
            }
        }
    }
    if found.non_blank < MIN_ROWS {
        return Err(CheckError::TooFewRows {
            found: found.non_blank,
        });
    }
    Ok(found)
}

/// `row` without its trailing blanks.
fn trimmed(row: &[u8]) -> &[u8] {
    let len = row.iter().rposition(|&b| b != BLANK).map_or(0, |i| i + 1);
    row.get(..len).unwrap_or(row)
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Where `needle` first occurs in `hay`, byte for byte. An empty needle is
/// nowhere: nothing here searches for one.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let (&first, _) = needle.split_first()?;
    let mut from = 0;
    loop {
        let rest = hay.get(from..)?;
        let at = from + rest.iter().position(|&b| b == first)?;
        if hay.get(at..)?.starts_with(needle) {
            return Some(at);
        }
        from = at + 1;
    }
}

/// Where a tolerant search found its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Found {
    /// Index of the text's first byte in the haystack.
    start: usize,
    /// One past its last byte, strays included.
    end: usize,
    /// How many stray bytes sit inside it: 0 for an exact occurrence.
    strays: usize,
}

/// May `byte` be one of the kernel's tick bytes? A letter — the first byte of
/// a process's name — or the `?` printed for a process without one.
const fn may_be_stray(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'?'
}

/// Find `needle` in `hay`, allowing runs of stray bytes between its bytes,
/// [`MAX_STRAY`] in total (the module docs give the rule). An exact
/// occurrence anywhere is preferred; failing that, the occurrence needing the
/// fewest strays, and of those the leftmost.
fn find_stray(hay: &[u8], needle: &[u8]) -> Option<Found> {
    if let Some(start) = find(hay, needle) {
        return Some(Found {
            start,
            end: start + needle.len(),
            strays: 0,
        });
    }
    let (&first, tail) = needle.split_first()?;
    let mut best: Option<Found> = None;
    for (start, _) in hay.iter().enumerate().filter(|&(_, &b)| b == first) {
        // Only an occurrence better than the best so far is of interest. No
        // occurrence is exact, so one stray cannot be bettered.
        let limit = match best {
            Some(Found { strays: 1, .. }) => break,
            Some(found) => found.strays - 1,
            None => MAX_STRAY,
        };
        if let Some(strays) = strays_from(hay, start, tail, limit) {
            best = Some(Found {
                start,
                end: start + needle.len() + strays,
                strays,
            });
        }
    }
    best
}

/// The fewest stray bytes, at most `limit`, with which `tail` follows the
/// byte at `hay[start]`; `None` if it does not.
///
/// Bit `k` of `reach`: the needle's bytes so far can be laid on `hay` from
/// `start` with exactly `k` strays among them, the latest at `start + j + k`.
/// Every way of placing the strays is followed at once, which a matcher that
/// commits to the first byte that fits cannot do.
fn strays_from(hay: &[u8], start: usize, tail: &[u8], limit: usize) -> Option<usize> {
    let mut reach: u64 = 1;
    for (j, &want) in (1..).zip(tail) {
        let mut next: u64 = 0;
        let mut from = reach;
        while from != 0 {
            let k = from.trailing_zeros() as usize;
            from &= from - 1;
            // Walk forward from the byte after the previous one: each byte
            // may be the one wanted, and may be skipped only as a stray.
            let mut strays = k;
            while let Some(&byte) = hay.get(start + j + strays) {
                if byte == want {
                    next |= 1 << strays;
                }
                if strays == limit || !may_be_stray(byte) {
                    break;
                }
                strays += 1;
            }
        }
        reach = next;
        if reach == 0 {
            return None;
        }
    }
    Some(reach.trailing_zeros() as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small screen with both margins: 10 × 10 cells, 5 spare pixels on the
    /// right and 8 below.
    const W: u32 = 85;
    const H: u32 = 168;
    const COLS: usize = 10;

    /// Eight short lines: the fewest that pass.
    const EIGHT: &[u8] = b"alpha 1\nbravo 22\ncharlie 3\ndelta\necho 5\nfoxtrot\ngolf\nhotel 8\n";

    fn fmt(w: u32, h: u32) -> Format {
        dump_format(w, h).unwrap()
    }

    /// What TTY would have on a `w × h` screen after writing `text`.
    fn canvas(w: u32, h: u32, text: &[u8]) -> Canvas {
        let f = fmt(w, h);
        let mut grid = Grid::new(f.cols(), f.rows());
        for &byte in text {
            grid.put(byte);
        }
        render(&f, &grid)
    }

    fn dump_of(canvas: &Canvas) -> Ppm {
        Ppm::parse(&canvas.to_ppm()).unwrap()
    }

    fn dump(w: u32, h: u32, text: &[u8]) -> Ppm {
        dump_of(&canvas(w, h, text))
    }

    /// TTY's verdict line for a `w × h` screen, as the renderer sizes it.
    fn fb_ok(w: u32, h: u32) -> Vec<u8> {
        let f = fmt(w, h);
        format!(
            "[diag tty] fb ok w={w} h={h} cols={} rows={}\n",
            f.cols(),
            f.rows()
        )
        .into_bytes()
    }

    /// A serial log: the verdict line, then `body`.
    fn log(w: u32, h: u32, body: &[u8]) -> Vec<u8> {
        [fb_ok(w, h).as_slice(), body].concat()
    }

    /// `[u8]` has no `replace`; the tests want one, and want to hear about a
    /// pattern that was not there to be replaced.
    trait Replace {
        fn replace(&self, from: &[u8], to: &[u8]) -> Vec<u8>;
    }

    impl Replace for [u8] {
        fn replace(&self, from: &[u8], to: &[u8]) -> Vec<u8> {
            let at = find(self, from).expect("the pattern to replace is not there");
            [&self[..at], to, &self[at + from.len()..]].concat()
        }
    }

    /// Check a screen showing `text` against a log holding exactly `text`.
    fn check_text(text: &[u8]) -> Result<Report, CheckError> {
        check(&dump(W, H, text), &log(W, H, text))
    }

    // --- the whole check passes ---------------------------------------------

    #[test]
    fn a_rendered_screen_decodes_back_to_its_text_and_passes() {
        let report = check_text(EIGHT).unwrap();
        assert_eq!(
            report.geometry,
            Geometry {
                width: 85,
                height: 168,
                cols: 10,
                rows: 10
            }
        );
        assert_eq!(report.non_blank_rows, 8);
        assert_eq!(report.wraps, 0);
        assert_eq!(
            report.text,
            [
                "alpha 1",
                "bravo 22",
                "charlie 3",
                "delta",
                "echo 5",
                "foxtrot",
                "golf",
                "hotel 8",
                "",
                ""
            ]
        );
        assert_eq!(
            report.to_string(),
            "ok 85x168 10x10 rows=8 wraps=0 stray=0 worst=0"
        );
    }

    #[test]
    fn every_printable_byte_decodes_back() {
        // 95 printable bytes on a 16-column screen: six rows, five of them
        // full, plus two short lines to make eight.
        let mut text: Vec<u8> = (0x20..=0x7E).collect();
        text.extend_from_slice(b"\nseven\neight\n");
        let (w, h) = (16 * 8, 9 * 16);
        let report = check(&dump(w, h, &text), &log(w, h, &text)).unwrap();
        assert_eq!(report.text[0], " !\"#$%&'()*+,-./");
        assert_eq!(report.text[5], "pqrstuvwxyz{|}~");
        assert_eq!(report.non_blank_rows, 8);
        // Five full rows, each running on into the next.
        assert_eq!(report.wraps, 5);
    }

    #[test]
    fn the_measured_geometry_passes() {
        // Task 0: 800×600 is 100 × 37 cells with an 8-pixel bottom margin.
        let mut text = Vec::new();
        for n in 0..50 {
            text.extend_from_slice(format!("minix.rs init: rotation line {n}\n").as_bytes());
        }
        let report = check(&dump(800, 600, &text), &log(800, 600, &text)).unwrap();
        assert_eq!(
            report.geometry,
            Geometry {
                width: 800,
                height: 600,
                cols: 100,
                rows: 37
            }
        );
        // 50 lines on 37 rows: the screen scrolled, the last row is the
        // cursor's and empty.
        assert_eq!(report.non_blank_rows, 36);
        assert_eq!(report.text[0], "minix.rs init: rotation line 14");
        assert_eq!(report.text[35], "minix.rs init: rotation line 49");
        assert_eq!(
            report.to_string(),
            "ok 800x600 100x37 rows=36 wraps=0 stray=0 worst=0"
        );
    }

    #[test]
    fn a_screen_larger_than_the_grid_keeps_the_rest_as_margin() {
        // 1920×1080 is 240 × 67 cells; the grid stops at 160 × 64.
        let report = check(&dump(1920, 1080, EIGHT), &log(1920, 1080, EIGHT)).unwrap();
        assert_eq!((report.geometry.cols, report.geometry.rows), (160, 64));
        let mut c = canvas(1920, 1080, EIGHT);
        // The first pixel right of column 159.
        c.store(160 * 8, 0, fmt(1920, 1080).fg());
        assert_eq!(
            check(&dump_of(&c), &log(1920, 1080, EIGHT)),
            Err(CheckError::Margin {
                x: 1280,
                y: 0,
                rgb: 0xAAAAAA
            })
        );
    }

    // --- step 1: geometry ----------------------------------------------------

    #[test]
    fn a_log_without_the_fb_ok_line_is_refused() {
        let image = dump(W, H, EIGHT);
        assert_eq!(check(&image, EIGHT), Err(CheckError::NoGeometryLine));
        assert_eq!(check(&image, b""), Err(CheckError::NoGeometryLine));
        // The other two verdicts are not this one.
        let mut other = b"[diag tty] fb none rc=-2\n[diag tty] fb FAIL pitch\n".to_vec();
        other.extend_from_slice(EIGHT);
        assert_eq!(check(&image, &other), Err(CheckError::NoGeometryLine));
    }

    #[test]
    fn an_incomplete_or_malformed_fb_ok_line_is_refused() {
        let image = dump(W, H, EIGHT);
        for line in [
            // Still being written: no line end.
            &b"[diag tty] fb ok w=85 h=168 cols=10 rows=10"[..],
            b"[diag tty] fb ok w=85 h=168 cols=10 rows=1",
            b"[diag tty] fb ok w=85 h=1",
            // Complete lines that are not the format.
            b"[diag tty] fb ok w=85 h=168 cols=10\n",
            b"[diag tty] fb ok w=85 h=168 cols=10 rows=\n",
            b"[diag tty] fb ok w=85 h=168 cols=10 rows=10 extra\n",
            b"[diag tty] fb ok w=x h=168 cols=10 rows=10\n",
            b"[diag tty] fb ok h=168 w=85 cols=10 rows=10\n",
            b"[diag tty] fb ok w=850000000000 h=168 cols=10 rows=10\n",
            // Three stray bytes among the numbers: past the tolerance there.
            b"[diag tty] fb ok w=8x5 h=1x68 cols=1x0 rows=10\n",
            b"[diag tty] fb ok w=85 h=168 cxolxs=10 rxows=10\n",
            // A stray is a letter or `?`: not a space, a sign or a digit.
            b"[diag tty] fb ok w=85 h=168 cols=10 rows=10 \n",
            b"[diag tty] fb ok w=-85 h=168 cols=10 rows=10\n",
            b"[diag tty] fb ok w=85  h=168 cols=10 rows=10\n",
            b"[diag tty] fb ok w=85 h=168 cols=10 rows=10.\n",
            // A byte missing or changed is never forgiven.
            b"[diag tty] fb ok w=85 h=168 cols=10 rws=10\n",
            b"[diag tty] fb ok w=85 h=168 cols=10 rowz=10\n",
            b"[diag tty] fb ok\n",
        ] {
            let mut bytes = line.to_vec();
            bytes.extend_from_slice(EIGHT);
            let result = check(&image, &bytes);
            assert!(
                matches!(result, Err(CheckError::BadGeometryLine(_))),
                "{:?} gave {result:?}",
                String::from_utf8_lossy(line)
            );
        }
    }

    #[test]
    fn the_fb_ok_line_may_end_in_a_carriage_return() {
        let mut bytes = b"[diag tty] fb ok w=85 h=168 cols=10 rows=10\r\n".to_vec();
        bytes.extend_from_slice(EIGHT);
        assert!(check(&dump(W, H, EIGHT), &bytes).is_ok());
    }

    #[test]
    fn the_last_fb_ok_line_is_the_one_used() {
        let image = dump(W, H, EIGHT);
        let stale = b"[diag tty] fb ok w=800 h=600 cols=100 rows=37\n";

        // An earlier TTY's line first, this one's last: passes.
        let good = [&stale[..], log(W, H, EIGHT).as_slice()].concat();
        assert!(check(&image, &good).is_ok());

        // The other way round, the last line describes another screen.
        let bad = [log(W, H, EIGHT).as_slice(), &stale[..]].concat();
        assert_eq!(
            check(&image, &bad),
            Err(CheckError::SizeMismatch {
                dump: (85, 168),
                log: (800, 600)
            })
        );

        // A LAST line with no line end is still being written, however
        // complete it looks: "rows=10" may be the start of "rows=100".
        let unended = [
            log(W, H, EIGHT).as_slice(),
            b"[diag tty] fb ok w=85 h=168 cols=10 rows=10",
        ]
        .concat();
        assert!(matches!(
            check(&image, &unended),
            Err(CheckError::BadGeometryLine(_))
        ));

        // A malformed LAST line is an error, not a reason to use an earlier one.
        let torn = [log(W, H, EIGHT).as_slice(), b"[diag tty] fb ok w=85"].concat();
        assert!(matches!(
            check(&image, &torn),
            Err(CheckError::BadGeometryLine(_))
        ));
    }

    #[test]
    fn a_dump_of_the_wrong_size_is_refused() {
        // Right text, right log, a screen one pixel wider or taller.
        for (w, h) in [(W + 1, H), (W, H + 1), (W - 1, H), (W, H - 1), (H, W)] {
            assert_eq!(
                check(&dump(w, h, EIGHT), &log(W, H, EIGHT)),
                Err(CheckError::SizeMismatch {
                    dump: (w, h),
                    log: (W, H)
                }),
                "{w}x{h}"
            );
        }
    }

    #[test]
    fn a_grid_the_renderer_would_not_derive_is_refused() {
        let image = dump(W, H, EIGHT);
        for (cols, rows) in [(11, 10), (10, 11), (9, 10), (10, 9), (0, 0)] {
            let mut bytes =
                format!("[diag tty] fb ok w={W} h={H} cols={cols} rows={rows}\n").into_bytes();
            bytes.extend_from_slice(EIGHT);
            assert_eq!(
                check(&image, &bytes),
                Err(CheckError::GridMismatch {
                    derived: (10, 10),
                    log: (cols, rows)
                })
            );
        }
    }

    // --- step 2: cells, margins ------------------------------------------------

    #[test]
    fn one_flipped_pixel_is_refused_wherever_it_is() {
        let f = fmt(W, H);
        let clean = canvas(W, H, EIGHT);
        let bytes = log(W, H, EIGHT);
        // Every pixel of the cell holding the `b` of "bravo" (col 0, row 1),
        // and of the blank cell after "delta" (col 5, row 3).
        for (col, row) in [(0u32, 1u32), (5, 3)] {
            for dy in 0..CELL_H {
                for dx in 0..CELL_W {
                    let (x, y) = (col * CELL_W + dx, row * CELL_H + dy);
                    let mut c = clean.clone();
                    let was = c.get(x, y).unwrap();
                    c.store(x, y, if was == f.fg() { f.bg() } else { f.fg() });
                    let result = check(&dump_of(&c), &bytes);
                    assert!(result.is_err(), "flipping ({x}, {y}) passed: {result:?}");
                }
            }
        }
    }

    #[test]
    fn a_flipped_pixel_is_reported_with_its_cell_and_shape() {
        let f = fmt(W, H);
        let mut c = canvas(W, H, EIGHT);
        // The blank cell at col 9, row 0: one lit pixel is no glyph.
        c.store(9 * 8 + 3, 5, f.fg());
        let err = check(&dump_of(&c), &log(W, H, EIGHT)).unwrap_err();
        let mut shape = [0u8; 16];
        shape[5] = 0x10;
        assert_eq!(
            err,
            CheckError::Cell {
                col: 9,
                row: 0,
                fault: CellFault::Shape(shape)
            }
        );
        let text = err.to_string();
        assert!(text.contains("col=9 row=0"), "{text}");
        assert!(text.contains(" 10 "), "{text}");
    }

    #[test]
    fn a_pixel_of_another_colour_is_refused_and_named() {
        let f = fmt(W, H);
        // One step off each console colour, and a colour of its own.
        for (pixel, rgb) in [
            (f.pack(0xAA, 0xAA, 0xAB), 0xAAAAAB),
            (f.pack(0x00, 0x01, 0x00), 0x000100),
            (f.pack(0xFF, 0x00, 0x00), 0xFF0000),
        ] {
            let mut c = canvas(W, H, EIGHT);
            // Inside the cell at col 2, row 4.
            c.store(2 * 8 + 1, 4 * 16 + 2, pixel);
            let err = check(&dump_of(&c), &log(W, H, EIGHT)).unwrap_err();
            assert_eq!(
                err,
                CheckError::Cell {
                    col: 2,
                    row: 4,
                    fault: CellFault::Colour { x: 17, y: 66, rgb }
                }
            );
            assert!(err.to_string().contains(&format!("#{rgb:06x}")), "{err}");
        }
    }

    #[test]
    fn a_non_background_margin_pixel_is_refused() {
        let f = fmt(W, H);
        // The grid is 80 × 160 of 85 × 168.
        for (x, y) in [
            (80, 0),   // first pixel right of the grid
            (84, 0),   // last column of the image
            (84, 159), // right margin, last grid row
            (0, 160),  // first pixel below the grid
            (0, 167),  // last row of the image
            (79, 160), // below the grid's last column
            (80, 160), // the corner both margins share
            (84, 167), // the image's last pixel
        ] {
            for pixel in [f.fg(), f.pack(0, 0, 1)] {
                let mut c = canvas(W, H, EIGHT);
                c.store(x, y, pixel);
                assert_eq!(
                    check(&dump_of(&c), &log(W, H, EIGHT)),
                    Err(CheckError::Margin {
                        x,
                        y,
                        rgb: rgb_of(pixel)
                    }),
                    "({x}, {y})"
                );
            }
        }
        // The grid's own last pixel is not margin: lit, it is a bad cell.
        let mut c = canvas(W, H, EIGHT);
        c.store(79, 159, f.fg());
        assert!(matches!(
            check(&dump_of(&c), &log(W, H, EIGHT)),
            Err(CheckError::Cell { col: 9, row: 9, .. })
        ));
    }

    #[test]
    fn an_uncleared_screen_is_refused() {
        // Painted but never cleared: the margins are whatever was there.
        let f = fmt(W, H);
        let mut grid = Grid::new(f.cols(), f.rows());
        for &byte in EIGHT {
            grid.put(byte);
        }
        let mut c = Canvas::new(&f, f.pack(0x12, 0x34, 0x56));
        paint_all(&mut c, &f, &grid);
        assert_eq!(
            check(&dump_of(&c), &log(W, H, EIGHT)),
            Err(CheckError::Margin {
                x: 80,
                y: 0,
                rgb: 0x123456
            })
        );
    }

    // --- step 3: the rows are in the log -----------------------------------------

    #[test]
    fn a_row_absent_from_the_log_is_refused_and_named() {
        let image = dump(W, H, EIGHT);
        // The log never had "delta" (row 3).
        let body = b"alpha 1\nbravo 22\ncharlie 3\necho 5\nfoxtrot\ngolf\nhotel 8\n";
        let err = check(&image, &log(W, H, body)).unwrap_err();
        assert_eq!(
            err,
            CheckError::RowNotInLog {
                row: 3,
                text: "delta".into()
            }
        );
        assert_eq!(
            err.to_string(),
            "screen row 3 is not in the serial log's console text, even allowing 32 tick bytes inside it: \"delta\""
        );

        // One byte different is absent too, in the first row and in the last.
        let body = EIGHT.replace(b"alpha 1", b"alpha 2");
        assert_eq!(
            check(&image, &log(W, H, &body)),
            Err(CheckError::RowNotInLog {
                row: 0,
                text: "alpha 1".into()
            })
        );
        let body = EIGHT.replace(b"hotel 8", b"hotel 9");
        assert_eq!(
            check(&image, &log(W, H, &body)),
            Err(CheckError::RowNotInLog {
                row: 7,
                text: "hotel 8".into()
            })
        );
    }

    #[test]
    fn leading_blanks_count_and_trailing_blanks_do_not() {
        let text = b"  indent\ntrail   \n3\n4\n5\n6\n7\n8\n";
        let report = check_text(text).unwrap();
        assert_eq!(report.text[0], "  indent");
        assert_eq!(report.text[1], "trail");
        // The log has the row's text without the indent: refused.
        let body = text.replace(b"  indent", b" indent");
        assert_eq!(
            check(&dump(W, H, text), &log(W, H, &body)),
            Err(CheckError::RowNotInLog {
                row: 0,
                text: "  indent".into()
            })
        );
        // The log has the row without its trailing blanks: still found.
        let body = text.replace(b"trail   ", b"trail");
        assert!(check(&dump(W, H, text), &log(W, H, &body)).is_ok());
    }

    #[test]
    fn too_few_rows_are_refused() {
        // Seven lines: one short.
        let seven = b"1 one\n2 two\n3 three\n4 four\n5 five\n6 six\n7 seven\n";
        assert_eq!(check_text(seven), Err(CheckError::TooFewRows { found: 7 }));
        // The eighth makes it pass.
        let eight = [&seven[..], b"8 eight\n"].concat();
        assert_eq!(check_text(&eight).unwrap().non_blank_rows, 8);
        // A blank screen is zero rows, with or without a log full of text.
        assert_eq!(check_text(b""), Err(CheckError::TooFewRows { found: 0 }));
        assert_eq!(
            check(&dump(W, H, b""), &log(W, H, EIGHT)),
            Err(CheckError::TooFewRows { found: 0 })
        );
        assert_eq!(MIN_ROWS, 8);
    }

    #[test]
    fn a_row_missing_from_the_log_outranks_too_few_rows() {
        // Two rows, one of them not the boot's: say that, it is the news.
        assert_eq!(
            check(&dump(W, H, b"one\ntwo\n"), &log(W, H, b"one\n")),
            Err(CheckError::RowNotInLog {
                row: 1,
                text: "two".into()
            })
        );
    }

    // --- step 3: wrapped lines ---------------------------------------------------

    #[test]
    fn a_wrapped_line_in_the_middle_of_the_screen_is_found_and_counted() {
        // 23 bytes on 10 columns: two full rows and a tail.
        let text = b"one\ntwo\n0123456789abcdefghijKLM\nsix\nseven\neight\n";
        let report = check_text(text).unwrap();
        assert_eq!(
            &report.text[2..5],
            ["0123456789", "abcdefghij", "KLM"],
            "the line took three rows"
        );
        assert_eq!(report.non_blank_rows, 8);
        assert_eq!(report.wraps, 2, "both full rows run on into the next");
    }

    #[test]
    fn a_wrapped_line_whose_continuation_is_the_last_row_is_found() {
        // Eight lines fill rows 0..=7; the ninth wraps from row 8 into row 9,
        // the screen's last, and nothing has scrolled.
        let text = [EIGHT, b"0123456789tail"].concat();
        let report = check_text(&text).unwrap();
        assert_eq!(report.text[8], "0123456789");
        assert_eq!(report.text[9], "tail");
        assert_eq!(report.text[0], "alpha 1", "nothing scrolled");
        assert_eq!(report.wraps, 1);
    }

    #[test]
    fn a_full_last_row_with_nothing_below_it_is_found() {
        // The wrap is pending: the last column of the last row was written
        // and the next byte has not come.
        let text = [EIGHT, b"nine\n0123456789"].concat();
        let report = check_text(&text).unwrap();
        assert_eq!(report.text[9], "0123456789");
        assert_eq!(report.wraps, 0, "there is no row below to run on into");
    }

    #[test]
    fn a_top_row_that_is_the_tail_of_a_scrolled_off_line_is_found() {
        // The head of the first line scrolls away; its tail is row 0.
        let text = [b"HEADHEADHHthe tail\n".as_slice(), EIGHT].concat();
        let report = check_text(&text).unwrap();
        assert_eq!(report.text[0], "the tail");
        assert_eq!(report.text[1], "alpha 1");
        assert_eq!(report.text[9], "", "the cursor's row");
        assert_eq!(report.wraps, 0);
    }

    #[test]
    fn a_top_row_from_the_middle_of_a_scrolled_off_line_is_found() {
        // Three rows of one line; the first scrolls away, leaving a full row
        // on top that still runs on into its tail.
        let text = [b"HEADHEADHHabcdefghijtail\n".as_slice(), &EIGHT[..53]].concat();
        let report = check_text(&text).unwrap();
        assert_eq!(report.text[0], "abcdefghij");
        assert_eq!(report.text[1], "tail");
        assert_eq!(report.wraps, 1);
    }

    #[test]
    fn a_line_exactly_as_wide_as_the_screen_is_not_joined_to_the_next() {
        // Ten bytes and a newline take ONE row (deferred wrap). Joined to the
        // row below it would be "exactly-10next", which nobody wrote.
        let text = [b"exactly-10\nnext\n".as_slice(), &EIGHT[..48]].concat();
        let report = check_text(&text).unwrap();
        assert_eq!(report.text[0], "exactly-10");
        assert_eq!(report.text[1], "next");
        assert_eq!(report.wraps, 0);
        assert!(find(&log(W, H, &text), b"exactly-10next").is_none());
    }

    #[test]
    fn only_a_row_that_reaches_the_last_column_can_be_a_wrap() {
        // "ab" then "cd" on two rows. The log also holds "abcd", and the two
        // rows' cells end to end ("ab", eight blanks, "cd"), on lines of
        // their own. The rows are short, so a line end separated them: no
        // wrap is counted, whatever else the log happens to hold.
        let text = [b"ab\ncd\n".as_slice(), &EIGHT[..48]].concat();
        let body = [text.as_slice(), b"abcd\nab        cd\n"].concat();
        let report = check(&dump(W, H, &text), &log(W, H, &body)).unwrap();
        assert_eq!(&report.text[..2], ["ab", "cd"]);
        assert_eq!(report.wraps, 0);
    }

    #[test]
    fn a_chain_of_full_width_rows_is_taken_apart_where_the_lines_end() {
        // Row 0 is a whole line of exactly ten bytes; rows 1 to 3 are one
        // line of 23; row 4 is another exact ten; then short lines.
        let text = b"exactly-10\n0123456789abcdefghijKLM\nEXACTLY=10\nfive\nsix\nseven\n";
        let report = check_text(text).unwrap();
        assert_eq!(
            &report.text[..6],
            [
                "exactly-10",
                "0123456789",
                "abcdefghij",
                "KLM",
                "EXACTLY=10",
                "five"
            ]
        );
        assert_eq!(report.non_blank_rows, 8);
        // Rows 1 and 2 run on; rows 0 and 4 are full and do not.
        assert_eq!(report.wraps, 2);
    }

    #[test]
    fn a_wrapped_line_the_log_does_not_hold_is_refused_at_the_row_that_differs() {
        let text = b"one\ntwo\n0123456789abcdefghijKLM\nsix\nseven\neight\n";
        let image = dump(W, H, text);
        // The log's line ends differently: the tail row is not in it.
        let body = text.replace(b"KLM", b"XYZ");
        assert_eq!(
            check(&image, &log(W, H, &body)),
            Err(CheckError::RowNotInLog {
                row: 4,
                text: "KLM".into()
            })
        );
        // The log's line differs in its middle: the second full row is not.
        let body = text.replace(b"abcdefghij", b"abcde-ghij");
        assert_eq!(
            check(&image, &log(W, H, &body)),
            Err(CheckError::RowNotInLog {
                row: 3,
                text: "abcdefghij".into()
            })
        );
    }

    // --- step 3: the log is bytes ----------------------------------------------

    #[test]
    fn stray_bytes_and_other_writers_around_the_lines_do_not_matter() {
        // Not UTF-8, NULs, CR LF line ends, kernel lines between and beside
        // the console's, and a raw byte directly before and after a line.
        let mut bytes = b"\xFF\xFE\x00[kernel] boot\r\n".to_vec();
        bytes.extend_from_slice(b"[diag tty] fb ok w=85 h=168 cols=10 rows=10\r\n");
        for line in EIGHT.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            bytes.extend_from_slice(b"\x80[ipc 12]\x00\n\xC3");
            bytes.extend_from_slice(line);
            bytes.extend_from_slice(b"\xFF\r\n[diag vfs] something else\r\n");
        }
        assert!(std::str::from_utf8(&bytes).is_err());
        let report = check(&dump(W, H, EIGHT), &bytes).unwrap();
        assert_eq!(report.non_blank_rows, 8);
    }

    #[test]
    fn the_kernels_tick_byte_inside_a_line_is_tolerated() {
        // Measured, slice 6.7: the log held "minix.rs initt: dev.null ok n=35"
        // for a row the screen showed without the second `t`.
        let row = b"minix.rs init: dev.null ok n=35\n";
        let text = [row.as_slice(), EIGHT].concat();
        let body = text.replace(b"minix.rs init:", b"minix.rs initt:");
        let (w, h) = (320, 160);
        let report = check(&dump(w, h, &text), &log(w, h, &body)).unwrap();
        assert_eq!(report.text[0], "minix.rs init: dev.null ok n=35");
        assert_eq!(report.stray, 1, "one row needed the tolerance");
        assert_eq!(report.worst, 1);
        assert_eq!(report.non_blank_rows, 9);
        assert_eq!(
            report.to_string(),
            "ok 320x160 40x10 rows=9 wraps=0 stray=1 worst=1"
        );

        // Two strays in one row, and strays in two rows.
        let body = body.replace(b"dev.null", b"dev.nSull");
        let report = check(&dump(w, h, &text), &log(w, h, &body)).unwrap();
        assert_eq!(report.stray, 1);
        let body = body.replace(b"charlie 3", b"charVlie 3");
        let report = check(&dump(w, h, &text), &log(w, h, &body)).unwrap();
        assert_eq!(report.stray, 2);
    }

    #[test]
    fn a_run_of_tick_bytes_inside_a_row_is_tolerated() {
        // Measured, slice 6.7: "minix.rs hello: iov CCCCCCCCCC......" in the
        // log for a row without the Cs.
        let text = [b"hello: iov ..........ok\n".as_slice(), EIGHT].concat();
        let body = text.replace(b"iov ...", b"iov CCCCCCCCCC...");
        let (w, h) = (320, 160);
        let report = check(&dump(w, h, &text), &log(w, h, &body)).unwrap();
        assert_eq!(report.text[0], "hello: iov ..........ok");
        assert_eq!((report.stray, report.worst), (1, 10));

        // A later row that needs fewer does not lower the worst.
        let two = body.replace(b"charlie 3", b"chaVrlie 3");
        let report = check(&dump(w, h, &text), &log(w, h, &two)).unwrap();
        assert_eq!((report.stray, report.worst), (2, 10));

        // Runs in several places of one row add up; mixed letters and `?`.
        let body = body.replace(b"hello:", b"htCello?:");
        let report = check(&dump(w, h, &text), &log(w, h, &body)).unwrap();
        assert_eq!((report.stray, report.worst), (1, 13));

        // A run BEFORE the row's first byte or after its last is not in it.
        let body = [
            b"CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC".as_slice(),
            &text,
            b"tttt",
        ]
        .concat();
        let report = check(&dump(w, h, &text), &log(w, h, &body)).unwrap();
        assert_eq!((report.stray, report.worst), (0, 0));
    }

    #[test]
    fn more_tick_bytes_than_the_cap_in_one_row_are_refused() {
        assert_eq!(MAX_STRAY, 32);
        let text = [b"hello: iov ...ok\n".as_slice(), EIGHT].concat();
        let (w, h) = (320, 160);
        let image = dump(w, h, &text);
        let with_run = |n: usize| {
            let run = [b"iov ".as_slice(), &vec![b'C'; n], b"..."].concat();
            log(w, h, &text.replace(b"iov ...", &run))
        };
        // Exactly the cap passes, and says so.
        let report = check(&image, &with_run(MAX_STRAY)).unwrap();
        assert_eq!((report.stray, report.worst), (1, 32));
        // One more does not.
        assert_eq!(
            check(&image, &with_run(MAX_STRAY + 1)),
            Err(CheckError::RowNotInLog {
                row: 0,
                text: "hello: iov ...ok".into()
            })
        );
        // Nor does the cap spread over two places with one more somewhere.
        let body = with_run(MAX_STRAY).replace(b"hello:", b"hetllo:");
        assert!(matches!(
            check(&image, &body),
            Err(CheckError::RowNotInLog { row: 0, .. })
        ));
        // The row is found where ANY occurrence is within the cap: the fewest
        // strays over the whole log is the row's count.
        let body = [
            with_run(MAX_STRAY + 5),
            text.replace(b"iov ...", b"iov CCC..."),
        ]
        .concat();
        let report = check(&image, &body).unwrap();
        assert_eq!((report.stray, report.worst), (1, 3));
    }

    #[test]
    fn only_a_letter_or_a_question_mark_can_be_a_tick_byte() {
        let text = [b"abcd\n".as_slice(), EIGHT].concat();
        let image = dump(W, H, &text);
        for stray in *b"xCZa?" {
            let body = [b"ab".as_slice(), &[stray], b"cd\n", EIGHT].concat();
            let report = check(&image, &log(W, H, &body)).unwrap();
            assert_eq!(
                (report.stray, report.worst),
                (1, 1),
                "{:?}",
                char::from(stray)
            );
        }
        // A digit, a space, punctuation, a control or a high byte between
        // them, and "ab" and "cd" are not the row "abcd".
        for stray in [
            b'7', b'0', b' ', b'.', b'-', b'[', b'@', b'`', b'{', b'\t', 0x00, 0x1B, 0x7F, 0x80,
            0xFF,
        ] {
            let body = [b"ab".as_slice(), &[stray], b"cd\n", EIGHT].concat();
            assert_eq!(
                check(&image, &log(W, H, &body)),
                Err(CheckError::RowNotInLog {
                    row: 0,
                    text: "abcd".into()
                }),
                "{stray:#04x}"
            );
            // Not even with real tick bytes around it.
            let body = [b"abC".as_slice(), &[stray], b"Ccd\n", EIGHT].concat();
            assert!(check(&image, &log(W, H, &body)).is_err(), "{stray:#04x}");
        }
    }

    #[test]
    fn a_substituted_or_missing_byte_is_refused() {
        for changed in [
            &b"charlie 4"[..],
            b"chaslie 3",
            b"Charlie 3",
            b"charle 3",
            b"harlie 3",
            b"charlie ",
        ] {
            let body = EIGHT.replace(b"charlie 3", changed);
            assert_eq!(
                check(&dump(W, H, EIGHT), &log(W, H, &body)),
                Err(CheckError::RowNotInLog {
                    row: 2,
                    text: "charlie 3".into()
                }),
                "{:?}",
                String::from_utf8_lossy(changed)
            );
        }
    }

    #[test]
    fn text_on_either_side_of_a_line_end_is_never_one_row() {
        // "ab" and "cd" on two log lines are not the row "abcd", with either
        // kind of line end between them.
        let text = [b"abcd\n".as_slice(), EIGHT].concat();
        for end in [&b"\n"[..], b"\r", b"\r\n"] {
            let body = [b"ab".as_slice(), end, b"cd\n", EIGHT].concat();
            assert_eq!(
                check(&dump(W, H, &text), &log(W, H, &body)),
                Err(CheckError::RowNotInLog {
                    row: 0,
                    text: "abcd".into()
                })
            );
        }
        // Nor with tick bytes on both sides of the line end.
        let body = [b"abC\r\nCcd\n".as_slice(), EIGHT].concat();
        assert!(check(&dump(W, H, &text), &log(W, H, &body)).is_err());
    }

    #[test]
    fn the_fb_ok_line_tolerates_stray_bytes_too() {
        let image = dump(W, H, EIGHT);
        for line in [
            &b"[diag tty] fb ok w=85 h=1T68 cols=10 rows=10\n"[..],
            b"[diag ttyy] fb ok w=85 h=168 cols=10 rows=10\n",
            b"[diag tty] fb okk w=85 h=168 cols=10 rows=10\n",
            b"[diag tty] fb ok w=85 h=168 cols=10 rows=10V\n",
            b"[diag ttSy] fb ok w=85 h=168 cols=1V0 rows=10\n",
            b"[diag tty] fb ok w=85 h=168 cSols=10 rVows=10\r\n",
            b"S[diag tty] fb ok w=85 h=168 cols=10 rows=10\n",
        ] {
            let bytes = [line, EIGHT].concat();
            let result = check(&image, &bytes);
            assert!(
                result.is_ok(),
                "{:?} gave {result:?}",
                String::from_utf8_lossy(line)
            );
        }
    }

    // --- the tolerant matcher ------------------------------------------------------

    fn strays(hay: &[u8], needle: &[u8]) -> Option<usize> {
        find_stray(hay, needle).map(|found| found.strays)
    }

    #[test]
    fn the_tolerant_search_prefers_an_exact_occurrence() {
        assert_eq!(
            find_stray(b"axb ab", b"ab"),
            Some(Found {
                start: 4,
                end: 6,
                strays: 0
            })
        );
        assert_eq!(
            find_stray(b"..axb..", b"ab"),
            Some(Found {
                start: 2,
                end: 5,
                strays: 1
            })
        );
        assert_eq!(find_stray(b"abc", b""), None);
        assert_eq!(find_stray(b"", b"a"), None);
        assert_eq!(strays(b"a", b"a"), Some(0));
    }

    #[test]
    fn the_tolerant_search_is_not_greedy() {
        // `-` is never a stray, so these have one reading each. Cases where
        // taking the first byte that fits, or the first start that begins to
        // fit, loses the match.
        assert_eq!(strays(b"aax-", b"aa-"), Some(1));
        assert_eq!(strays(b"axa-", b"aa-"), Some(1));
        assert_eq!(
            strays(b"a-axa-", b"aa-"),
            Some(1),
            "the second start, not the first"
        );
        assert_eq!(strays(b"-a-xx-b", b"-ab"), None);
        assert_eq!(strays(b"initt:", b"init:"), Some(1));
        assert_eq!(strays(b"iov CCCCCCCCCC...", b"iov ..."), Some(10));
        // A needle byte that is itself a letter may be the stray or the
        // match: `a(b)b(xx)a`, not `ab` and then no `a`.
        assert_eq!(strays(b"abbxxa", b"aba"), Some(3));
        assert_eq!(strays(b"abb-a", b"aba"), None, "`-` is in the way");
        // Strays before the first byte or after the last are not inside it.
        assert_eq!(strays(b"xxxa-bxxx", b"a-b"), Some(0));
        // The fewest strays of any occurrence, and of those the leftmost.
        assert_eq!(
            find_stray(b"-xxxa -xa -ya", b"-a"),
            Some(Found {
                start: 6,
                end: 9,
                strays: 1
            })
        );
        // ... including when one start works with fewer or with more.
        assert_eq!(
            find_stray(b"axbbbb", b"abbb"),
            Some(Found {
                start: 0,
                end: 5,
                strays: 1
            })
        );
    }

    #[test]
    fn the_tolerant_search_agrees_with_trying_every_deletion() {
        // The rule, spelled out the slow way: some stretch of `hay` starts
        // and ends with the needle's first and last bytes and becomes the
        // needle when some of its inner letters — at most `limit` — are
        // taken out. Checked for every needle of up to 4 bytes over {a, -}
        // against every haystack of up to 6 bytes over {a, x, -}, at every
        // limit that can matter, from every start.
        fn strings(alphabet: &[u8], max: usize) -> Vec<Vec<u8>> {
            let mut all = vec![vec![]];
            let mut from = 0;
            for _ in 0..max {
                let upto = all.len();
                for i in from..upto {
                    for &c in alphabet {
                        let mut s = all[i].clone();
                        s.push(c);
                        all.push(s);
                    }
                }
                from = upto;
            }
            all
        }
        /// The fewest deletions that turn a stretch starting at `start` into
        /// the needle.
        fn slow(hay: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
            let mut best = None;
            for end in start + needle.len()..=hay.len() {
                let stretch = &hay[start..end];
                let inner = stretch.len().saturating_sub(2);
                for mask in 0u32..1 << inner {
                    let mut kept = Vec::new();
                    let mut legal = true;
                    for (i, &b) in stretch.iter().enumerate() {
                        let out = i >= 1 && i <= inner && mask & (1 << (i - 1)) != 0;
                        if out {
                            legal &= b.is_ascii_alphabetic() || b == b'?';
                        } else {
                            kept.push(b);
                        }
                    }
                    let n = mask.count_ones() as usize;
                    if legal && kept == needle && best.is_none_or(|b| n < b) {
                        best = Some(n);
                    }
                }
            }
            best
        }
        let hays = strings(b"ax-", 6);
        let mut tolerant = 0;
        for needle in strings(b"a-", 4).iter().filter(|n| !n.is_empty()) {
            for hay in &hays {
                let mut overall: Option<usize> = None;
                for start in 0..hay.len() {
                    if hay[start] != needle[0] {
                        continue;
                    }
                    let want = slow(hay, start, needle);
                    for limit in 0..=5 {
                        assert_eq!(
                            strays_from(hay, start, &needle[1..], limit),
                            want.filter(|&n| n <= limit),
                            "needle {:?} in {:?} from {start}, limit {limit}",
                            String::from_utf8_lossy(needle),
                            String::from_utf8_lossy(hay)
                        );
                    }
                    if let Some(n) = want {
                        overall = Some(overall.map_or(n, |o| o.min(n)));
                    }
                }
                assert_eq!(
                    find_stray(hay, needle).map(|f| f.strays),
                    overall,
                    "needle {:?} in {:?}",
                    String::from_utf8_lossy(needle),
                    String::from_utf8_lossy(hay)
                );
                tolerant += usize::from(overall.is_some_and(|n| n > 0));
            }
        }
        assert!(
            tolerant > 500,
            "the comparison exercised the tolerance: {tolerant}"
        );
    }

    // --- the log as it really is: fixtures cut from a measured boot -------------

    /// The kernel lines of the measured log, verbatim.
    const IPC: &[u8] = b"[ipc 1414400] caller=17 call=3 target=0x7ffe result=0\r\n";
    const KSYS: &[u8] = b"[ksys 1412800] caller=17 call=0 result=0\r\n";
    const FORK: &[u8] = b"[ksys SYS_FORK] parent=i nr=10 child_nr=19 child_e=0x8013 pages=23\r\n";

    #[test]
    fn kernel_lines_are_taken_out_of_the_console_stream() {
        // Verbatim shapes: a kernel line beginning mid console line, several
        // in a row, and the console line carrying on after them.
        let raw = [
            b"Cminix.rs init: vfs long write -----".as_slice(),
            KSYS,
            IPC,
            b"-----",
            IPC,
            b"C--vfs-loop-end\r\n",
            IPC,
            KSYS,
            b"Cminix.rs hello: iov ",
            FORK,
            IPC,
            b"CCCCCCCCCC....ok match=1\r\nminix.rs hello: errno ok\r\nt",
            b"[ksys SYS_EXIT] target=b nr=19 freed=531 devs=0 result=0\r\n",
        ]
        .concat();
        assert_eq!(
            console_stream(&raw),
            b"Cminix.rs init: vfs long write ----------C--vfs-loop-end\r\n\
              Cminix.rs hello: iov CCCCCCCCCC....ok match=1\r\nminix.rs hello: errno ok\r\nt"
        );
    }

    #[test]
    fn every_kernel_prefix_of_the_measured_boot_is_recognised() {
        for line in [
            &b"[ipc 100] caller=17 call=3 target=0x7ffe result=0\r\n"[..],
            b"[ksys 100] caller=17 call=0 result=0\r\n",
            b"[ksys SYS_EXEC] target=19 name=bigprog src=name entry=0x101000\r\n",
            b"[pf va=0x1000]\r\n",
            b"[diag vm] something\r\n",
            b"[diag tty] fb ok w=800 h=600 cols=100 rows=37\r\n",
            b"[as new]\r\n",
            b"[alarm 3]\r\n",
            b"[noq x]\r\n",
            b"[exec x]\r\n",
            b"[devmap] tty fb va=0x100000000 pa=0x4c7a0000 pages=469\r\n",
            b"[virtio x]\r\n",
            b"[dma x]\r\n",
            b"[efault deliver] x\r\n",
            b"[fb] ok pa=0x4c7a0000 w=800 h=600\r\n",
            b"[mair x]\r\n",
            b"[ramdisk x]\r\n",
            // A line end of the other kind, and none at all yet.
            b"[ipc 100] caller=17\n",
            b"[ipc 100] call",
        ] {
            let raw = [b"before ".as_slice(), line, b"after"].concat();
            let expect: &[u8] = if line.ends_with(b"\n") {
                b"before after"
            } else {
                b"before "
            };
            assert_eq!(
                console_stream(&raw),
                expect,
                "{:?}",
                String::from_utf8_lossy(line)
            );
        }
    }

    #[test]
    fn what_is_not_a_kernel_prefix_is_kept() {
        for text in [
            // The firmware's escape sequences, from the measured log.
            &b"\x1b[2J\x1b[01;01H\x1b[=3h\x1b[01;02Hi\r\n"[..],
            // No name, an upper-case name, a digit, no space or bracket after.
            b"a[] b\r\n",
            b"a[ ] b\r\n",
            b"a[OK] b\r\n",
            b"a[Ipc 1] b\r\n",
            b"a[1] b[12 3]\r\n",
            b"a[ipc1] b\r\n",
            b"a[ipc\r\n",
            b"a[ipc",
            b"a[",
            b"x = y[I] + [z, w]\r\n",
            b"no bracket at all\r\n",
            b"",
        ] {
            assert_eq!(
                console_stream(text),
                text,
                "{:?}",
                String::from_utf8_lossy(text)
            );
        }
    }

    #[test]
    fn console_text_that_looks_like_a_kernel_line_is_lost_and_its_row_refused() {
        // The price of the recogniser, pinned: a console line holding
        // `[word]` or `[word ` is taken out from there to its end, so its row
        // is not found. A false FAILURE, never a false pass.
        assert_eq!(
            console_stream(b"x = y[i] + 1\r\nnext\r\n"),
            b"x = ynext\r\n"
        );
        assert_eq!(
            console_stream(b"sh: [ok] done\r\nnext\r\n"),
            b"sh: next\r\n"
        );
        let text = [b"sh: [ok] done\n".as_slice(), EIGHT].concat();
        let (w, h) = (320, 160);
        assert_eq!(
            check(&dump(w, h, &text), &log(w, h, &text)),
            Err(CheckError::RowNotInLog {
                row: 0,
                text: "sh: [ok] done".into()
            })
        );
    }

    /// A log in the measured shape: kernel lines between every console line
    /// and wherever `|` stands in `body`, and a tick run before each line.
    fn noisy(w: u32, h: u32, body: &[u8]) -> Vec<u8> {
        let mut out = b"UEFI firmware\r\n\x1b[2J\x1b[01;01H[fb] ok w=800\r\n".to_vec();
        out.extend_from_slice(IPC);
        out.extend_from_slice(&fb_ok(w, h));
        for line in body.split_inclusive(|&b| b == b'\n') {
            out.extend_from_slice(b"tCCCCC");
            for piece in line.split_inclusive(|&b| b == b'|') {
                let (text, cut) = match piece.split_last() {
                    Some((b'|', text)) => (text, true),
                    _ => (piece, false),
                };
                // TTY's line end is CR LF.
                match text.split_last() {
                    Some((b'\n', text)) => {
                        out.extend_from_slice(text);
                        out.extend_from_slice(b"\r\n");
                    }
                    _ => out.extend_from_slice(text),
                }
                if cut {
                    out.extend_from_slice(KSYS);
                    out.extend_from_slice(IPC);
                }
            }
            out.extend_from_slice(IPC);
            out.extend_from_slice(KSYS);
        }
        out
    }

    #[test]
    fn a_console_line_interrupted_by_kernel_lines_is_found() {
        let text = [
            b"minix.rs init: vfs long write ----------vfs-loop-end\n".as_slice(),
            EIGHT,
        ]
        .concat();
        let (w, h) = (160, 320);
        let image = dump(w, h, &text);
        // Uninterrupted, but with kernel lines and tick runs between lines.
        let report = check(&image, &noisy(w, h, &text)).unwrap();
        assert_eq!(report.text[0], "minix.rs init: vfs l");
        assert_eq!(report.text[2], "vfs-loop-end");
        assert_eq!((report.wraps, report.stray, report.worst), (2, 0, 0));

        // Kernel lines in the middle of a row, at a row boundary, and twice
        // in one row: the row is split across two kernel lines.
        for cut in [
            &b"minix.rs init: vfs long wr|ite ----------vfs-loop-end\n"[..],
            b"minix.rs init: vfs l|ong write ----------vfs-loop-end\n",
            b"minix.rs i|nit: v|fs long write -----|-----vfs-loop|-end\n",
            b"|minix.rs init: vfs long write ----------vfs-loop-end|\n",
        ] {
            let body = [cut, EIGHT].concat();
            let report = check(&image, &noisy(w, h, &body)).unwrap();
            assert_eq!(
                (report.non_blank_rows, report.wraps, report.stray),
                (11, 2, 0),
                "{:?}",
                String::from_utf8_lossy(cut)
            );
        }
        // Without the stripping none of that would be found.
        let body = [
            &b"minix.rs init: vfs long wr|ite ----------vfs-loop-end\n"[..],
            EIGHT,
        ]
        .concat();
        assert!(find_stray(&noisy(w, h, &body), b"minix.rs init: vfs l").is_some());
        assert!(find_stray(&noisy(w, h, &body), b"ong write ----------").is_none());
    }

    #[test]
    fn tick_runs_after_a_kernel_line_inside_a_row_are_tolerated() {
        // Measured: "…-----[ksys …]\r\n[ipc …]\r\nCCCCC-----…vfs-loop-end".
        let text = [b"write ----------vfs-loop-end\n".as_slice(), EIGHT].concat();
        let (w, h) = (320, 160);
        let raw = noisy(w, h, &text).replace(
            b"write -----",
            &[b"write -----".as_slice(), KSYS, IPC, b"CCCCC"].concat(),
        );
        let report = check(&dump(w, h, &text), &raw).unwrap();
        assert_eq!((report.stray, report.worst), (1, 5));
    }

    #[test]
    fn the_refusals_hold_in_a_noisy_log() {
        let text = [b"minix.rs init: dev.null ok n=35\n".as_slice(), EIGHT].concat();
        let (w, h) = (320, 160);
        let image = dump(w, h, &text);
        assert!(check(&image, &noisy(w, h, &text)).is_ok());
        let refused = Err(CheckError::RowNotInLog {
            row: 0,
            text: "minix.rs init: dev.null ok n=35".into(),
        });
        for changed in [
            // A substituted byte, a missing one, a missing row.
            &b"minix.rs init: dev.null ok n=36\n"[..],
            b"minix.rs init: dev.nul ok n=35\n",
            b"minix.rs inis: dev.null ok n=35\n",
            b"",
            // A stray digit, space, or punctuation mark.
            b"minix.rs init: dev.null ok n=335\n",
            b"minix.rs init: dev.null  ok n=35\n",
            b"minix.rs init:: dev.null ok n=35\n",
            // A line end inside it.
            b"minix.rs init: dev\n.null ok n=35\n",
        ] {
            let body = [changed, EIGHT].concat();
            assert_eq!(
                check(&image, &noisy(w, h, &body)),
                refused,
                "{:?}",
                String::from_utf8_lossy(changed)
            );
        }
        // The text only ever existed inside a kernel line: not console text.
        let body = [
            b"[diag vfs] minix.rs init: dev.null ok n=35\n".as_slice(),
            EIGHT,
        ]
        .concat();
        assert_eq!(check(&image, &noisy(w, h, &body)), refused);
    }

    #[test]
    fn a_row_that_lost_letters_still_passes_which_is_the_cost_of_the_tolerance() {
        // Pinned, not wanted: the log's `ni` is taken for two tick bytes.
        let shown = [b"minix.rs it: stdin ok\n".as_slice(), EIGHT].concat();
        let written = [b"minix.rs init: stdin ok\n".as_slice(), EIGHT].concat();
        let (w, h) = (320, 160);
        let report = check(&dump(w, h, &shown), &noisy(w, h, &written)).unwrap();
        assert_eq!((report.stray, report.worst), (1, 2));
    }

    #[test]
    fn the_fb_ok_line_is_read_from_the_raw_log_wherever_it_starts() {
        // It is a kernel line: it can begin mid console line, and it is gone
        // from the console stream.
        let raw = [
            b"alpha".as_slice(),
            &fb_ok(W, H),
            b" 1\r\n",
            IPC,
            &EIGHT[8..],
        ]
        .concat();
        assert!(find(&console_stream(&raw), FB_OK).is_none());
        assert_eq!(check(&dump(W, H, EIGHT), &raw).unwrap().non_blank_rows, 8);
    }

    // --- helpers -----------------------------------------------------------------

    #[test]
    fn find_is_an_exact_byte_search() {
        assert_eq!(find(b"abcabc", b"bc"), Some(1));
        assert_eq!(find(b"abcabc", b"abcabc"), Some(0));
        assert_eq!(find(b"abcabc", b"abcabcd"), None);
        assert_eq!(
            find(b"aab", b"ab"),
            Some(1),
            "a false start is recovered from"
        );
        assert_eq!(find(b"abc", b"c"), Some(2));
        assert_eq!(find(b"abc", b"d"), None);
        assert_eq!(find(b"", b"a"), None);
        assert_eq!(find(b"abc", b""), None);
        assert_eq!(find(b"\xFF\x00\xFE", b"\x00\xFE"), Some(1));
    }

    #[test]
    fn trimmed_removes_trailing_blanks_only() {
        assert_eq!(trimmed(b"  a b  "), b"  a b");
        assert_eq!(trimmed(b"    "), b"");
        assert_eq!(trimmed(b""), b"");
        assert_eq!(trimmed(b"x"), b"x");
    }

    #[test]
    fn a_canvas_round_trips_through_a_ppm() {
        let c = canvas(W, H, EIGHT);
        let f = fmt(W, H);
        assert_eq!(Canvas::from_ppm(&dump_of(&c), &f), c);
        assert_eq!(c.get(W, 0), None);
        assert_eq!(c.get(0, H), None);
        assert_eq!(COLS, f.cols());
    }

    #[test]
    fn every_error_names_what_was_found() {
        for (error, needles) in [
            (CheckError::NoGeometryLine, &["fb ok"][..]),
            (CheckError::BadGeometryLine("w=8".into()), &["w=8"]),
            (
                CheckError::SizeMismatch {
                    dump: (640, 480),
                    log: (800, 600),
                },
                &["640x480", "800x600"],
            ),
            (CheckError::Format(FormatError::Pitch), &["pitch"]),
            (
                CheckError::GridMismatch {
                    derived: (100, 37),
                    log: (99, 36),
                },
                &["100x37", "cols=99 rows=36"],
            ),
            (
                CheckError::Margin {
                    x: 3,
                    y: 599,
                    rgb: 0xAAAAAA,
                },
                &["(3, 599)", "#aaaaaa"],
            ),
            (
                CheckError::Rerender {
                    x: 1,
                    y: 2,
                    dump: 0xAAAAAA,
                    painted: 0,
                },
                &["(1, 2)", "#aaaaaa", "#000000"],
            ),
            (CheckError::TooFewRows { found: 3 }, &["3", "8"]),
        ] {
            let text = error.to_string();
            for needle in needles {
                assert!(text.contains(needle), "{text:?} lacks {needle:?}");
            }
        }
    }
}
