// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The `GET_FRAMEBUFFER` reply: decoded, then accepted or refused (slice 6.7,
//! decisions F4 and F9).
//!
//! The kernel validated Limine's framebuffer before it mapped it (F2). TTY
//! validates the copy that came back through `SYS_GETINFO` again, **without
//! trusting it**: the reply is the only thing between a wrong number and a
//! volatile store through a device mapping. [`accept`] is the whole decision —
//! the binary makes the kernel call, hands the payload here, and prints the
//! verdict — so every refusal has a host test.
//!
//! The checks, in order; the first that fails names the refusal:
//!
//! 1. the reported VA is the address the kernel maps the framebuffer at
//!    ([`Refusal::Va`]) — the binary stores at that constant, so a reply naming
//!    another address describes a mapping TTY does not have;
//! 2. [`Format::new`]'s conditions ([`Refusal::Format`]): 32 bpp, sane and
//!    distinct shifts, a non-zero size, `width * 4 <= pitch`, `pitch % 4 == 0`;
//! 3. `pitch * height` fits the window ([`Refusal::Window`]) — the bound that
//!    keeps `y * pitch + x * 4` inside what could be mapped at all;
//! 4. the screen holds at least one 8×16 cell ([`Refusal::Grid`]) — a smaller
//!    one is a legal [`Format`] with nothing to draw on.

use minixrs_kernel_shared::callnr::{
    GETINFO_FB_BLUE_SHIFT_OFF, GETINFO_FB_BPP_OFF, GETINFO_FB_GREEN_SHIFT_OFF,
    GETINFO_FB_HEIGHT_OFF, GETINFO_FB_PITCH_OFF, GETINFO_FB_RED_SHIFT_OFF, GETINFO_FB_VA_OFF,
    GETINFO_FB_WIDTH_OFF,
};

use crate::paint::{Format, FormatError};

/// A `GET_FRAMEBUFFER` reply's fields, as the kernel wrote them (F4). Nothing
/// here has been checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reply {
    /// Where the kernel says the framebuffer is mapped.
    pub va: u64,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bytes per row.
    pub pitch: u32,
    /// The red channel's bit offset in a pixel.
    pub red_shift: u8,
    /// The green channel's bit offset.
    pub green_shift: u8,
    /// The blue channel's bit offset.
    pub blue_shift: u8,
    /// Bits per pixel.
    pub bpp: u8,
}

impl Reply {
    /// Read the fields out of a reply message's payload at the offsets
    /// `kernel-shared::callnr` names. The kernel writes them native-endian.
    pub fn decode(payload: &[u8; 96]) -> Self {
        Self {
            va: u64::from_ne_bytes(field(payload, GETINFO_FB_VA_OFF)),
            width: u32::from_ne_bytes(field(payload, GETINFO_FB_WIDTH_OFF)),
            height: u32::from_ne_bytes(field(payload, GETINFO_FB_HEIGHT_OFF)),
            pitch: u32::from_ne_bytes(field(payload, GETINFO_FB_PITCH_OFF)),
            red_shift: payload[GETINFO_FB_RED_SHIFT_OFF],
            green_shift: payload[GETINFO_FB_GREEN_SHIFT_OFF],
            blue_shift: payload[GETINFO_FB_BLUE_SHIFT_OFF],
            bpp: payload[GETINFO_FB_BPP_OFF],
        }
    }
}

/// `N` bytes of `payload` at `off`. The offsets are constants `callnr.rs`
/// asserts lie inside the 96-byte payload, so the fallback is unreachable; it
/// is a zero field — which [`accept`] refuses — rather than a panic.
fn field<const N: usize>(payload: &[u8; 96], off: usize) -> [u8; N] {
    off.checked_add(N)
        .and_then(|end| payload.get(off..end))
        .and_then(|bytes| bytes.try_into().ok())
        .unwrap_or([0; N])
}

/// Why TTY will not draw on the framebuffer a reply describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The reply's VA is not the address the framebuffer is mapped at.
    Va,
    /// One of F2's conditions, re-checked.
    Format(FormatError),
    /// `pitch * height` is larger than the framebuffer window.
    Window,
    /// The screen is smaller than one cell across or down.
    Grid,
}

impl Refusal {
    /// One token, for the `[diag tty] fb FAIL <reason>` line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Va => "va",
            Self::Format(e) => e.as_str(),
            Self::Window => "window",
            Self::Grid => "grid",
        }
    }
}

/// Decide whether `reply` describes a framebuffer TTY may store to, given the
/// address it is mapped at (`uspace::FB_VA`) and how many bytes could be mapped
/// there (`uspace::FB_WINDOW_SIZE`).
///
/// On `Ok` the format has at least one column and one row, and for every
/// `x < width`, `y < height` the byte offset `y * pitch + x * 4` plus the four
/// bytes stored there lies below `pitch * height <= window` — the bound the
/// binary's store relies on.
pub const fn accept(reply: &Reply, mapped_va: u64, window: u64) -> Result<Format, Refusal> {
    if reply.va != mapped_va {
        return Err(Refusal::Va);
    }
    let fmt = match Format::new(
        reply.width,
        reply.height,
        reply.pitch,
        reply.red_shift,
        reply.green_shift,
        reply.blue_shift,
        reply.bpp,
    ) {
        Ok(fmt) => fmt,
        Err(e) => return Err(Refusal::Format(e)),
    };
    if fmt.byte_len() > window {
        return Err(Refusal::Window);
    }
    if fmt.cols() == 0 || fmt.rows() == 0 {
        return Err(Refusal::Grid);
    }
    Ok(fmt)
}

