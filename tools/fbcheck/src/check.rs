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
//! finds its framebuffer. The **last** occurrence in the log is the one used:
//! a log holds one per TTY start, and the last TTY to start is the one whose
//! screen can be dumped. The line must be complete — all four numbers,
//! nothing after them, and a line end (`\n` or `\r`) — or the check fails
//! rather than read half a number from a line still being written.
//!
//! It is read exactly, with no allowance for stray bytes, because none can be
//! in it: the kernel writes the whole of a `[diag …]` line inside one kernel
//! call (`diag_print` in `kernel/src/system/do_diagctl.rs` — the prefix, the
//! text and the line end, with nothing able to run in between), like every
//! other kernel line.
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
//! ## 3. The text is the boot's, and it is where the boot put it
//!
//! At least [`MIN_ROWS`] rows must be non-blank, and **the screen, read top
//! to bottom, must be one unbroken stretch of what the console wrote**: every
//! row where the row above it leaves off, a line ending where the row ends
//! short of the last column, a line wrapping where it does not.
//!
//! ### What the serial log really is
//!
//! Measured on fed boots (slice 6.7; 54 MB of log for a default disk boot, of
//! which some 60 KB is this console): the serial line has three writers, and
//! only one is the console.
//!
//! * **TTY**, writing the console's bytes; its lines end `\r\n`. TTY writes
//!   each byte to the UART before it draws it (F9), and its first bytes come
//!   after its own `fb ok` line.
//! * **The kernel, a line at a time.** `[ipc 1414500] caller=17 …\r\n`,
//!   `[ksys SYS_FORK] …`, `[diag vfs] …`, `[pf …`, `[as …`, `[devmap …` and
//!   the rest. Each is written whole — nothing lands inside one — but it can
//!   begin **in the middle of a console line**, which carries on after it:
//!   `…-----[ksys 1412800] caller=17 call=0 result=0\r\n-----…`. A million of
//!   them in that boot.
//! * **The kernel, a byte at a time.** One byte per 10 ms of guest time while
//!   a process is running: the first byte of its name
//!   (`kernel/src/clock.rs`). They come singly (`minix.rs initt: dev.null
//!   ok`) and, when nothing else prints between ticks, in **runs**
//!   (`minix.rs hello: iov CCCCCCCCCC....`), anywhere: inside a console line,
//!   and — thousands at a time — between two of them.
//!
//! ### The console stream
//!
//! The log is cut at the `fb ok` line step 1 used — nothing before it was
//! ever drawn by this TTY — and **the kernel's lines are taken out of the
//! rest** ([`console_stream`]). A kernel line starts at a `[` followed by one
//! or more of `a`–`z`, `_`, `.`, `-` and then a space or a `]` — `[ipc `,
//! `[ksys `, `[diag tty]`, `[efault ` — and runs through the next `\n`, or to
//! the end of the log if it has none yet. Everything else is kept, in order.
//! What is left is the console's bytes with the tick bytes still among them.
//!
//! A **stray byte** is what a tick prints: an ASCII letter, or the `?` the
//! kernel prints for a process without a name. Never a digit, a space,
//! punctuation or a line end.
//!
//! ### The rule
//!
//! Each row is taken as its text — its cells with the trailing blanks
//! removed. Walking the rows from the top down to the last non-blank one, the
//! stream must hold, in this order and with nothing else between:
//!
//! * **the row's text**, byte for byte, with stray bytes allowed between two
//!   of its bytes, in runs of any length, [`MAX_STRAY`] in total for the row;
//! * then **up to the row's width in blanks**, one directly after another —
//!   a line may really end in spaces, which a screen cannot show;
//! * then **a line end** (`\n`, or `\r` and `\n`), after which the next row
//!   starts a new line — or, only if the text and blanks reached the last
//!   column, **nothing**: the line wrapped, and the next row's text carries
//!   straight on. (A line of exactly the screen's width followed by a newline
//!   takes one row, not two — F8's deferred wrap — so a full row may be
//!   followed by either.)
//!
//! A blank row between two others is thus an empty line (or one of blanks).
//! Stray bytes may also sit, in any number, *between* those pieces: before a
//! row's first byte, after its last, and between a line end's `\r` and
//! `\n`. Those are not counted against [`MAX_STRAY`], which is about what is
//! inside a row; the runs between lines reach thousands. They are bounded by
//! what they are: a run ends at the first byte that is not a letter or `?`.
//! The one place between pieces where none is allowed is after a trailing
//! blank: a blank is followed by another blank or by the line end, or else a
//! word lost after a space (`errno` for `errno ok`) would read as a blank and
//! two tick bytes. No console line of the measured boots ends in a blank.
//!
//! Two rows are treated specially, and only these two:
//!
//! * **The last non-blank row** needs only its text to be there. It may be
//!   the cursor's row, a line still being written — and the log is read after
//!   the dump is taken, so the log may hold more of that line, and more
//!   lines, than the screen did. What follows its text is not looked at.
//! * **The top row** decides whether the screen has scrolled, below.
//!
//! Every way of reading the stream is followed at once — which byte is the
//! row's and which a stray, whether a full row wrapped or ended — so a match
//! cannot be missed by committing early.
//!
//! ### Scrolled, or not yet
//!
//! * **Not scrolled:** the top row begins at the very start of the console
//!   stream. Row 0 is the first thing TTY wrote (its banner), and everything
//!   since is on the screen in order. [`Report::scrolled`] is `false`.
//! * **Scrolled:** the walk works from somewhere later in the stream — the
//!   top row begins another line, or is the tail of a line whose head has
//!   scrolled away — **and** the screen is full: its last non-blank row is
//!   the last row or the one above it (after a scroll the cursor is on the
//!   last row, and every row above it has been written). A screen that
//!   starts later in the stream and is *not* full is refused: that is what a
//!   scroll that cleared the screen instead of moving it would look like.
//!
//! The first reading is tried first, so a screen that could be read either
//! way is reported as not scrolled. Neither needs TTY's cooperation, and a
//! screen cannot fake the second by not drawing its first rows: with the
//! first line missing, row 0 would have to be a line of the stream, and what
//! the stream has there is the line that was not drawn.
//!
//! Why it matters: until the screen scrolls, nothing has exercised the scroll
//! path — the grid shifting and the screen being repainted from it. A caller
//! that wants that proved asks for a scrolled screen (`--scrolled`).
//!
//! ### The cap on strays, and what it costs
//!
//! [`MAX_STRAY`] is 120. The longest run measured *inside* a console line was
//! 30, on a fast host under hardware acceleration; ticks are counted in guest
//! time, so a slower host — CI's — writes proportionally more of them into
//! the same pause, and a cap near the measurement would refuse an honest
//! screen there. [`Report::stray`] is the number of rows that needed any and
//! [`Report::worst`] the most one row needed — the figure to watch.
//!
//! The price of allowing them at all: **a row that has lost letters still
//! passes.** `minix.rs it: stdin ok` is accepted for the log's `minix.rs
//! init: stdin ok`, with `ni` taken for tick bytes — up to [`MAX_STRAY`]
//! letters inside a row, and any number of letters at the very start of a
//! line (`x.rs init: …`, with `mini` taken for a run before the line) or at
//! its very end (`… stdio o` for `… stdio ok`) — so a line made of nothing
//! but letters can be shown as a blank row. Measured runs before a line are
//! of any letters in any order (`vp`, `tCCCCC`), so no shape tells them from
//! text.
//!
//! ### What step 3 does not see
//!
//! * **Lost letters**, as above. A lost digit, space or punctuation mark is
//!   seen; so is any wrong, extra or moved character.
//! * **The end of the last non-blank row.** It is matched as a prefix, so a
//!   last row cut short passes.
//! * **The head of a scrolled screen's top row.** It may begin anywhere in a
//!   line, so a top row that has lost its first characters passes — and so
//!   does a wrapped line that begins there and has lost characters anywhere
//!   along it, since the whole of it can be read as starting later.
//! * **A frozen screen.** A screen that stopped being drawn some time ago is
//!   still an unbroken stretch of the stream. Nothing here compares the
//!   screen's last row with the stream's end.
//! * **Console text that looks like a kernel line.** A console line holding
//!   `[word]` or `[word ` is taken out from there to its end, and the screen
//!   showing it is refused: a false failure, never a false pass. Nothing the
//!   boot prints to the console does.
//! * **Bytes the screen draws differently.** A tab is drawn as a move of the
//!   cursor, an unprintable byte as `?` (F8), and a `\r` not followed by a
//!   `\n` returns to the start of the row; a screen made from any of them is
//!   refused. Nothing in the boot writes one to the console.
//!
//! Row order, a repeated row, a row cut short and a row shifted left are all
//! seen: each breaks the stretch.

