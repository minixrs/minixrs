// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The pure half of virtio-blk's bring-up (slice 6.2, spec T11/T12): where the
//! queue, the request header, the status byte and the bounce buffer sit in the DMA
//! run, how the capacity is decoded, and the DMA cookie check. Host-tested; `main.rs` does the
//! kernel calls and register accesses around it.

// Only `main.rs`'s `not(test)`-gated paths reach most of this, so under
// `cargo test` (which drops `_start` and `main`) it would read as dead code.
#![cfg_attr(test, allow(dead_code))]

use crate::blk::REQ_HEADER_LEN;
use minixrs_driver_rt::{DmaBuf, DmaRegion, QueueLayout};
use minixrs_kernel_shared::message::USER_PAGE_SIZE;
use minixrs_kernel_shared::uspace::DMA_COOKIE;

/// The bit virtio-blk's interrupt notifications carry in the pending word.
pub const VBLK_IRQ_NOTIFY_ID: u8 = 0;
/// Optional features requested: none, so `negotiate` accepts `VIRTIO_F_VERSION_1`
/// alone. 6.4 adds `VIRTIO_BLK_F_FLUSH`.
pub const VBLK_WANTED_FEATURES: u64 = 0;
/// Offset of `capacity` (u64, 512-byte sectors) in the device configuration.
pub const VBLK_CONFIG_CAPACITY_OFF: usize = 0;
/// Page of the DMA run holding queue 0 (descriptor table, available and used rings).
pub const VBLK_QUEUE_PAGE: u64 = 0;
/// Page of the DMA run holding the request header and the status byte.
pub const VBLK_HEADER_PAGE: u64 = 1;
/// Page of the DMA run holding the one 4096-byte bounce buffer (H3).
pub const VBLK_DATA_PAGE: u64 = 2;

/// Offset of the request header in its page.
pub const VBLK_HEADER_OFF: usize = 0;
/// Offset of the status byte in the header page: right after the header.
pub const VBLK_STATUS_OFF: usize = VBLK_HEADER_OFF + REQ_HEADER_LEN;
/// Bytes of status the device writes back.
pub const VBLK_STATUS_LEN: usize = 1;

// Header and status share one page and must both fit in it.
const _: () = assert!((VBLK_STATUS_OFF + VBLK_STATUS_LEN) as u64 <= USER_PAGE_SIZE);

/// Where virtio-blk's four areas sit in its DMA run: the queue and the bounce
/// buffer on a page each, the request header and the status byte on a third.
#[derive(Clone, Copy, Debug)]
pub struct VblkLayout {
    /// The queue page (the `VirtQueue`'s memory).
    pub queue: DmaBuf,
    /// The request header: [`REQ_HEADER_LEN`] bytes the device reads.
    pub header: DmaBuf,
    /// The status byte the device writes, right after the header.
    pub status: DmaBuf,
    /// The bounce buffer: one whole page, a BDEV block.
    pub data: DmaBuf,
}

/// Lay virtio-blk out in `region`. `None` unless the run is page-aligned at both
/// ends of the translation, holds the three pages, and `queue` fits in page 0.
/// (A run that wraps either address space never became a `DmaRegion`.)
pub fn vblk_layout(region: &DmaRegion, queue: QueueLayout) -> Option<VblkLayout> {
    let run = region.run();
    if !run.va.is_multiple_of(USER_PAGE_SIZE) || !run.pa.is_multiple_of(USER_PAGE_SIZE) {
        return None;
    }
    if u64::try_from(queue.bytes).ok()? > USER_PAGE_SIZE {
        return None;
    }
    let page_len = usize::try_from(USER_PAGE_SIZE).ok()?;
    let page_off = |page: u64| usize::try_from(page.checked_mul(USER_PAGE_SIZE)?).ok();
    let header_page = page_off(VBLK_HEADER_PAGE)?;
    // `DmaRegion::buf` refuses a range the run does not wholly hold, so a run
    // shorter than the three pages is refused at the data page.
    Some(VblkLayout {
        queue: region.buf(page_off(VBLK_QUEUE_PAGE)?, page_len)?,
        header: region.buf(header_page.checked_add(VBLK_HEADER_OFF)?, REQ_HEADER_LEN)?,
        status: region.buf(header_page.checked_add(VBLK_STATUS_OFF)?, VBLK_STATUS_LEN)?,
        data: region.buf(page_off(VBLK_DATA_PAGE)?, page_len)?,
    })
}