#[cfg(test)]
mod tests {
    use minixrs_kernel_shared::uspace::{FB_VA, FB_WINDOW_SIZE};

    use super::*;

    /// Task 0's measured reply: 800×600, pitch 3200, `r=16 g=8 b=0`, 32 bpp.
    fn measured() -> Reply {
        Reply {
            va: FB_VA,
            width: 800,
            height: 600,
            pitch: 3200,
            red_shift: 16,
            green_shift: 8,
            blue_shift: 0,
            bpp: 32,
        }
    }

    fn verdict(reply: Reply) -> Result<Format, Refusal> {
        accept(&reply, FB_VA, FB_WINDOW_SIZE)
    }

    // --- decoding ---------------------------------------------------------

    #[test]
    fn decode_reads_each_field_at_its_offset() {
        let mut payload = [0u8; 96];
        payload[0..8].copy_from_slice(&0x1_0000_0000u64.to_ne_bytes());
        payload[8..12].copy_from_slice(&800u32.to_ne_bytes());
        payload[12..16].copy_from_slice(&600u32.to_ne_bytes());
        payload[16..20].copy_from_slice(&3200u32.to_ne_bytes());
        payload[20] = 16;
        payload[21] = 8;
        payload[22] = 0;
        payload[23] = 32;
        assert_eq!(Reply::decode(&payload), measured());
    }

    #[test]
    fn decode_keeps_the_fields_apart() {
        // Every field distinct and every byte of it non-zero, so a field read
        // at a neighbour's offset, or a byte short, changes the answer.
        let mut payload = [0u8; 96];
        payload[0..8].copy_from_slice(&0x0102_0304_0506_0708u64.to_ne_bytes());
        payload[8..12].copy_from_slice(&0x1112_1314u32.to_ne_bytes());
        payload[12..16].copy_from_slice(&0x2122_2324u32.to_ne_bytes());
        payload[16..20].copy_from_slice(&0x3132_3334u32.to_ne_bytes());
        payload[20] = 0x41;
        payload[21] = 0x42;
        payload[22] = 0x43;
        payload[23] = 0x44;
        // Past the reply: must not be read.
        payload[24..].fill(0xEE);
        assert_eq!(
            Reply::decode(&payload),
            Reply {
                va: 0x0102_0304_0506_0708,
                width: 0x1112_1314,
                height: 0x2122_2324,
                pitch: 0x3132_3334,
                red_shift: 0x41,
                green_shift: 0x42,
                blue_shift: 0x43,
                bpp: 0x44,
            }
        );
    }

    #[test]
    fn an_all_zero_payload_is_refused() {
        // What `sys_getinfo` leaves if the kernel answered OK and wrote nothing.
        assert_eq!(verdict(Reply::decode(&[0u8; 96])), Err(Refusal::Va));
    }

    // --- accepting --------------------------------------------------------

    #[test]
    fn the_measured_reply_is_accepted() {
        let fmt = verdict(measured()).unwrap();
        assert_eq!((fmt.width(), fmt.height(), fmt.pitch()), (800, 600, 3200));
        assert_eq!((fmt.cols(), fmt.rows()), (100, 37));
        assert_eq!(fmt.fg(), 0x00AA_AAAA);
    }

    #[test]
    fn a_framebuffer_that_exactly_fills_the_window_is_accepted() {
        // 16 MiB = 4096 bytes a row × 4096 rows.
        let reply = Reply {
            width: 1024,
            pitch: 4096,
            height: 4096,
            ..measured()
        };
        let fmt = verdict(reply).unwrap();
        assert_eq!(fmt.byte_len(), FB_WINDOW_SIZE);
    }

    #[test]
    fn one_cell_is_the_smallest_screen_accepted() {
        let reply = Reply {
            width: 8,
            height: 16,
            pitch: 32,
            ..measured()
        };
        let fmt = verdict(reply).unwrap();
        assert_eq!((fmt.cols(), fmt.rows()), (1, 1));
    }

    // --- refusing: one test per token --------------------------------------

    #[test]
    fn a_reply_naming_another_address_is_refused() {
        for va in [0, FB_VA - 4096, FB_VA + 4, FB_VA + 4096, u64::MAX] {
            let reply = Reply { va, ..measured() };
            assert_eq!(verdict(reply), Err(Refusal::Va), "va={va:#x}");
        }
    }

    #[test]
    fn the_address_is_checked_before_the_format() {
        // Wrong in every way at once: the address is what is named.
        let reply = Reply {
            va: 0,
            bpp: 24,
            width: 0,
            ..measured()
        };
        assert_eq!(verdict(reply), Err(Refusal::Va));
    }