use std::collections::BTreeMap;
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
/// row's text in the log. Four times the longest run measured on a fast
/// host; see the module docs.
pub const MAX_STRAY: usize = 120;

// The matcher keeps one bit per stray count in a `u128`.
const _: () = assert!(MAX_STRAY < 128);

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

/// A screen that is the boot's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The geometry the log and the dump agree on.
    pub geometry: Geometry,
    /// Rows holding something other than blanks.
    pub non_blank_rows: usize,
    /// Has the screen scrolled? `false`: its top row is the first thing the
    /// console wrote. `true`: it begins later, and the screen is full.
    pub scrolled: bool,
    /// Rows that reached the last column and whose line carried straight on
    /// in the row below: wrapped lines.
    pub wraps: usize,
    /// Rows that matched only by allowing stray bytes inside them.
    pub stray: usize,
    /// The most stray bytes any one row needed; at most [`MAX_STRAY`].
    pub worst: usize,
    /// Every row's text, top to bottom, trailing blanks removed.
    pub text: Vec<String>,
}

impl Report {
    /// The verdict's fields, without its first word:
    /// `800x600 100x37 rows=<non-blank> wraps=<n> stray=<n> worst=<n>`.
    pub fn fields(&self) -> String {
        let g = &self.geometry;
        format!(
            "{}x{} {}x{} rows={} wraps={} stray={} worst={}",
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

impl fmt::Display for Report {
    /// The one-line verdict: `ok <fields> scrolled=yes` or `… scrolled=no`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scrolled = if self.scrolled { "yes" } else { "no" };
        write!(f, "ok {} scrolled={scrolled}", self.fields())
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
    /// Step 3: a row's text is nowhere in the console stream, stray bytes
    /// allowed for.
    RowNotInLog { row: usize, text: String },
    /// Step 3: a row's text is in the console stream, but not where the
    /// screen has it: not after the row above it.
    RowMisplaced { row: usize, text: String },
    /// Step 3: a row's text is where the screen has it, but what follows it
    /// in the stream is neither the end of its line nor a wrap into the next
    /// row.
    RowEndsEarly { row: usize, text: String },
    /// Step 3: the screen reads as a later stretch of the stream — it has
    /// lost the console's first line — but is not full. `last` is its last
    /// non-blank row.
    NotFull { last: usize, rows: usize },
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
            Self::RowNotInLog { row, text } => write!(
                f,
                "screen row {row} is not in the serial log's console text, even allowing \
                 {MAX_STRAY} tick bytes inside it: {text:?}"
            ),
            Self::RowMisplaced { row, text } => write!(
                f,
                "screen row {row} is in the serial log's console text, but not where the screen \
                 has it — it does not follow the row above it there (shifted, repeated, out of \
                 order, or left over from before a scroll): {text:?}"
            ),
            Self::RowEndsEarly { row, text } => write!(
                f,
                "screen row {row} stops where the serial log's console text goes on — neither \
                 the end of a line nor a wrap into the next row follows it there (cut short, or \
                 the row below is not its continuation): {text:?}"
            ),
            Self::NotFull { last, rows } => write!(
                f,
                "the screen does not begin with the console's first line, yet it is not full: \
                 its last non-blank row is {last} of {rows} (a scrolled screen is written down \
                 to its last rows)"
            ),
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
///
/// `Ok` is a screen that is the boot's; whether it has scrolled yet is
/// [`Report::scrolled`], and a caller that needs a scrolled one checks it.
pub fn check(image: &Ppm, log: &[u8]) -> Result<Report, CheckError> {
    // Step 1.
    let (geometry, at) = fb_ok_line(log)?;
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

    // Step 3. The stream starts AT the `fb ok` line, which `console_stream`
    // then takes out with the other kernel lines: what is left is what this
    // TTY wrote, from its first byte.
    let console = console_stream(log.get(at..).unwrap_or(&[]));
    let placed = place(&console, &rows, fmt.cols())?;
    Ok(Report {
        geometry,
        non_blank_rows: placed.non_blank,
        scrolled: placed.scrolled,
        wraps: placed.tally.wraps,
        stray: placed.tally.stray,
        worst: placed.tally.worst,
        text: rows.iter().map(|row| lossy(trimmed(row))).collect(),
    })
}

/// Step 1's reading of the log: the last `[diag tty] fb ok` line.
pub fn geometry_from_log(log: &[u8]) -> Result<Geometry, CheckError> {
    fb_ok_line(log).map(|(geometry, _)| geometry)
}

/// The last `[diag tty] fb ok` line: what it says, and where in `log` it
/// starts.
///
/// Read exactly. It is a kernel line, written whole inside one kernel call
/// (the module docs say where), so no tick byte can be in it and none is
/// allowed for. It must have its line end (`\n` or `\r`): without one it may
/// still be being written, and "rows=10" the start of "rows=100".
fn fb_ok_line(log: &[u8]) -> Result<(Geometry, usize), CheckError> {
    let at = rfind(log, FB_OK).ok_or(CheckError::NoGeometryLine)?;
    let from = log.get(at..).unwrap_or(&[]);
    let end = from.iter().position(|&b| b == b'\n' || b == b'\r');
    let line = end.and_then(|end| from.get(..end));
    let bad = || CheckError::BadGeometryLine(lossy(line.unwrap_or(from)));
    let line = line.ok_or_else(bad)?;
    let geometry = line
        .strip_prefix(FB_OK)
        .and_then(parse_geometry)
        .ok_or_else(bad)?;
    Ok((geometry, at))
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

/// ` w=<w> h=<h> cols=<c> rows=<r>`, and nothing after it.
fn parse_geometry(mut rest: &[u8]) -> Option<Geometry> {
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

/// May `byte` be one of the kernel's tick bytes? A letter — the first byte of
/// a process's name — or the `?` printed for a process without one.
const fn may_be_stray(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'?'
}

/// What one reading of the screen cost. Ordered so that the smallest is the
/// reading to report: the fewest strays in its worst row first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Tally {
    /// The most stray bytes inside any one row.
    worst: usize,
    /// Rows with any stray byte inside them.
    stray: usize,
    /// Rows whose line carried on in the row below.
    wraps: usize,
}

/// How the screen sits in the console stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placed {
    non_blank: usize,
    scrolled: bool,
    tally: Tally,
}

/// Where a walk down the screen stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// This row's text is not where the row above left off.
    Text(usize),
    /// This row's text is there, but neither a line end nor a wrap follows.
    End(usize),
}

impl Stop {
    /// How far the walk got: later rows first, and at one row, having found
    /// its text counts for more than not.
    const fn progress(self) -> (usize, u8) {
        match self {
            Self::Text(row) => (row, 0),
            Self::End(row) => (row, 1),
        }
    }
}

/// Where the top row may begin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Top {
    /// At the start of the stream: the screen has not scrolled.
    Start,
    /// Anywhere after that.
    Later,
}

/// Positions in the stream reached so far, each with the cheapest way there.
type Reached = BTreeMap<usize, Tally>;

fn reach_cheapest(reached: &mut Reached, at: usize, tally: Tally) {
    reached
        .entry(at)
        .and_modify(|known| *known = (*known).min(tally))
        .or_insert(tally);
}

/// The console stream, and for each position where the run of stray bytes
/// starting there ends.
struct Stream<'a> {
    bytes: &'a [u8],
    /// `run_end[p]`: the first position at or after `p` that does not hold a
    /// stray byte (`bytes.len()` if there is none).
    run_end: Vec<usize>,
}

impl<'a> Stream<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        let mut run_end = vec![bytes.len(); bytes.len() + 1];
        for (p, &byte) in bytes.iter().enumerate().rev() {
            if !may_be_stray(byte) {
                run_end[p] = p;
            } else if let Some(&next) = run_end.get(p + 1) {
                run_end[p] = next;
            }
        }
        Self { bytes, run_end }
    }

    /// The first position at or after `at` that is not a stray byte.
    fn past_strays(&self, at: usize) -> usize {
        self.run_end.get(at).copied().unwrap_or(self.bytes.len())
    }

    /// If, strays aside, `byte` is next at `at`: the position after it.
    fn then(&self, at: usize, byte: u8) -> Option<usize> {
        let at = self.past_strays(at);
        (self.bytes.get(at) == Some(&byte)).then_some(at + 1)
    }

    /// If, strays aside, a line end is next at `at` — `\n`, or `\r` and
    /// `\n` — the position after it.
    fn line_end(&self, at: usize) -> Option<usize> {
        self.then(at, b'\n')
            .or_else(|| self.then(self.then(at, b'\r')?, b'\n'))
    }

    /// If `byte` is at `at` itself, no strays allowed: the position after it.
    fn here(&self, at: usize, byte: u8) -> Option<usize> {
        (self.bytes.get(at) == Some(&byte)).then_some(at + 1)
    }

    /// If a line end is at `at` itself: the position after it. (Strays may
    /// still sit between its `\r` and its `\n`.)
    fn line_end_here(&self, at: usize) -> Option<usize> {
        self.here(at, b'\n')
            .or_else(|| self.then(self.here(at, b'\r')?, b'\n'))
    }

    /// Where a row's text beginning with `first` may start, given that the
    /// row above left off at `at`: `first` itself, after any strays.
    fn starts(&self, at: usize, first: u8) -> impl Iterator<Item = usize> + '_ {
        // A first byte that could itself be a stray may be any one of a run
        // of them; one that could not is the byte the run ends at.
        let end = self.past_strays(at);
        let from = if may_be_stray(first) { at } else { end };
        (from..=end).filter(move |&p| self.bytes.get(p) == Some(&first))
    }
}

