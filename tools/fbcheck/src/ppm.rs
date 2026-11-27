// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The one image format a QMP `screendump` is read in: binary PPM (`P6`) with
//! a maximum sample value of 255.
//!
//! QEMU 11 writes exactly `P6\n<width> <height>\n255\n` and then
//! `width * height` pixels of three bytes each, red, green, blue. The parser
//! accepts that and the rest of what Netpbm allows in a `P6` header — any
//! white space between the fields, and `#` comments running to the end of
//! their line before the maximum value — because both cost a few lines and a
//! dump that has been through another tool is still a dump. After the maximum
//! value comes **exactly one** white-space byte and then the pixels, as the
//! format says.
//!
//! Everything else is refused, by name: another magic number (`P3`, `P5`, a
//! PNG), a maximum value other than 255 (two-byte samples above it, a
//! different scale below it), a zero dimension, and pixel data that is not
//! exactly `width * height * 3` bytes — a truncated file and one with
//! trailing bytes alike.

use std::fmt;

/// The two bytes a binary PPM starts with.
const MAGIC: &[u8] = b"P6";
/// The only maximum sample value accepted: one byte per channel, full scale.
const MAXVAL: u32 = 255;
/// Bytes per pixel in the file: red, green, blue.
const SAMPLES: usize = 3;
/// A header field longer than this is not a dimension anybody has. It also
/// keeps the digits inside a `u32`.
const MAX_DIGITS: usize = 9;

/// Why a file is not a PPM this tool reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PpmError {
    /// The file does not start with `P6` and white space.
    Magic,
    /// The named header field is missing or is not a decimal number.
    Field(&'static str),
    /// The maximum sample value is not 255.
    MaxVal(u32),
    /// The width or the height is zero.
    ZeroSize,
    /// `width * height * 3` does not fit this machine's address space.
    TooLarge,
    /// The pixel data is not `width * height * 3` bytes long.
    Length { expected: usize, found: usize },
}

impl fmt::Display for PpmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Magic => write!(f, "not a binary PPM: it does not start with \"P6\""),
            Self::Field(name) => write!(f, "PPM header: the {name} is missing or not a number"),
            Self::MaxVal(n) => write!(f, "PPM header: the maximum value is {n}, not 255"),
            Self::ZeroSize => write!(f, "PPM header: the width or the height is zero"),
            Self::TooLarge => write!(f, "PPM header: the image is too large to hold"),
            Self::Length { expected, found } => write!(
                f,
                "PPM pixel data is {found} bytes where the header's size needs exactly {expected}"
            ),
        }
    }
}

/// A decoded image: its size and its pixels, three bytes each, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ppm {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

impl Ppm {
    /// Parse a whole file.
    pub fn parse(bytes: &[u8]) -> Result<Self, PpmError> {
        if !bytes.starts_with(MAGIC) {
            return Err(PpmError::Magic);
        }
        let mut pos = MAGIC.len();
        // "P61 1 255" is not a PPM: the magic number is its own token.
        if !bytes.get(pos).is_some_and(|&b| is_space(b) || b == b'#') {
            return Err(PpmError::Magic);
        }
        let width = field(bytes, &mut pos, "width")?;
        let height = field(bytes, &mut pos, "height")?;
        let maxval = field(bytes, &mut pos, "maximum value")?;
        if maxval != MAXVAL {
            return Err(PpmError::MaxVal(maxval));
        }
        if width == 0 || height == 0 {
            return Err(PpmError::ZeroSize);
        }
        // Exactly one white-space byte ends the header; the next byte is a
        // sample even if it looks like white space or a `#`.
        if !bytes.get(pos).copied().is_some_and(is_space) {
            return Err(PpmError::Field("white space after the maximum value"));
        }
        pos += 1;

        let expected = usize::try_from(width)
            .ok()
            .zip(usize::try_from(height).ok())
            .and_then(|(w, h)| w.checked_mul(h))
            .and_then(|n| n.checked_mul(SAMPLES))
            .ok_or(PpmError::TooLarge)?;
        let data = bytes.get(pos..).unwrap_or(&[]);
        if data.len() != expected {
            return Err(PpmError::Length {
                expected,
                found: data.len(),
            });
        }
        Ok(Self {
            width,
            height,
            rgb: data.to_vec(),
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

    /// The pixels, `(red, green, blue)`, left to right and top to bottom.
    pub fn pixels(&self) -> impl Iterator<Item = (u8, u8, u8)> + '_ {
        self.rgb
            .as_chunks::<SAMPLES>()
            .0
            .iter()
            .map(|&[r, g, b]| (r, g, b))
    }
}

/// Write the file QEMU writes: `P6\n<width> <height>\n255\n` and the pixels.
/// `rgb` is taken as given; it is the caller's business that it is
/// `width * height * 3` bytes (the tests that build a short file rely on
/// that).
pub fn encode(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let mut out = format!("P6\n{width} {height}\n{MAXVAL}\n").into_bytes();
    out.extend_from_slice(rgb);
    out
}

/// The white space a PPM header is made of.
const fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C)
}

