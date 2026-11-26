// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Glyph lookup over the vendored Spleen 8×16 font (slice 6.7, decision F7).
//!
//! `drivers/tty/font/spleen-8x16.psfu` is the upstream release file, byte for
//! byte (`font/README.md` records where it came from and its digest). It is
//! embedded here with `include_bytes!` and its header is checked in a `const`
//! context, so a wrong file — the wrong size, the wrong format, a truncated
//! download — fails the **build**, not the boot.
//!
//! ## The file is PSF version 1
//!
//! The spec calls it PSF2; it is not. Upstream's `bdf2psf` writes version 2 for
//! the wider faces and version 1 for this one, the only face exactly eight
//! pixels wide. The layout:
//!
//! ```text
//! 0      magic      36 04
//! 2      mode       bit 0: 512 glyphs (else 256); bit 1: a Unicode table follows
//! 3      charsize   bytes per glyph = rows per glyph; the width is always 8
//! 4      glyphs     count × charsize bytes, one byte per row, bit 7 leftmost
//! …      table      little-endian u16 code points; 0xFFFF ends a glyph's list,
//!                   0xFFFE introduces a combining sequence
//! ```
//!
//! ## Why indexing by byte is sound
//!
//! A PSF file's glyph order is its own business — the Unicode table is what
//! says which glyph draws which character. This font happens to put U+0020 to
//! U+007E at glyphs `0x20..=0x7E`, and a host test walks the table to pin
//! exactly that, so a release that reorders them fails `cargo test` instead of
//! printing the wrong letters. Glyphs below `0x20` are *not* the ASCII controls
//! (they are symbols — glyph 1 is a smiley), which is the other reason every
//! byte outside the printable range draws [`REPLACEMENT`].

/// A cell's width in pixels.
pub const CELL_W: u32 = 8;
/// A cell's height in pixels.
pub const CELL_H: u32 = 16;

/// The first byte that has a glyph of its own.
pub const FIRST_PRINTABLE: u8 = 0x20;
/// The last byte that has a glyph of its own.
pub const LAST_PRINTABLE: u8 = 0x7E;
/// What every other byte draws (F7): `?`.
pub const REPLACEMENT: u8 = b'?';

/// One glyph: [`CELL_H`] rows, top first; in each row bit 7 is the leftmost
/// pixel and a set bit is foreground.
pub type Glyph = [u8; CELL_H as usize];

/// The upstream file, unmodified.
const PSF: &[u8] = include_bytes!("../font/spleen-8x16.psfu");

const PSF1_MAGIC: [u8; 2] = [0x36, 0x04];
const PSF1_MODE_512: u8 = 0x01;
const PSF1_MODE_HAS_TABLE: u8 = 0x02;
const PSF1_HEADER_LEN: usize = 4;

/// What a PSF1 header says about the file it heads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Psf1 {
    /// Number of glyphs: 256 or 512.
    glyphs: usize,
    /// Byte offset of the Unicode table — the end of the glyph data.
    table: usize,
}

/// Why a file is not the font this module can draw with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FontError {
    /// Shorter than a PSF1 header.
    TooShort,
    /// Not PSF1 — a PSF2 file (`72 b5 4a 86`) lands here.
    Magic,
    /// A PSF1 font, but not [`CELL_H`] rows tall.
    Height,
    /// No Unicode table, so nothing says which glyph is which character.
    NoUnicodeTable,
    /// The header promises more glyph data than the file holds.
    Truncated,
}

/// Check a PSF1 header against what this module needs: 8×16, a Unicode table,
/// and all the glyph data present. `const` so the embedded file is checked at
/// build time; a plain function so the refusals can be host-tested.
const fn parse_psf1(bytes: &[u8]) -> Result<Psf1, FontError> {
    if bytes.len() < PSF1_HEADER_LEN {
        return Err(FontError::TooShort);
    }
    if bytes[0] != PSF1_MAGIC[0] || bytes[1] != PSF1_MAGIC[1] {
        return Err(FontError::Magic);
    }
    let mode = bytes[2];
    if bytes[3] as u32 != CELL_H {
        return Err(FontError::Height);
    }
    if mode & PSF1_MODE_HAS_TABLE == 0 {
        return Err(FontError::NoUnicodeTable);
    }
    let glyphs: usize = if mode & PSF1_MODE_512 != 0 { 512 } else { 256 };
    // At most 4 + 512 * 16: no overflow to check.
    let table = PSF1_HEADER_LEN + glyphs * CELL_H as usize;
    if bytes.len() < table {
        return Err(FontError::Truncated);
    }
    Ok(Psf1 { glyphs, table })
}