/// Walk the rows `texts` (trailing blanks removed; the last one non-blank)
/// down the stream and return the cheapest reading, or where every reading
/// stopped.
fn walk(stream: &Stream, texts: &[&[u8]], cols: usize, top: Top) -> Result<Tally, Stop> {
    // Where the row about to be read may begin, strays aside.
    let mut gaps = Reached::new();
    let first_row = texts.first().copied().unwrap_or(&[]);
    // The start of the stream, and the strays that may precede its first
    // line: where a screen that has not scrolled begins.
    let head = stream.past_strays(0);
    match (top, first_row.first()) {
        (Top::Start, _) => {
            gaps.insert(0, Tally::default());
        }
        // A blank top row of a scrolled screen is an empty line: it begins
        // where some line begins.
        (Top::Later, None) => {
            for (p, _) in stream
                .bytes
                .iter()
                .enumerate()
                .filter(|&(_, &b)| b == b'\n')
            {
                gaps.insert(p + 1, Tally::default());
            }
        }
        // Any other may begin anywhere — it may be the tail of a line whose
        // head has scrolled away — except at the stream's start.
        (Top::Later, Some(_)) => {}
    }

    for (row, &text) in texts.iter().enumerate() {
        // The row's text.
        let mut ends = Reached::new();
        match text.split_first() {
            None => ends = std::mem::take(&mut gaps),
            Some((&first, tail)) => {
                let mut from: Vec<(usize, Tally)> = Vec::new();
                if row == 0 && top == Top::Later {
                    from.extend(
                        (head + 1..stream.bytes.len())
                            .filter(|&p| stream.bytes.get(p) == Some(&first))
                            .map(|p| (p, Tally::default())),
                    );
                } else {
                    for (&at, &tally) in &gaps {
                        from.extend(stream.starts(at, first).map(|p| (p, tally)));
                    }
                }
                for (start, tally) in from {
                    let mut ways = reach(stream.bytes, start, tail, MAX_STRAY);
                    while ways != 0 {
                        let strays = ways.trailing_zeros() as usize;
                        ways &= ways - 1;
                        let tally = Tally {
                            worst: tally.worst.max(strays),
                            stray: tally.stray + usize::from(strays > 0),
                            wraps: tally.wraps,
                        };
                        reach_cheapest(&mut ends, start + text.len() + strays, tally);
                    }
                }
            }
        }
        if ends.is_empty() {
            return Err(Stop::Text(row));
        }
        // The last non-blank row: its text is all that is asked of it.
        if row + 1 == texts.len() {
            return ends.values().copied().min().ok_or(Stop::Text(row));
        }

        // What follows it: blanks up to the last column, then a line end —
        // or, at the last column only, the next row straight away. Strays
        // may follow the text; once a blank has been read, only blanks and
        // the line end may, with nothing between — or a word lost after a
        // space ("errno" for "errno ok") would read as a blank and a run of
        // tick bytes.
        let blanks = cols.saturating_sub(text.len());
        gaps = Reached::new();
        for (&end, &tally) in &ends {
            let mut at = end;
            for seen in 0..=blanks {
                let line_end = if seen == 0 {
                    stream.line_end(at)
                } else {
                    stream.line_end_here(at)
                };
                if let Some(next) = line_end {
                    reach_cheapest(&mut gaps, next, tally);
                }
                if seen == blanks {
                    let wrapped = Tally {
                        wraps: tally.wraps + 1,
                        ..tally
                    };
                    reach_cheapest(&mut gaps, at, wrapped);
                    break;
                }
                let blank = if seen == 0 {
                    stream.then(at, BLANK)
                } else {
                    stream.here(at, BLANK)
                };
                match blank {
                    Some(next) => at = next,
                    None => break,
                }
            }
        }
        if gaps.is_empty() {
            return Err(Stop::End(row));
        }
    }
    // `texts` is never empty; an empty walk reads nothing.
    Err(Stop::Text(0))
}