/// Read one decimal header field, skipping the white space and the comments in
/// front of it, and leave `pos` on the byte after its last digit.
fn field(bytes: &[u8], pos: &mut usize, name: &'static str) -> Result<u32, PpmError> {
    loop {
        match bytes.get(*pos) {
            Some(&b) if is_space(b) => *pos += 1,
            Some(b'#') => {
                // A comment runs to the end of its line.
                while bytes.get(*pos).is_some_and(|&b| b != b'\n' && b != b'\r') {
                    *pos += 1;
                }
            }
            _ => break,
        }
    }
    let start = *pos;
    let mut value: u32 = 0;
    while let Some(digit) = bytes.get(*pos).filter(|b| b.is_ascii_digit()) {
        if *pos - start >= MAX_DIGITS {
            return Err(PpmError::Field(name));
        }
        // At most nine digits, so neither step can leave a `u32`.
        value = value * 10 + u32::from(digit - b'0');
        *pos += 1;
    }
    if *pos == start {
        return Err(PpmError::Field(name));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Six bytes: a red pixel and a blue one.
    const TWO: [u8; 6] = [0xFF, 0, 0, 0, 0, 0xFF];

    fn rgb(ppm: &Ppm) -> Vec<(u8, u8, u8)> {
        ppm.pixels().collect()
    }

    #[test]
    fn the_header_qemu_writes_is_accepted() {
        let ppm = Ppm::parse(b"P6\n2 1\n255\n\xFF\x00\x00\x00\x00\xFF").unwrap();
        assert_eq!((ppm.width(), ppm.height()), (2, 1));
        assert_eq!(rgb(&ppm), [(0xFF, 0, 0), (0, 0, 0xFF)]);
    }

    #[test]
    fn encode_writes_that_header_and_parse_reads_it_back() {
        let file = encode(1, 2, &TWO);
        assert!(file.starts_with(b"P6\n1 2\n255\n"));
        let ppm = Ppm::parse(&file).unwrap();
        assert_eq!((ppm.width(), ppm.height()), (1, 2));
        assert_eq!(rgb(&ppm), [(0xFF, 0, 0), (0, 0, 0xFF)]);
    }

    #[test]
    fn the_measured_size_is_accepted() {
        let file = encode(800, 600, &vec![0; 800 * 600 * 3]);
        let ppm = Ppm::parse(&file).unwrap();
        assert_eq!((ppm.width(), ppm.height()), (800, 600));
        assert_eq!(ppm.pixels().count(), 480_000);
    }

    #[test]
    fn any_white_space_and_comments_are_allowed_between_the_fields() {
        let mut file =
            b"P6 # made by hand\r\n# a second comment\n  2\t1 # the size\n255\n".to_vec();
        file.extend_from_slice(&TWO);
        let ppm = Ppm::parse(&file).unwrap();
        assert_eq!((ppm.width(), ppm.height()), (2, 1));
        assert_eq!(rgb(&ppm), [(0xFF, 0, 0), (0, 0, 0xFF)]);
    }

    #[test]
    fn only_one_byte_of_white_space_follows_the_maximum_value() {
        // The pixels here ARE white space and a `#`: they must not be eaten.
        let ppm = Ppm::parse(b"P6\n1 1\n255\n\n #").unwrap();
        assert_eq!(rgb(&ppm), [(b'\n', b' ', b'#')]);
        // Two bytes of white space make the data one byte too long.
        assert_eq!(
            Ppm::parse(b"P6\n1 1\n255\n\n\x01\x02\x03"),
            Err(PpmError::Length {
                expected: 3,
                found: 4
            })
        );
        // None at all is no header end.
        assert!(matches!(
            Ppm::parse(b"P6\n1 1\n255"),
            Err(PpmError::Field(_))
        ));
    }

    #[test]
    fn another_magic_number_is_refused() {
        for file in [
            &b"P3\n1 1\n255\n0 0 0\n"[..],
            b"P5\n1 1\n255\n\x00",
            b"P7\n",
            b"\x89PNG\r\n\x1a\n",
            b"P",
            b"",
            b"P61 1 255\n\x00\x00\x00",
        ] {
            assert_eq!(Ppm::parse(file), Err(PpmError::Magic), "{file:?}");
        }
    }

    #[test]
    fn a_maximum_value_other_than_255_is_refused() {
        assert_eq!(
            Ppm::parse(b"P6\n1 1\n65535\n\x00\x00\x00\x00\x00\x00"),
            Err(PpmError::MaxVal(65535))
        );
        assert_eq!(
            Ppm::parse(b"P6\n1 1\n254\n\x00\x00\x00"),
            Err(PpmError::MaxVal(254))
        );
        assert_eq!(
            Ppm::parse(b"P6\n1 1\n0\n\x00\x00\x00"),
            Err(PpmError::MaxVal(0))
        );
    }

    #[test]
    fn a_missing_or_malformed_field_is_refused_by_name() {
        assert_eq!(Ppm::parse(b"P6\n"), Err(PpmError::Field("width")));
        assert_eq!(Ppm::parse(b"P6\nx 1\n255\n"), Err(PpmError::Field("width")));
        assert_eq!(
            Ppm::parse(b"P6\n-1 1\n255\n"),
            Err(PpmError::Field("width"))
        );
        assert_eq!(Ppm::parse(b"P6\n1\n"), Err(PpmError::Field("height")));
        assert_eq!(
            Ppm::parse(b"P6\n1 1\n"),
            Err(PpmError::Field("maximum value"))
        );
        // Ten digits: past any screen, and past a `u32` for some values.
        assert_eq!(
            Ppm::parse(b"P6\n4294967296 1\n255\n"),
            Err(PpmError::Field("width"))
        );
    }

    #[test]
    fn a_zero_dimension_is_refused() {
        assert_eq!(Ppm::parse(b"P6\n0 1\n255\n"), Err(PpmError::ZeroSize));
        assert_eq!(Ppm::parse(b"P6\n1 0\n255\n"), Err(PpmError::ZeroSize));
    }

    #[test]
    fn pixel_data_of_the_wrong_length_is_refused() {
        // One byte short: a dump cut off in the middle of its last pixel.
        assert_eq!(
            Ppm::parse(&encode(2, 1, &TWO[..5])),
            Err(PpmError::Length {
                expected: 6,
                found: 5
            })
        );
        // No data at all.
        assert_eq!(
            Ppm::parse(&encode(2, 1, &[])),
            Err(PpmError::Length {
                expected: 6,
                found: 0
            })
        );
        // One byte over.
        let mut long = TWO.to_vec();
        long.push(0);
        assert_eq!(
            Ppm::parse(&encode(2, 1, &long)),
            Err(PpmError::Length {
                expected: 6,
                found: 7
            })
        );
    }

    #[test]
    fn a_size_whose_byte_count_overflows_is_refused_not_wrapped() {
        // 999_999_999² × 3 is far past a 64-bit address space.
        let result = Ppm::parse(b"P6\n999999999 999999999\n255\n\x00");
        assert!(
            matches!(result, Err(PpmError::TooLarge | PpmError::Length { .. })),
            "{result:?}"
        );
        assert_ne!(
            result,
            Err(PpmError::Length {
                expected: 0,
                found: 1
            })
        );
    }

    #[test]
    fn every_error_says_what_was_wrong() {
        assert!(PpmError::Magic.to_string().contains("P6"));
        assert!(PpmError::Field("height").to_string().contains("height"));
        assert!(PpmError::MaxVal(7).to_string().contains('7'));
        let text = PpmError::Length {
            expected: 6,
            found: 5,
        }
        .to_string();
        assert!(text.contains('6') && text.contains('5'), "{text}");
    }
}