/// The embedded file's header, checked at build time. Each refusal has its own
/// message because a `const` panic can only carry a literal.
const HEADER: Psf1 = match parse_psf1(PSF) {
    Ok(header) => header,
    Err(FontError::TooShort) => panic!("font/spleen-8x16.psfu: shorter than a PSF1 header"),
    Err(FontError::Magic) => panic!("font/spleen-8x16.psfu: not a PSF version 1 file"),
    Err(FontError::Height) => panic!("font/spleen-8x16.psfu: not 16 rows tall"),
    Err(FontError::NoUnicodeTable) => panic!("font/spleen-8x16.psfu: no Unicode table"),
    Err(FontError::Truncated) => panic!("font/spleen-8x16.psfu: glyph data truncated"),
};

/// How many glyphs are kept: one per printable byte.
const PRINTABLE: usize = (LAST_PRINTABLE - FIRST_PRINTABLE) as usize + 1;

// PSF1 has no fewer than 256 glyphs, so this cannot fail today; it is here so
// that a different header parser cannot quietly make the table read past them.
const _: () = assert!(
    HEADER.glyphs > LAST_PRINTABLE as usize,
    "font/spleen-8x16.psfu: fewer glyphs than printable bytes"
);

/// The printable glyphs, copied out of the file at build time: entry `i` is
/// byte `FIRST_PRINTABLE + i`. Copying is what lets [`glyph`] hand out a
/// `&'static Glyph` with neither `unsafe` nor a fallible slice conversion, and
/// leaves the other 417 glyphs out of the driver's image.
static GLYPHS: [Glyph; PRINTABLE] = {
    let mut table = [[0u8; CELL_H as usize]; PRINTABLE];
    let mut i = 0;
    while i < PRINTABLE {
        let base = PSF1_HEADER_LEN + (FIRST_PRINTABLE as usize + i) * CELL_H as usize;
        let mut row = 0;
        while row < CELL_H as usize {
            table[i][row] = PSF[base + row];
            row += 1;
        }
        i += 1;
    }
    table
};

