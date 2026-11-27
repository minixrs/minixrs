// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Whether the kernel will use the framebuffer the bootloader reported.
//!
//! `kernel/src/` is bare-metal only and carries no `#[cfg(test)]`, so the
//! decision — slice 6.7's F2, every condition of it — is the pure function
//! [`validate`], over plain integers the kernel copies out of Limine's
//! response. The kernel's half is reading the response and acting on the
//! answer: the allocator carve ([`crate::memrange::subtract`] over
//! [`Framebuffer::range`]), the mapping into TTY at [`crate::uspace::FB_VA`],
//! and the `SYS_GETINFO(GET_FRAMEBUFFER)` reply.
//!
//! Nothing here is an ABI: [`RawFramebuffer`] and [`Framebuffer`] never cross a
//! process boundary (the reply's wire layout is `callnr`'s `GETINFO_FB_*_OFF`),
//! and nothing is emitted into the generated C headers.

use crate::memrange::Range;
use crate::message::USER_PAGE_SIZE;
use crate::uspace::FB_WINDOW_SIZE;

/// Bits per pixel of the one format the kernel accepts, and the value the
/// `GET_FRAMEBUFFER` reply carries at `GETINFO_FB_BPP_OFF`.
pub const FB_BPP: u8 = 32;

/// Limine's `LIMINE_FRAMEBUFFER_RGB` memory model.
pub const MEMORY_MODEL_RGB: u8 = 1;

/// Width of each colour channel the kernel accepts, in bits.
pub const CHANNEL_BITS: u8 = 8;

/// The fields of the bootloader's framebuffer description that [`validate`]
/// judges, exactly as reported — nothing here has been checked.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RawFramebuffer {
    /// The framebuffer's address in the bootloader's higher-half direct map.
    pub address: u64,
    /// Width in pixels.
    pub width: u64,
    /// Height in pixels.
    pub height: u64,
    /// Bytes per row.
    pub pitch: u64,
    /// Bits per pixel.
    pub bpp: u16,
    /// Memory model; [`MEMORY_MODEL_RGB`] is the only one accepted.
    pub memory_model: u8,
    pub red_mask_size: u8,
    pub red_mask_shift: u8,
    pub green_mask_size: u8,
    pub green_mask_shift: u8,
    pub blue_mask_size: u8,
    pub blue_mask_shift: u8,
}

/// A framebuffer [`validate`] accepted.
///
/// Every field satisfies F2: 32 bits per pixel, three 8-bit channels at
/// distinct byte-aligned shifts, `width * 4 <= pitch`, `pitch` a multiple of
/// four, `pa` page-aligned, and `pages` whole pages — at most the framebuffer
/// window's — ending at or below `u64::MAX`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Framebuffer {
    /// Physical address of the first pixel; page-aligned.
    pub pa: u64,
    /// Width in pixels; non-zero.
    pub width: u32,
    /// Height in pixels; non-zero.
    pub height: u32,
    /// Bytes per row.
    pub pitch: u32,
    /// Bit offset of the red channel within a pixel: 0, 8, 16 or 24.
    pub red_shift: u8,
    /// Bit offset of the green channel.
    pub green_shift: u8,
    /// Bit offset of the blue channel.
    pub blue_shift: u8,
    /// `ceil(pitch * height / 4096)`: the pages the kernel maps and carves.
    pub pages: u64,
}

impl Framebuffer {
    /// The framebuffer's physical range rounded **outward** to whole pages,
    /// `[pa, pa + pages * 4096)` — what is removed from the frame allocator and
    /// what is mapped into TTY. The last page may extend past `pitch * height`.
    ///
    /// Cannot overflow for a value [`validate`] returned, which proved the sum;
    /// for a hand-built value that breaks that, it saturates rather than wraps,
    /// so the range grows instead of pointing somewhere else.
    #[must_use]
    pub const fn range(&self) -> Range {
        let len = self.pages.saturating_mul(USER_PAGE_SIZE);
        (self.pa, self.pa.saturating_add(len))
    }
}

/// Why [`validate`] refused a framebuffer: one variant per condition of F2, in
/// the order the conditions are tested.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Reject {
    /// `bpp` is not 32.
    Bpp,
    /// The memory model is not RGB.
    Model,
    /// A channel's mask is not 8 bits wide.
    MaskSize,
    /// A channel's shift is not one of 0, 8, 16, 24, or two channels share one.
    Shift,
    /// `width` or `height` is zero.
    Size,
    /// `pitch` is below `width * 4` or is not a multiple of four.
    Pitch,
    /// `address` is not in the higher-half direct map, or the framebuffer's end
    /// is not a representable physical address.
    Address,
    /// The physical address is not page-aligned.
    Align,
    /// `pitch * height`, rounded up to pages, does not fit the framebuffer
    /// window (or does not fit `u64` at all).
    Window,
}