/// Step 3: place the screen in the console stream.
fn place(console: &[u8], rows: &[Vec<u8>], cols: usize) -> Result<Placed, CheckError> {
    let texts: Vec<&[u8]> = rows.iter().map(|row| trimmed(row)).collect();
    let non_blank = texts.iter().filter(|text| !text.is_empty()).count();
    let Some(last) = texts.iter().rposition(|text| !text.is_empty()) else {
        return Err(CheckError::TooFewRows { found: 0 });
    };
    let shown = texts.get(..=last).unwrap_or(&texts);
    let stream = Stream::new(console);

    let placed = match walk(&stream, shown, cols, Top::Start) {
        Ok(tally) => Placed {
            non_blank,
            scrolled: false,
            tally,
        },
        Err(from_start) => match walk(&stream, shown, cols, Top::Later) {
            // After a scroll the cursor is on the last row and every row
            // above it has been written.
            Ok(_) if last + 2 < rows.len() => {
                return Err(CheckError::NotFull {
                    last,
                    rows: rows.len(),
                });
            }
            Ok(tally) => Placed {
                non_blank,
                scrolled: true,
                tally,
            },
            Err(later) => {
                // Say where the reading that got furthest stopped.
                let stop = if later.progress() >= from_start.progress() {
                    later
                } else {
                    from_start
                };
                return Err(refusal(console, &texts, stop));
            }
        },
    };
    if non_blank < MIN_ROWS {
        return Err(CheckError::TooFewRows { found: non_blank });
    }
    Ok(placed)
}