/// The glyph `byte` draws: its own for `0x20..=0x7E`, [`REPLACEMENT`]'s for
/// everything else.
pub fn glyph(byte: u8) -> &'static Glyph {
    let drawn = if (FIRST_PRINTABLE..=LAST_PRINTABLE).contains(&byte) {
        byte
    } else {
        REPLACEMENT
    };
    &GLYPHS[usize::from(drawn - FIRST_PRINTABLE)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Unicode table, one list of code points per glyph. A combining
    /// sequence (after `0xFFFE`) is not a code point of its own and is left out.
    fn unicode_entries() -> Vec<Vec<u16>> {
        let header = parse_psf1(PSF).expect("the vendored font parses");
        let (units, rest) = PSF[header.table..].as_chunks::<2>();
        assert!(
            rest.is_empty(),
            "the Unicode table is a whole number of u16s"
        );
        let mut entries = Vec::new();
        let mut current = Vec::new();
        let mut in_sequence = false;
        for unit in units {
            match u16::from_le_bytes(*unit) {
                0xFFFF => {
                    entries.push(core::mem::take(&mut current));
                    in_sequence = false;
                }
                0xFFFE => in_sequence = true,
                cp if !in_sequence => current.push(cp),
                _ => {}
            }
        }
        assert!(current.is_empty(), "the table ends on a terminator");
        entries
    }

    /// The raw bytes of glyph `n`, straight from the file.
    fn raw(n: usize) -> &'static [u8] {
        let h = CELL_H as usize;
        &PSF[PSF1_HEADER_LEN + n * h..PSF1_HEADER_LEN + (n + 1) * h]
    }

    #[test]
    fn the_vendored_header_is_accepted() {
        assert_eq!(PSF.len(), 10648, "the upstream 2.3.0 file's size");
        assert_eq!(
            parse_psf1(PSF),
            Ok(Psf1 {
                glyphs: 512,
                table: 4 + 512 * 16,
            })
        );
    }

    #[test]
    fn a_wrong_file_is_refused() {
        assert_eq!(parse_psf1(&[]), Err(FontError::TooShort));
        assert_eq!(parse_psf1(&[0x36, 0x04, 0x03]), Err(FontError::TooShort));

        // A PSF2 header: the format the spec expected and the file is not.
        let mut psf2 = vec![0x72, 0xb5, 0x4a, 0x86];
        psf2.resize(32 + 256 * 16, 0);
        assert_eq!(parse_psf1(&psf2), Err(FontError::Magic));

        // The right format at the wrong height (an 8×14 font).
        let mut short = vec![0x36, 0x04, 0x02, 14];
        short.resize(4 + 256 * 14, 0);
        assert_eq!(parse_psf1(&short), Err(FontError::Height));

        // No Unicode table: nothing to prove the ASCII identity with.
        let mut bare = vec![0x36, 0x04, 0x00, 16];
        bare.resize(4 + 256 * 16, 0);
        assert_eq!(parse_psf1(&bare), Err(FontError::NoUnicodeTable));

        // One byte short of its 256 glyphs, then exactly enough.
        let mut cut = vec![0x36, 0x04, 0x02, 16];
        cut.resize(4 + 256 * 16 - 1, 0);
        assert_eq!(parse_psf1(&cut), Err(FontError::Truncated));
        cut.push(0);
        assert_eq!(
            parse_psf1(&cut),
            Ok(Psf1 {
                glyphs: 256,
                table: 4 + 256 * 16,
            })
        );

        // The 512-glyph bit doubles what must be present.
        cut[2] = 0x03;
        assert_eq!(parse_psf1(&cut), Err(FontError::Truncated));

        // The real file, cut inside its glyph data.
        assert_eq!(
            parse_psf1(&PSF[..4 + 512 * 16 - 1]),
            Err(FontError::Truncated)
        );
    }

    /// F7's identity: glyph `n` is U+00`n` for every printable ASCII `n`, and no
    /// other glyph claims that code point. This is what licenses indexing the
    /// font by byte.
    #[test]
    fn glyph_n_is_ascii_n_through_the_unicode_table() {
        let entries = unicode_entries();
        assert_eq!(entries.len(), 512, "one table entry per glyph");
        for n in FIRST_PRINTABLE..=LAST_PRINTABLE {
            let cp = u16::from(n);
            assert_eq!(
                entries[usize::from(n)].first(),
                Some(&cp),
                "glyph {n:#04x} is not listed first as U+{cp:04X}"
            );
            for (other, entry) in entries.iter().enumerate() {
                if other != usize::from(n) {
                    assert!(
                        !entry.contains(&cp),
                        "glyph {other:#x} also claims U+{cp:04X}"
                    );
                }
            }
        }
    }

    /// The reason unprintable bytes cannot index the font directly: the glyphs
    /// below 0x20 are symbols, not controls.
    #[test]
    fn the_glyphs_below_space_are_not_ascii() {
        let entries = unicode_entries();
        for (n, entry) in (0..u16::from(FIRST_PRINTABLE)).zip(&entries) {
            assert!(
                !entry.contains(&n),
                "glyph {n:#x} unexpectedly maps to its own code point"
            );
        }
        assert_eq!(entries[1].first(), Some(&0x263A), "glyph 1 is a smiley");
    }

    /// `A`, written out by hand from the picture — the one check that does not
    /// go through this module's own reading of the file.
    ///
    /// ```text
    /// ........
    /// ........
    /// .#####..
    /// ##...##.
    /// ##...##.
    /// ##...##.
    /// #######.
    /// ##...##.
    /// ##...##.
    /// ##...##.
    /// ##...##.
    /// ##...##.
    /// ........
    /// ........
    /// ........
    /// ........
    /// ```
    #[test]
    fn golden_glyph_capital_a() {
        const A: Glyph = [
            0b0000_0000,
            0b0000_0000,
            0b0111_1100,
            0b1100_0110,
            0b1100_0110,
            0b1100_0110,
            0b1111_1110,
            0b1100_0110,
            0b1100_0110,
            0b1100_0110,
            0b1100_0110,
            0b1100_0110,
            0b0000_0000,
            0b0000_0000,
            0b0000_0000,
            0b0000_0000,
        ];
        assert_eq!(glyph(b'A'), &A);
    }

    #[test]
    fn every_printable_glyph_is_the_files() {
        for n in FIRST_PRINTABLE..=LAST_PRINTABLE {
            assert_eq!(glyph(n).as_slice(), raw(usize::from(n)), "glyph {n:#04x}");
        }
    }

    #[test]
    fn every_other_byte_draws_the_question_mark() {
        let question = raw(0x3F);
        assert_eq!(REPLACEMENT, 0x3F);
        for byte in 0..=u8::MAX {
            if !(FIRST_PRINTABLE..=LAST_PRINTABLE).contains(&byte) {
                assert_eq!(glyph(byte).as_slice(), question, "byte {byte:#04x}");
            }
        }
        // …and not the glyph the file holds at that index.
        assert_ne!(glyph(0x01).as_slice(), raw(0x01));
        assert_ne!(glyph(0x7F).as_slice(), raw(0x7F));
        assert_ne!(glyph(0xFF).as_slice(), raw(0xFF));
    }

    /// Space is the only blank glyph and no two printable glyphs are the same
    /// picture — which is what makes `paint::decode_cell` unambiguous.
    #[test]
    fn printable_glyphs_are_distinct_and_only_space_is_blank() {
        assert_eq!(glyph(b' '), &[0u8; 16]);
        for a in FIRST_PRINTABLE..=LAST_PRINTABLE {
            if a != b' ' {
                assert_ne!(glyph(a), &[0u8; 16], "glyph {a:#04x} is blank");
            }
            for b in a + 1..=LAST_PRINTABLE {
                assert_ne!(glyph(a), glyph(b), "{a:#04x} and {b:#04x} look alike");
            }
        }
    }

    #[test]
    fn a_glyph_is_one_byte_per_row_of_an_eight_pixel_cell() {
        assert_eq!(glyph(b'A').len(), CELL_H as usize);
        assert_eq!(u8::BITS, CELL_W);
    }
}