impl Reject {
    /// The one-token name the kernel prints after `[fb] FAIL `.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Reject::Bpp => "bpp",
            Reject::Model => "model",
            Reject::MaskSize => "mask-size",
            Reject::Shift => "shift",
            Reject::Size => "size",
            Reject::Pitch => "pitch",
            Reject::Address => "address",
            Reject::Align => "align",
            Reject::Window => "window",
        }
    }
}

/// True when `shift` puts an 8-bit channel on a byte of a 32-bit pixel.
const fn shift_ok(shift: u8) -> bool {
    matches!(shift, 0 | 8 | 16 | 24)
}

/// One past the highest physical address a page descriptor can name: 48 bits.
///
/// The kernel's mapper masks a physical address to those bits when it writes a
/// leaf, so a framebuffer page above this would be mapped at a different
/// address without any assert firing. [`validate`] refuses a range that ends
/// past it.
pub const PA_LIMIT: u64 = 1 << 48;

/// Decides whether the kernel uses `raw`, the first framebuffer the bootloader
/// reported, given the direct map's offset.
///
/// The conditions are F2's, tested in this order, and the first that fails is
/// the answer:
///
/// 1. `bpp == 32`; the memory model is RGB; each mask is 8 bits; each shift is
///    0, 8, 16 or 24 and the three are pairwise distinct;
/// 2. `width` and `height` are non-zero; `pitch >= width * 4` and `pitch` is a
///    multiple of four;
/// 3. `address >= hhdm_offset`, and the physical address `address -
///    hhdm_offset` is page-aligned;
/// 4. `pitch * height`, rounded up to pages, fits
///    [`FB_WINDOW_SIZE`](crate::uspace::FB_WINDOW_SIZE).
///
/// Every product and sum is checked. Two consequences are checked rather than
/// assumed: the page-rounded end `pa + pages * 4096` must not overflow or pass
/// [`PA_LIMIT`] ([`Reject::Address`]), and width, height and pitch must fit the reply's
/// `u32` fields (implied by 2 and 4, since a non-zero height bounds the pitch
/// by the window and the pitch bounds the width; [`Reject::Window`] if not).
///
/// A caller with no direct-map offset to give must not call this with zero:
/// every address is at or above zero, so condition 3 would pass vacuously.
pub fn validate(raw: &RawFramebuffer, hhdm_offset: u64) -> Result<Framebuffer, Reject> {
    if raw.bpp != FB_BPP as u16 {
        return Err(Reject::Bpp);
    }
    if raw.memory_model != MEMORY_MODEL_RGB {
        return Err(Reject::Model);
    }
    if raw.red_mask_size != CHANNEL_BITS
        || raw.green_mask_size != CHANNEL_BITS
        || raw.blue_mask_size != CHANNEL_BITS
    {
        return Err(Reject::MaskSize);
    }
    let (r, g, b) = (
        raw.red_mask_shift,
        raw.green_mask_shift,
        raw.blue_mask_shift,
    );
    if !shift_ok(r) || !shift_ok(g) || !shift_ok(b) || r == g || g == b || r == b {
        return Err(Reject::Shift);
    }

    if raw.width == 0 || raw.height == 0 {
        return Err(Reject::Size);
    }
    let min_pitch = raw.width.checked_mul(4).ok_or(Reject::Pitch)?;
    if raw.pitch < min_pitch || !raw.pitch.is_multiple_of(4) {
        return Err(Reject::Pitch);
    }

    let pa = raw
        .address
        .checked_sub(hhdm_offset)
        .ok_or(Reject::Address)?;
    if !pa.is_multiple_of(USER_PAGE_SIZE) {
        return Err(Reject::Align);
    }

    let bytes = raw.pitch.checked_mul(raw.height).ok_or(Reject::Window)?;
    // The window is a whole number of pages (`uspace` asserts it), so the bytes
    // fit it exactly when their page-rounded length does.
    if bytes > FB_WINDOW_SIZE {
        return Err(Reject::Window);
    }
    let pages = bytes.div_ceil(USER_PAGE_SIZE);
    let len = pages.checked_mul(USER_PAGE_SIZE).ok_or(Reject::Window)?;
    match pa.checked_add(len) {
        Some(end) if end <= PA_LIMIT => {}
        _ => return Err(Reject::Address),
    }

    Ok(Framebuffer {
        pa,
        width: u32::try_from(raw.width).map_err(|_| Reject::Window)?,
        height: u32::try_from(raw.height).map_err(|_| Reject::Window)?,
        pitch: u32::try_from(raw.pitch).map_err(|_| Reject::Window)?,
        red_shift: r,
        green_shift: g,
        blue_shift: b,
        pages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HHDM: u64 = 0xFFFF_0000_0000_0000;

    /// What QEMU 11.0.3's `ramfb` under edk2 and Limine reported (slice 6.7,
    /// Task 0).
    const MEASURED: RawFramebuffer = RawFramebuffer {
        address: HHDM + 0x4c7a_0000,
        width: 800,
        height: 600,
        pitch: 3200,
        bpp: 32,
        memory_model: MEMORY_MODEL_RGB,
        red_mask_size: 8,
        red_mask_shift: 16,
        green_mask_size: 8,
        green_mask_shift: 8,
        blue_mask_size: 8,
        blue_mask_shift: 0,
    };

    #[test]
    fn the_measured_framebuffer_is_accepted() {
        let fb = validate(&MEASURED, HHDM).expect("accepted");
        assert_eq!(
            fb,
            Framebuffer {
                pa: 0x4c7a_0000,
                width: 800,
                height: 600,
                pitch: 3200,
                red_shift: 16,
                green_shift: 8,
                blue_shift: 0,
                pages: 469,
            }
        );
    }

    #[test]
    fn the_range_is_rounded_outward_to_whole_pages() {
        // 3200 * 600 = 0x1d4c00 bytes: 468 pages and 0xc00 bytes more.
        let fb = validate(&MEASURED, HHDM).expect("accepted");
        assert_eq!(fb.range(), (0x4c7a_0000, 0x4c7a_0000 + 469 * 4096));
        assert_eq!(fb.range().1, 0x4c97_5000);
    }

    #[test]
    fn a_whole_page_length_is_not_rounded_further() {
        let raw = RawFramebuffer {
            width: 1024,
            height: 768,
            pitch: 4096,
            ..MEASURED
        };
        assert_eq!(validate(&raw, HHDM).expect("accepted").pages, 768);
    }

    #[test]
    fn one_byte_over_a_page_takes_another_page() {
        // 4 * 1025 = 4100 bytes: one page and four bytes.
        let raw = RawFramebuffer {
            width: 1,
            height: 1025,
            pitch: 4,
            ..MEASURED
        };
        assert_eq!(validate(&raw, HHDM).expect("accepted").pages, 2);
    }

    #[test]
    fn a_range_built_by_hand_saturates_instead_of_wrapping() {
        let fb = Framebuffer {
            pa: u64::MAX - 0xFFF,
            pages: u64::MAX,
            ..validate(&MEASURED, HHDM).expect("accepted")
        };
        assert_eq!(fb.range(), (u64::MAX - 0xFFF, u64::MAX));
    }

    #[test]
    fn only_32_bits_per_pixel_is_accepted() {
        for bpp in [0, 8, 15, 16, 24, 31, 33, 64] {
            let raw = RawFramebuffer { bpp, ..MEASURED };
            assert_eq!(validate(&raw, HHDM), Err(Reject::Bpp), "bpp={bpp}");
        }
    }

    #[test]
    fn only_the_rgb_memory_model_is_accepted() {
        for memory_model in [0, 2, 255] {
            let raw = RawFramebuffer {
                memory_model,
                ..MEASURED
            };
            assert_eq!(validate(&raw, HHDM), Err(Reject::Model));
        }
    }

    #[test]
    fn each_mask_must_be_eight_bits() {
        let cases = [
            RawFramebuffer {
                red_mask_size: 5,
                ..MEASURED
            },
            RawFramebuffer {
                green_mask_size: 6,
                ..MEASURED
            },
            RawFramebuffer {
                blue_mask_size: 10,
                ..MEASURED
            },
        ];
        for raw in cases {
            assert_eq!(validate(&raw, HHDM), Err(Reject::MaskSize));
        }
    }

    #[test]
    fn each_shift_must_be_on_a_byte() {
        let cases = [
            RawFramebuffer {
                red_mask_shift: 4,
                ..MEASURED
            },
            RawFramebuffer {
                green_mask_shift: 9,
                ..MEASURED
            },
            RawFramebuffer {
                blue_mask_shift: 32,
                ..MEASURED
            },
        ];
        for raw in cases {
            assert_eq!(validate(&raw, HHDM), Err(Reject::Shift));
        }
    }

    #[test]
    fn the_shifts_must_be_pairwise_distinct() {
        // (r, g, b): each pair equal in turn.
        for (r, g, b) in [(8, 8, 0), (16, 0, 0), (24, 8, 24)] {
            let raw = RawFramebuffer {
                red_mask_shift: r,
                green_mask_shift: g,
                blue_mask_shift: b,
                ..MEASURED
            };
            assert_eq!(validate(&raw, HHDM), Err(Reject::Shift), "{r},{g},{b}");
        }
    }

    #[test]
    fn every_distinct_byte_order_is_accepted() {
        // BGRX and a format with the padding byte at the bottom.
        for (r, g, b) in [(0, 8, 16), (24, 16, 8), (8, 24, 0)] {
            let raw = RawFramebuffer {
                red_mask_shift: r,
                green_mask_shift: g,
                blue_mask_shift: b,
                ..MEASURED
            };
            let fb = validate(&raw, HHDM).expect("accepted");
            assert_eq!((fb.red_shift, fb.green_shift, fb.blue_shift), (r, g, b));
        }
    }

    #[test]
    fn a_zero_width_or_height_is_refused() {
        let no_width = RawFramebuffer {
            width: 0,
            ..MEASURED
        };
        let no_height = RawFramebuffer {
            height: 0,
            ..MEASURED
        };
        assert_eq!(validate(&no_width, HHDM), Err(Reject::Size));
        assert_eq!(validate(&no_height, HHDM), Err(Reject::Size));
    }

    #[test]
    fn the_pitch_must_cover_the_width() {
        let short = RawFramebuffer {
            pitch: 3196,
            ..MEASURED
        };
        assert_eq!(validate(&short, HHDM), Err(Reject::Pitch));
        let exact = RawFramebuffer {
            pitch: 3200,
            ..MEASURED
        };
        assert!(validate(&exact, HHDM).is_ok());
    }

    #[test]
    fn a_pitch_wider_than_the_width_is_accepted() {
        let raw = RawFramebuffer {
            pitch: 4096,
            ..MEASURED
        };
        let fb = validate(&raw, HHDM).expect("accepted");
        assert_eq!((fb.width, fb.pitch, fb.pages), (800, 4096, 600));
    }

    #[test]
    fn the_pitch_must_be_a_multiple_of_four() {
        for pitch in [3201, 3202, 3203] {
            let raw = RawFramebuffer { pitch, ..MEASURED };
            assert_eq!(validate(&raw, HHDM), Err(Reject::Pitch), "pitch={pitch}");
        }
    }

    #[test]
    fn a_width_whose_minimum_pitch_overflows_is_refused() {
        let raw = RawFramebuffer {
            width: u64::MAX / 4 + 1,
            pitch: u64::MAX - 3,
            ..MEASURED
        };
        assert_eq!(validate(&raw, HHDM), Err(Reject::Pitch));
    }

    #[test]
    fn an_address_below_the_direct_map_is_refused() {
        let raw = RawFramebuffer {
            address: HHDM - 0x1000,
            ..MEASURED
        };
        assert_eq!(validate(&raw, HHDM), Err(Reject::Address));
        let physical = RawFramebuffer {
            address: 0x4c7a_0000,
            ..MEASURED
        };
        assert_eq!(validate(&physical, HHDM), Err(Reject::Address));
    }

    #[test]
    fn the_first_byte_of_the_direct_map_is_an_address_in_it() {
        let raw = RawFramebuffer {
            address: HHDM,
            ..MEASURED
        };
        assert_eq!(validate(&raw, HHDM).expect("accepted").pa, 0);
    }

    #[test]
    fn an_unaligned_physical_address_is_refused() {
        for off in [1, 0x800, 0xFFF] {
            let raw = RawFramebuffer {
                address: HHDM + 0x4c7a_0000 + off,
                ..MEASURED
            };
            assert_eq!(validate(&raw, HHDM), Err(Reject::Align), "off={off:#x}");
        }
    }

    #[test]
    fn a_framebuffer_filling_the_window_exactly_is_accepted() {
        // 4096 * 4096 bytes is the whole 16 MiB window.
        let raw = RawFramebuffer {
            width: 1024,
            height: 4096,
            pitch: 4096,
            ..MEASURED
        };
        let fb = validate(&raw, HHDM).expect("accepted");
        assert_eq!(fb.pages * USER_PAGE_SIZE, FB_WINDOW_SIZE);
    }

    #[test]
    fn one_row_past_the_window_is_refused() {
        let raw = RawFramebuffer {
            width: 1024,
            height: 4097,
            pitch: 4096,
            ..MEASURED
        };
        assert_eq!(validate(&raw, HHDM), Err(Reject::Window));
    }

    #[test]
    fn four_bytes_past_the_window_is_refused() {
        // 4 * (4 MiB + 1) bytes: the window and one pixel more, which still
        // rounds to one page over.
        let raw = RawFramebuffer {
            width: 1,
            height: FB_WINDOW_SIZE / 4 + 1,
            pitch: 4,
            ..MEASURED
        };
        assert_eq!(validate(&raw, HHDM), Err(Reject::Window));
    }

    #[test]
    fn a_length_that_overflows_is_refused() {
        let raw = RawFramebuffer {
            width: 1,
            height: u64::MAX,
            pitch: 8,
            ..MEASURED
        };
        assert_eq!(validate(&raw, HHDM), Err(Reject::Window));
    }

    #[test]
    fn an_end_past_the_top_of_the_address_space_is_refused() {
        // Page-aligned, one page below the top, and 469 pages long.
        let raw = RawFramebuffer {
            address: u64::MAX - 0xFFF,
            ..MEASURED
        };
        assert_eq!(validate(&raw, 0), Err(Reject::Address));
        // The last page of the address space would need `end == 2^64`.
        let one_page = RawFramebuffer {
            address: u64::MAX - 0xFFF,
            width: 1,
            height: 1,
            pitch: 4,
            ..MEASURED
        };
        assert_eq!(validate(&one_page, 0), Err(Reject::Address));
    }

    #[test]
    fn an_end_past_the_page_tables_physical_reach_is_refused() {
        // A page descriptor holds 48 bits of physical address and the mapper
        // masks the rest away, so a page past 2^48 would be mapped somewhere
        // else. 469 pages ending exactly at the limit are the last accepted.
        let last = PA_LIMIT - 469 * USER_PAGE_SIZE;
        let fits = RawFramebuffer {
            address: last,
            ..MEASURED
        };
        assert_eq!(validate(&fits, 0).map(|fb| fb.pa), Ok(last));
        let crosses = RawFramebuffer {
            address: last + USER_PAGE_SIZE,
            ..MEASURED
        };
        assert_eq!(validate(&crosses, 0), Err(Reject::Address));
        // A kernel-half address under the measured direct-map offset: the
        // physical address is 1 MiB below the limit and 469 pages are longer.
        let wrapped = RawFramebuffer {
            address: 0xFFFF_FFFF_FFF0_0000,
            ..MEASURED
        };
        assert_eq!(validate(&wrapped, HHDM), Err(Reject::Address));
    }

    #[test]
    fn the_first_failing_condition_is_the_one_reported() {
        // Everything wrong at once: `bpp` is tested first.
        let raw = RawFramebuffer {
            address: 1,
            width: 0,
            height: 0,
            pitch: 1,
            bpp: 24,
            memory_model: 0,
            red_mask_size: 5,
            red_mask_shift: 3,
            green_mask_size: 6,
            green_mask_shift: 3,
            blue_mask_size: 5,
            blue_mask_shift: 3,
        };
        assert_eq!(validate(&raw, HHDM), Err(Reject::Bpp));
        let raw = RawFramebuffer { bpp: 32, ..raw };
        assert_eq!(validate(&raw, HHDM), Err(Reject::Model));
        let raw = RawFramebuffer {
            memory_model: MEMORY_MODEL_RGB,
            ..raw
        };
        assert_eq!(validate(&raw, HHDM), Err(Reject::MaskSize));
    }

    #[test]
    fn every_reject_has_its_own_one_token_name() {
        let all = [
            Reject::Bpp,
            Reject::Model,
            Reject::MaskSize,
            Reject::Shift,
            Reject::Size,
            Reject::Pitch,
            Reject::Address,
            Reject::Align,
            Reject::Window,
        ];
        for (i, a) in all.iter().enumerate() {
            assert!(!a.token().is_empty());
            assert!(!a.token().contains(' '), "{:?} is not one token", a);
            for b in &all[i + 1..] {
                assert_ne!(a.token(), b.token());
            }
        }
    }
}