/// Turn where a walk stopped into what to tell a person.
fn refusal(console: &[u8], texts: &[&[u8]], stop: Stop) -> CheckError {
    let text_of = |row: usize| lossy(texts.get(row).copied().unwrap_or(&[]));
    match stop {
        Stop::End(row) => CheckError::RowEndsEarly {
            row,
            text: text_of(row),
        },
        Stop::Text(row) => {
            let text = texts.get(row).copied().unwrap_or(&[]);
            // A blank row "is" anywhere; only its place can be wrong.
            if text.is_empty() || find_stray(console, text).is_some() {
                CheckError::RowMisplaced {
                    row,
                    text: text_of(row),
                }
            } else {
                CheckError::RowNotInLog {
                    row,
                    text: text_of(row),
                }
            }
        }
    }
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

/// Where `needle` last occurs in `hay`.
fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    hay.windows(needle.len()).rposition(|w| w == needle)
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

/// Find `needle` anywhere in `hay`, allowing runs of stray bytes between its
/// bytes, [`MAX_STRAY`] in total. An exact occurrence is preferred; failing
/// that, the occurrence needing the fewest strays, and of those the leftmost.
///
/// Used only to word a refusal: is a row's text nowhere, or merely not where
/// the screen has it?
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
        let ways = reach(hay, start, tail, limit);
        if ways != 0 {
            let strays = ways.trailing_zeros() as usize;
            best = Some(Found {
                start,
                end: start + needle.len() + strays,
                strays,
            });
        }
    }
    best
}