/// The device's `capacity` field, decoded: little-endian 512-byte sectors.
pub const fn capacity_from_config(bytes: [u8; 8]) -> u64 {
    u64::from_le_bytes(bytes)
}

/// Whether `word`, read at the run's start, is the kernel's stamp for `pa`
/// (`pa ^ DMA_COOKIE`, spec T7) — i.e. the VA the driver holds and the PA it was
/// told name the same memory.
pub const fn cookie_ok(word: u64, pa: u64) -> bool {
    word == pa ^ DMA_COOKIE
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_driver_rt::{DmaRun, QUEUE_SIZE, queue_layout};

    const PA: u64 = 0x4800_0000;
    const PAGE: u64 = USER_PAGE_SIZE;
    /// Host pages behind each test region.
    const HOST_PAGES: u64 = 4;

    #[repr(C, align(4096))]
    struct Pages([u8; (HOST_PAGES * PAGE) as usize]);

    /// A region over `len` bytes of leaked, page-aligned host memory, starting
    /// `skip` bytes in, with physical address `pa`.
    fn region(skip: u64, pa: u64, len: u64) -> DmaRegion {
        assert!(skip + len <= HOST_PAGES * PAGE);
        // Leaked on purpose: a DMA run is never unmapped.
        let base = Box::into_raw(Box::new(Pages([0; (HOST_PAGES * PAGE) as usize])));
        let run = DmaRun {
            va: base.expose_provenance() as u64 + skip,
            pa,
            len,
        };
        // SAFETY: `skip..skip + len` lies inside the leaked allocation (asserted
        // above), which is writable, never freed, and has no reference to it.
        unsafe { DmaRegion::new(run) }.expect("a valid host run")
    }

    fn q8() -> QueueLayout {
        queue_layout(QUEUE_SIZE).expect("size 8 is a valid queue")
    }

    #[test]
    fn layout_places_queue_header_and_data_on_separate_pages() {
        let r = region(0, PA, HOST_PAGES * PAGE);
        let va = r.run().va;
        let l = vblk_layout(&r, q8()).expect("a four-page run holds three pages");
        assert_eq!((l.queue.pa(), l.queue.va()), (0x4800_0000, va));
        assert_eq!((l.header.pa(), l.header.va()), (0x4800_1000, va + 0x1000));
        assert_eq!((l.data.pa(), l.data.va()), (0x4800_2000, va + 0x2000));
        for page in [l.queue, l.data] {
            assert_eq!(u64::from(page.len()), PAGE);
        }
    }

    #[test]
    fn layout_sizes_the_header_and_puts_the_status_byte_right_after_it() {
        let r = region(0, PA, HOST_PAGES * PAGE);
        let va = r.run().va;
        let l = vblk_layout(&r, q8()).expect("a four-page run holds three pages");
        // Exactly the request header: the device is told to read 16 bytes.
        assert_eq!(l.header.len(), 16);
        assert_eq!(l.header.len() as usize, REQ_HEADER_LEN);
        // Exactly one byte, at offset 16 of the header page.
        assert_eq!(l.status.len(), 1);
        assert_eq!((l.status.pa(), l.status.va()), (0x4800_1010, va + 0x1010));
        // The two do not overlap, and neither reaches the data page.
        assert_eq!(l.header.pa() + u64::from(l.header.len()), l.status.pa());
        assert_eq!(l.status.pa() + 1, 0x4800_1011);
        assert!(l.status.pa() + u64::from(l.status.len()) <= l.data.pa());
    }

    #[test]
    fn layout_buffers_refuse_an_access_past_their_end() {
        let l = vblk_layout(&region(0, PA, HOST_PAGES * PAGE), q8()).unwrap();
        // A 17th header byte would land on the status byte; a second status
        // byte on whatever follows it.
        assert!(l.header.write_from(0, &[0u8; 16]));
        assert!(!l.header.write_from(0, &[0u8; 17]));
        assert!(l.status.write_from(0, &[0xFF]));
        assert!(!l.status.write_from(1, &[0]));
        let mut b = [0u8; 1];
        assert!(l.status.read_into(0, &mut b));
        assert_eq!(b, [0xFF]);
        // Writing the header leaves the status byte alone, and the reverse.
        assert!(l.header.write_from(0, &[0xAA; 16]));
        assert!(l.status.read_into(0, &mut b));
        assert_eq!(b, [0xFF]);
    }

    #[test]
    fn layout_rejects_a_two_page_run() {
        assert!(vblk_layout(&region(0, PA, 2 * PAGE), q8()).is_none());
        // Short of the status byte, though the queue page and the header fit.
        assert!(vblk_layout(&region(0, PA, PAGE + 16), q8()).is_none());
        // One byte short of the data page's end.
        assert!(vblk_layout(&region(0, PA, 3 * PAGE - 1), q8()).is_none());
        // Exactly three pages is enough.
        assert!(vblk_layout(&region(0, PA, 3 * PAGE), q8()).is_some());
    }

    #[test]
    fn layout_rejects_a_queue_larger_than_its_page() {
        let big = queue_layout(256).expect("256 is a valid size");
        assert!(big.bytes > 0x1000);
        assert!(vblk_layout(&region(0, PA, HOST_PAGES * PAGE), big).is_none());
    }

    #[test]
    fn layout_rejects_an_unaligned_run() {
        let len = 3 * PAGE;
        assert!(vblk_layout(&region(0, PA + 16, len), q8()).is_none());
        assert!(vblk_layout(&region(16, PA, len), q8()).is_none());
        assert!(vblk_layout(&region(16, PA + 16, len), q8()).is_none());
        assert!(vblk_layout(&region(PAGE, PA, len), q8()).is_some());
    }

    #[test]
    fn a_wrapping_run_never_becomes_a_region() {
        // 6.2's layout refused these itself; now `DmaRegion::new` does, from the
        // numbers alone, so `vblk_layout` cannot be handed one.
        for run in [
            DmaRun {
                va: 0xC000_0000,
                pa: u64::MAX - 0xFFF,
                len: 0x1_0000,
            },
            DmaRun {
                va: u64::MAX - 0xFFF,
                pa: PA,
                len: 0x1_0000,
            },
        ] {
            // SAFETY: both runs overflow, which `new` refuses in its documented
            // up-front checks before relying on the contract; nothing is accessed.
            assert!(unsafe { DmaRegion::new(run) }.is_none(), "{run:?}");
        }
    }

    #[test]
    fn capacity_is_little_endian() {
        assert_eq!(capacity_from_config([0x00, 0x0A, 0, 0, 0, 0, 0, 0]), 2560);
        assert_eq!(
            capacity_from_config([1, 2, 3, 4, 5, 6, 7, 8]),
            0x0807_0605_0403_0201
        );
    }

    #[test]
    fn cookie_matches_only_its_own_pa() {
        let pa = 0x4800_0000;
        assert!(cookie_ok(pa ^ DMA_COOKIE, pa));
        assert!(!cookie_ok(pa ^ DMA_COOKIE, pa + 0x1000));
        assert!(!cookie_ok((pa + 0x1000) ^ DMA_COOKIE, pa));
        // An unstamped (zeroed) page matches only a PA equal to DMA_COOKIE, which
        // is no RAM address.
        assert!(!cookie_ok(0, pa));
        assert!(!cookie_ok(pa, pa));
    }
}