    #[test]
    fn each_format_condition_is_refused_by_name() {
        let cases = [
            (
                Reply {
                    bpp: 24,
                    ..measured()
                },
                FormatError::Bpp,
            ),
            (
                Reply {
                    red_shift: 4,
                    ..measured()
                },
                FormatError::Shift,
            ),
            (
                Reply {
                    blue_shift: 32,
                    ..measured()
                },
                FormatError::Shift,
            ),
            (
                Reply {
                    green_shift: 16,
                    ..measured()
                },
                FormatError::ShiftsOverlap,
            ),
            (
                Reply {
                    width: 0,
                    ..measured()
                },
                FormatError::ZeroSize,
            ),
            (
                Reply {
                    height: 0,
                    ..measured()
                },
                FormatError::ZeroSize,
            ),
            // `width * 4 <= pitch` (plan Review Focus 5).
            (
                Reply {
                    pitch: 3196,
                    ..measured()
                },
                FormatError::Pitch,
            ),
            (
                Reply {
                    pitch: 3202,
                    ..measured()
                },
                FormatError::Pitch,
            ),
            // `width * 4` does not fit a `u32`.
            (
                Reply {
                    width: 0x4000_0000,
                    pitch: 0xFFFF_FFFC,
                    ..measured()
                },
                FormatError::Pitch,
            ),
        ];
        for (reply, error) in cases {
            assert_eq!(verdict(reply), Err(Refusal::Format(error)), "{reply:?}");
        }
    }

    #[test]
    fn a_framebuffer_larger_than_the_window_is_refused() {
        // One row past an exact fit.
        let reply = Reply {
            width: 1024,
            pitch: 4096,
            height: 4097,
            ..measured()
        };
        assert_eq!(verdict(reply), Err(Refusal::Window));
        // One pixel of pitch past an exact fit.
        let reply = Reply {
            width: 1024,
            pitch: 4100,
            height: 4096,
            ..measured()
        };
        assert_eq!(verdict(reply), Err(Refusal::Window));
        // The largest product two `u32`s make: no overflow, still refused.
        let reply = Reply {
            width: 1,
            pitch: 0xFFFF_FFFC,
            height: u32::MAX,
            ..measured()
        };
        assert_eq!(verdict(reply), Err(Refusal::Window));
    }

    #[test]
    fn the_window_is_the_one_passed_in() {
        // The measured framebuffer is 1 920 000 bytes.
        assert!(accept(&measured(), FB_VA, 1_920_000).is_ok());
        assert_eq!(accept(&measured(), FB_VA, 1_919_999), Err(Refusal::Window));
        assert_eq!(accept(&measured(), FB_VA, 0), Err(Refusal::Window));
    }

    #[test]
    fn a_screen_smaller_than_one_cell_is_refused() {
        // No column, no row, neither — each a legal `Format`.
        for (width, height) in [(7, 600), (800, 15), (7, 15), (1, 1)] {
            let reply = Reply {
                width,
                height,
                pitch: width * 4,
                ..measured()
            };
            assert_eq!(verdict(reply), Err(Refusal::Grid), "{width}x{height}");
        }
    }

    #[test]
    fn an_accepted_format_keeps_every_store_inside_the_window() {
        // The bound the bin's `store` rests on, at the corner furthest in:
        // the last pixel's four bytes end at or before `pitch * height`.
        for reply in [
            measured(),
            Reply {
                pitch: 4096,
                ..measured()
            },
            Reply {
                width: 1024,
                pitch: 4096,
                height: 4096,
                ..measured()
            },
            Reply {
                width: 8,
                height: 16,
                pitch: 32,
                ..measured()
            },
        ] {
            let fmt = verdict(reply).unwrap();
            let (x, y) = (u64::from(fmt.width() - 1), u64::from(fmt.height() - 1));
            let end = y * u64::from(fmt.pitch()) + x * 4 + 4;
            assert!(end <= fmt.byte_len(), "{reply:?}");
            assert!(fmt.byte_len() <= FB_WINDOW_SIZE, "{reply:?}");
        }
    }

    // --- the tokens -------------------------------------------------------

    #[test]
    fn every_refusal_is_one_distinct_token() {
        let all = [
            Refusal::Va,
            Refusal::Format(FormatError::Bpp),
            Refusal::Format(FormatError::Shift),
            Refusal::Format(FormatError::ShiftsOverlap),
            Refusal::Format(FormatError::ZeroSize),
            Refusal::Format(FormatError::Pitch),
            Refusal::Window,
            Refusal::Grid,
        ];
        for (i, a) in all.iter().enumerate() {
            let token = a.as_str();
            assert!(!token.is_empty());
            assert!(
                token.bytes().all(|b| b.is_ascii_graphic()),
                "{token:?} is not one token"
            );
            for b in &all[i + 1..] {
                assert_ne!(token, b.as_str());
            }
        }
        assert_eq!(Refusal::Va.as_str(), "va");
        assert_eq!(Refusal::Window.as_str(), "window");
        assert_eq!(Refusal::Grid.as_str(), "grid");
        assert_eq!(Refusal::Format(FormatError::Pitch).as_str(), "pitch");
    }
}