/// Every number of stray bytes, up to `limit`, with which `tail` can follow
/// the byte at `hay[start]`: bit `k` is set if it can with exactly `k`.
///
/// While reading, bit `k` of `ways` says the needle's bytes so far can be
/// laid on `hay` from `start` with exactly `k` strays among them, the latest
/// at `start + j + k`. Every way of placing the strays is followed at once,
/// which a matcher that commits to the first byte that fits cannot do.
fn reach(hay: &[u8], start: usize, tail: &[u8], limit: usize) -> u128 {
    let mut ways: u128 = 1;
    for (j, &want) in (1..).zip(tail) {
        let mut next: u128 = 0;
        let mut from = ways;
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
                if strays >= limit || !may_be_stray(byte) {
                    break;
                }
                strays += 1;
            }
        }
        ways = next;
        if ways == 0 {
            break;
        }
    }
    ways
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
            "ok 85x168 10x10 rows=8 wraps=0 stray=0 worst=0 scrolled=no"
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
            "ok 800x600 100x37 rows=36 wraps=0 stray=0 worst=0 scrolled=yes"
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
            "screen row 3 is not in the serial log's console text, even allowing 120 tick bytes inside it: \"delta\""
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
    fn what_came_before_the_fb_ok_line_does_not_matter_and_what_comes_after_does() {
        // Before it: firmware text, not UTF-8, NULs, and console-looking text
        // of an earlier TTY. None of it was drawn by this one.
        let mut bytes = b"\xFF\xFE\x00UEFI firmware\r\n\x1b[2Jalpha 0\r\nzulu\r\n".to_vec();
        bytes.extend_from_slice(b"[diag tty] fb ok w=85 h=168 cols=10 rows=10\r\n");
        bytes.extend_from_slice(&EIGHT.replace(b"\n", b"\r\n"));
        assert!(std::str::from_utf8(&bytes).is_err());
        let report = check(&dump(W, H, EIGHT), &bytes).unwrap();
        assert_eq!((report.non_blank_rows, report.scrolled), (8, false));

        // After it, a byte that is neither console text nor a tick byte
        // breaks the stretch: the row after it does not follow the row above.
        for junk in [&b"\xFF"[..], b"\x00", b"7", b"-", b"zulu 9\n"] {
            let body = EIGHT.replace(b"charlie 3", &[junk, b"charlie 3"].concat());
            assert_eq!(
                check(&dump(W, H, EIGHT), &log(W, H, &body)),
                Err(CheckError::RowMisplaced {
                    row: 2,
                    text: "charlie 3".into()
                }),
                "{junk:?}"
            );
        }
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
            "ok 320x160 40x10 rows=9 wraps=0 stray=1 worst=1 scrolled=no"
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
        assert_eq!(MAX_STRAY, 120);
        let text = [b"hello: iov ...ok\n".as_slice(), EIGHT].concat();
        let (w, h) = (320, 160);
        let image = dump(w, h, &text);
        let with_run = |n: usize| {
            let run = [b"iov ".as_slice(), &vec![b'C'; n], b"..."].concat();
            log(w, h, &text.replace(b"iov ...", &run))
        };
        // Exactly the cap passes, and says so.
        let report = check(&image, &with_run(MAX_STRAY)).unwrap();
        assert_eq!((report.stray, report.worst), (1, MAX_STRAY));
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
        // The cap is on what is INSIDE a row. A run before the row, however
        // long, is not.
        let body = [vec![b'C'; 40 * MAX_STRAY], text.clone()].concat();
        let report = check(&image, &log(w, h, &body)).unwrap();
        assert_eq!((report.stray, report.worst), (0, 0));
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
    fn no_tick_byte_is_allowed_for_in_the_fb_ok_line() {
        // The kernel writes the line inside one kernel call; nothing can land
        // in it, so a line with something in it is not the line.
        let image = dump(W, H, EIGHT);
        for line in [
            &b"[diag tty] fb ok w=85 h=1T68 cols=10 rows=10\n"[..],
            b"[diag tty] fb okk w=85 h=168 cols=10 rows=10\n",
            b"[diag tty] fb ok w=85 h=168 cols=10 rows=10V\n",
            b"[diag tty] fb ok w=85 h=168 cSols=10 rows=10\r\n",
        ] {
            let result = check(&image, &[line, EIGHT].concat());
            assert!(
                matches!(result, Err(CheckError::BadGeometryLine(_))),
                "{:?} gave {result:?}",
                String::from_utf8_lossy(line)
            );
        }
        // Inside the marker itself, it is not found at all.
        for line in [
            &b"[diag ttyy] fb ok w=85 h=168 cols=10 rows=10\n"[..],
            b"[diag tty] fbC ok w=85 h=168 cols=10 rows=10\n",
        ] {
            assert_eq!(
                check(&image, &[line, EIGHT].concat()),
                Err(CheckError::NoGeometryLine)
            );
        }
        // Before the line is not in it.
        let line = b"S[diag tty] fb ok w=85 h=168 cols=10 rows=10\n";
        assert!(check(&image, &[&line[..], EIGHT].concat()).is_ok());
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
        /// Every number of deletions that turns a stretch starting at
        /// `start` into the needle, as a bitmask.
        fn slow(hay: &[u8], start: usize, needle: &[u8]) -> u128 {
            let mut ways = 0;
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
                    if legal && kept == needle {
                        ways |= 1 << mask.count_ones();
                    }
                }
            }
            ways
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
                            reach(hay, start, &needle[1..], limit),
                            want & ((1 << (limit + 1)) - 1),
                            "needle {:?} in {:?} from {start}, limit {limit}",
                            String::from_utf8_lossy(needle),
                            String::from_utf8_lossy(hay)
                        );
                    }
                    if want != 0 {
                        let n = want.trailing_zeros() as usize;
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
    fn the_console_stream_starts_at_the_fb_ok_line() {
        // The line is a kernel line: it can begin anywhere, and it is not
        // itself in the stream.
        let raw = [b"firmware\x1b[2J".as_slice(), &fb_ok(W, H), IPC, EIGHT].concat();
        assert!(find(&console_stream(&raw), FB_OK).is_none());
        let report = check(&dump(W, H, EIGHT), &raw).unwrap();
        assert_eq!((report.non_blank_rows, report.scrolled), (8, false));

        // Text written before it was not drawn by this TTY: a screen showing
        // it is showing something this console never wrote.
        let raw = [b"alpha 1\n".as_slice(), &fb_ok(W, H), &EIGHT[8..]].concat();
        assert_eq!(
            check(&dump(W, H, EIGHT), &raw),
            Err(CheckError::RowNotInLog {
                row: 0,
                text: "alpha 1".into()
            })
        );
    }

    // --- scrolled, or not yet -----------------------------------------------------

    /// A 10 × 10 screen showing exactly `rows`, whatever a console would have
    /// drawn: for screens no honest TTY produces.
    fn dump_rows(rows: &[&[u8]]) -> Ppm {
        let f = fmt(W, H);
        let mut grid = Grid::new(f.cols(), f.rows());
        for r in 0..f.rows() {
            let text = rows.get(r).copied().unwrap_or(b"");
            assert!(text.len() <= COLS);
            for c in 0..f.cols() {
                grid.put(text.get(c).copied().unwrap_or(b' '));
            }
        }
        dump_of(&render(&f, &grid))
    }

    /// Twelve numbered lines, `"1: one\n"` to `"12: twelve\n"`.
    const TWELVE: &[u8] = b"1: one\n2: two\n3: three\n4: four\n5: five\n6: six\n7: seven\n8: eight\n9: nine\n10: ten\n11: eleven\n12: twelve\n";

    fn lines(text: &[u8]) -> Vec<&[u8]> {
        text.split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
            .collect()
    }

    #[test]
    fn a_screen_that_begins_with_the_consoles_first_line_has_not_scrolled() {
        assert!(!check_text(EIGHT).unwrap().scrolled);
        // Nine lines: the cursor is on the last row, and nothing has moved.
        let nine = &TWELVE[..TWELVE.len() - 32];
        assert_eq!(lines(nine).len(), 9);
        let report = check_text(nine).unwrap();
        assert_eq!((report.non_blank_rows, report.scrolled), (9, false));
        // Ten lines and no newline after the tenth: every row is written and
        // still nothing has scrolled. Full is not scrolled.
        let ten = &TWELVE[..TWELVE.len() - 23];
        assert!(ten.ends_with(b"10: ten"));
        let report = check_text(ten).unwrap();
        assert_eq!((report.non_blank_rows, report.scrolled), (10, false));
        assert!(report.to_string().ends_with(" scrolled=no"));
    }

    #[test]
    fn a_screen_whose_first_line_has_scrolled_away_is_scrolled() {
        // The newline after the tenth line scrolls once.
        let ten = &TWELVE[..TWELVE.len() - 22];
        assert!(ten.ends_with(b"10: ten\n"));
        let report = check_text(ten).unwrap();
        assert_eq!(report.text[0], "2: two");
        assert_eq!(report.text[8], "10: ten");
        assert_eq!(report.text[9], "", "the cursor's row");
        assert!(report.scrolled);
        assert!(report.to_string().ends_with(" scrolled=yes"));
        // And on: twelve lines, three scrolls.
        let report = check_text(TWELVE).unwrap();
        assert_eq!(report.text[0], "4: four");
        assert!(report.scrolled);
        // A line still being written on the last row of a scrolled screen.
        let text = [TWELVE, b"13: thir"].concat();
        let report = check_text(&text).unwrap();
        assert_eq!(
            (report.text[9].as_str(), report.scrolled),
            ("13: thir", true)
        );
    }

    #[test]
    fn a_screen_that_reads_both_ways_is_reported_as_not_scrolled() {
        // The console wrote the same ten lines twice; the screen shows the
        // second ten, having scrolled, and cannot be told from the first.
        let ten = &TWELVE[..TWELVE.len() - 23];
        let twice = [ten, b"\n", ten].concat();
        let report = check(&dump(W, H, &twice), &log(W, H, &twice)).unwrap();
        assert_eq!(report.text[0], "1: one");
        assert!(!report.scrolled, "the cautious answer");
    }

    #[test]
    fn a_screen_that_never_drew_its_first_rows_cannot_pass_as_scrolled() {
        let all = lines(TWELVE);
        let honest: Vec<&[u8]> = all[..10].to_vec();
        let body = &TWELVE[..TWELVE.len() - 23];
        assert!(
            !check(&dump_rows(&honest), &log(W, H, body))
                .unwrap()
                .scrolled
        );

        // Row 0 never drawn: rows 1.. are the console's lines 2.., and row 0
        // is no line of the stream at that place.
        let mut rows = honest.clone();
        rows[0] = b"";
        assert_eq!(
            check(&dump_rows(&rows), &log(W, H, body)),
            Err(CheckError::RowEndsEarly {
                row: 0,
                text: String::new()
            })
        );
        // Rows 0 and 1 never drawn.
        rows[1] = b"";
        assert!(check(&dump_rows(&rows), &log(W, H, body)).is_err());
        // The first line drawn on row 0, but it is the console's second.
        let mut rows = honest.clone();
        rows.remove(0);
        // Nine rows from line 2: a later stretch, and not a full screen...
        rows.pop();
        assert_eq!(
            check(&dump_rows(&rows), &log(W, H, body)),
            Err(CheckError::NotFull { last: 7, rows: 10 })
        );
    }

    #[test]
    fn a_later_stretch_on_a_screen_that_is_not_full_is_refused() {
        // What a "scroll" that cleared the screen and went back to the top
        // would leave: the newest lines, in order, from row 0, and blank
        // rows below them.
        let all = lines(TWELVE);
        for shown in 1..=8 {
            let rows: Vec<&[u8]> = all[3..3 + shown].to_vec();
            assert_eq!(
                check(&dump_rows(&rows), &log(W, H, TWELVE)),
                Err(CheckError::NotFull {
                    last: shown - 1,
                    rows: 10
                }),
                "{shown} rows"
            );
        }
        // Written down to the last row but one: that is a scrolled screen.
        let rows: Vec<&[u8]> = all[3..12].to_vec();
        let report = check(&dump_rows(&rows), &log(W, H, TWELVE)).unwrap();
        assert_eq!((report.non_blank_rows, report.scrolled), (9, true));
        let err = CheckError::NotFull { last: 3, rows: 10 }.to_string();
        assert!(err.contains("3 of 10") && err.contains("not full"), "{err}");
    }

    // --- every row where the row above leaves off ---------------------------------

    /// The honest unscrolled screen of the first ten of [`TWELVE`], with
    /// `change` applied to its rows.
    fn doctored(change: impl Fn(&mut Vec<&[u8]>)) -> Result<Report, CheckError> {
        let mut rows: Vec<&[u8]> = lines(TWELVE)[..10].to_vec();
        change(&mut rows);
        check(&dump_rows(&rows), &log(W, H, &TWELVE[..TWELVE.len() - 23]))
    }

    #[test]
    fn a_row_cut_short_is_refused() {
        assert!(doctored(|_| {}).is_ok());
        for cut in [&b"3: "[..], b"3:", b"3"] {
            assert_eq!(
                doctored(|rows| rows[2] = cut),
                Err(CheckError::RowEndsEarly {
                    row: 2,
                    text: lossy(trimmed(cut))
                }),
                "{:?}",
                String::from_utf8_lossy(cut)
            );
        }
        // Letters lost from the END of a line are not seen: they read as tick
        // bytes before the line end. Pinned, not wanted.
        assert!(doctored(|rows| rows[2] = b"3: thr").is_ok());
        // A word lost after a space, a digit lost, punctuation lost.
        let body = b"errno ok\nn=15\ndone.\n4\n5\n6\n7\n8\n9\n";
        let shown = |rows: &[&[u8]]| check(&dump_rows(rows), &log(W, H, body));
        let honest: Vec<&[u8]> = lines(body);
        assert!(shown(&honest).is_ok());
        for (row, cut) in [(0, &b"errno"[..]), (1, b"n=1"), (2, b"done")] {
            let mut rows = honest.clone();
            rows[row] = cut;
            assert_eq!(
                shown(&rows),
                Err(CheckError::RowEndsEarly {
                    row,
                    text: lossy(cut)
                })
            );
        }
        let text = CheckError::RowEndsEarly {
            row: 2,
            text: "3: thr".into(),
        }
        .to_string();
        assert!(
            text.contains("screen row 2") && text.contains("\"3: thr\""),
            "{text}"
        );
    }

    #[test]
    fn overflow_that_was_dropped_instead_of_wrapped_is_refused() {
        // Fourteen bytes on ten columns must take two rows.
        let body = b"0123456789-=+!\n2\n3\n4\n5\n6\n7\n8\n";
        assert_eq!(check_text(body).unwrap().wraps, 1);
        let rows: Vec<&[u8]> = vec![b"0123456789", b"2", b"3", b"4", b"5", b"6", b"7", b"8"];
        assert_eq!(
            check(&dump_rows(&rows), &log(W, H, body)),
            Err(CheckError::RowMisplaced {
                row: 1,
                text: "2".into()
            })
        );
    }

    #[test]
    fn a_row_shifted_left_is_refused_when_it_lost_more_than_letters() {
        for shifted in [&b" three"[..], b"three"] {
            assert_eq!(
                doctored(|rows| rows[2] = shifted),
                Err(CheckError::RowMisplaced {
                    row: 2,
                    text: lossy(shifted)
                })
            );
        }
        // Having lost only letters from its front, it is not: they read as a
        // run of tick bytes before the line. Pinned, not wanted.
        let body = b"alpha 1\nbravo 22\ncharlie 3\ndelta\necho 5\nfoxtrot\ngolf\nhotel 8\n";
        let mut rows = lines(body);
        rows[2] = b"lie 3";
        let report = check(&dump_rows(&rows), &log(W, H, body)).unwrap();
        assert_eq!(report.stray, 0, "a run between rows is not inside one");
    }

    #[test]
    fn a_repeated_row_and_rows_out_of_order_are_refused() {
        // Row 2 replaced by a copy of row 4's line.
        assert_eq!(
            doctored(|rows| rows[2] = rows[4]),
            Err(CheckError::RowMisplaced {
                row: 2,
                text: "5: five".into()
            })
        );
        // Two rows exchanged.
        assert_eq!(
            doctored(|rows| rows.swap(2, 3)),
            Err(CheckError::RowMisplaced {
                row: 2,
                text: "4: four".into()
            })
        );
        // A row missing, the rest moved up.
        assert_eq!(
            doctored(|rows| {
                rows.remove(2);
            }),
            Err(CheckError::RowMisplaced {
                row: 2,
                text: "4: four".into()
            })
        );
        // A blank row where a line should be, and a line where the console
        // wrote none.
        assert!(matches!(
            doctored(|rows| rows[2] = b""),
            Err(CheckError::RowEndsEarly { row: 2, .. })
        ));
        assert!(matches!(
            doctored(|rows| rows.insert(2, b"")),
            Err(CheckError::RowEndsEarly { row: 2, .. })
        ));
        let text = CheckError::RowMisplaced {
            row: 2,
            text: "4: four".into(),
        }
        .to_string();
        assert!(
            text.contains("screen row 2") && text.contains("\"4: four\""),
            "{text}"
        );
    }

    #[test]
    fn rows_left_over_from_before_a_scroll_are_refused() {
        // Mutation 7's screen: the grid scrolled, nothing was repainted but
        // the newest line. Rows 0..=8 still show lines 1..=9; row 9 shows
        // line 11 over the cursor's row.
        let all = lines(TWELVE);
        let mut rows: Vec<&[u8]> = all[..9].to_vec();
        rows.push(all[10]);
        assert_eq!(
            check(&dump_rows(&rows), &log(W, H, TWELVE)),
            Err(CheckError::RowMisplaced {
                row: 9,
                text: "11: eleven".into()
            })
        );
    }

    #[test]
    fn the_last_non_blank_row_is_matched_as_a_prefix() {
        // A line still being written when the dump was taken; by the time the
        // log was read it had been finished and more had followed.
        let shown = [EIGHT, b"nine: par"].concat();
        let written = [EIGHT, b"nine: partial, then finished\nten\neleven\n"].concat();
        let report = check(&dump(W, H, &shown), &log(W, H, &written)).unwrap();
        assert_eq!(report.text[8], "nine: par");
        // The same exemption lets a last row that was cut short through.
        // Pinned, not wanted.
        assert!(doctored(|rows| rows[9] = b"10: t").is_ok());
        // It is still its text that must be there.
        assert_eq!(
            doctored(|rows| rows[9] = b"10: tan"),
            Err(CheckError::RowNotInLog {
                row: 9,
                text: "10: tan".into()
            })
        );
        // Blank rows below it ask for nothing.
        assert!(check_text(EIGHT).is_ok());
    }

    #[test]
    fn blank_lines_blanks_at_a_lines_end_and_a_wrap_at_a_blank_are_read_as_drawn() {
        // An empty line is a blank row between two others. (Twenty rows, so
        // that eight of them can hold text.)
        let text = b"one\n\nthree\n\n\nsix\n7\n8\n9\n10\n11\n";
        let report = check(&dump(80, 320, text), &log(80, 320, text)).unwrap();
        assert_eq!(&report.text[..6], ["one", "", "three", "", "", "six"]);
        assert_eq!(report.non_blank_rows, 8, "blank rows are not counted");
        // The log without one of the empty lines is another screen.
        let body = text.replace(b"three\n\n\n", b"three\n\n");
        assert!(check(&dump(80, 320, text), &log(80, 320, &body)).is_err());

        // A line of blanks, and a line ending in them.
        let text = b"one\n   \nthree  \n4\n5\n6\n7\n8\n9\n";
        assert_eq!(check_text(text).unwrap().non_blank_rows, 8);

        // A line that wraps where it has blanks: the row looks short and its
        // line carries on below all the same.
        let text = b"abcdefgh  ij\n2\n3\n4\n5\n6\n7\n8\n";
        let report = check_text(text).unwrap();
        assert_eq!(&report.text[..2], ["abcdefgh", "ij"]);
        assert_eq!(report.wraps, 1);
        // One blank fewer in the log and the row no longer reaches the last
        // column: "ij" cannot be its continuation.
        let body = text.replace(b"gh  ij", b"gh ij");
        assert!(check(&dump(W, H, text), &log(W, H, &body)).is_err());
    }

    #[test]
    fn tick_bytes_are_allowed_after_a_rows_text_but_not_after_a_trailing_blank() {
        let honest = lines(EIGHT);
        // After the text, before the line end; between `\r` and `\n`.
        for end in [&b"CCCCC\r\n"[..], b"t\n", b"\rv\n", b"C\rC\n"] {
            let body = EIGHT.replace(b"delta\n", &[b"delta".as_slice(), end].concat());
            assert!(
                check(&dump_rows(&honest), &log(W, H, &body)).is_ok(),
                "{end:?}"
            );
        }
        // After a blank, they would be a lost word.
        let body = EIGHT.replace(b"delta\n", b"delta ok\n");
        assert_eq!(
            check(&dump_rows(&honest), &log(W, H, &body)),
            Err(CheckError::RowEndsEarly {
                row: 3,
                text: "delta".into()
            })
        );
        let body = EIGHT.replace(b"delta\n", b"delta  C\n");
        assert!(check(&dump_rows(&honest), &log(W, H, &body)).is_err());
        // Nor between two blanks.
        let body = EIGHT.replace(b"delta\n", b"delta C \n");
        assert!(check(&dump_rows(&honest), &log(W, H, &body)).is_err());
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
