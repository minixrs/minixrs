// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The pure half of virtio-blk's bring-up (slice 6.2, spec T11/T12): where the
//! queue, the request header and the bounce buffer sit in the DMA run, how the
//! capacity is decoded, and the DMA cookie check. Host-tested; `main.rs` does the
//! kernel calls and register accesses around it.

// Only `main.rs`'s `not(test)`-gated paths reach most of this, so under
// `cargo test` (which drops `_start` and `main`) it would read as dead code.
#![cfg_attr(test, allow(dead_code))]

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

/// Where virtio-blk's three areas sit in its DMA run: one whole page each.
// `header` and `data` are read once the driver serves requests (slice 6.3, Task 5);
// bring-up alone uses only `queue`.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct VblkLayout {
    /// The queue page (the `VirtQueue`'s memory).
    pub queue: DmaBuf,
    /// The request header page.
    pub header: DmaBuf,
    /// The bounce-buffer page.
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
    // `DmaRegion::buf` refuses a page the run does not wholly hold, so a run
    // shorter than the three pages is refused at the data page.
    let page = |page: u64| {
        let offset = usize::try_from(page.checked_mul(USER_PAGE_SIZE)?).ok()?;
        region.buf(offset, page_len)
    };
    Some(VblkLayout {
        queue: page(VBLK_QUEUE_PAGE)?,
        header: page(VBLK_HEADER_PAGE)?,
        data: page(VBLK_DATA_PAGE)?,
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
        for page in [l.queue, l.header, l.data] {
            assert_eq!(u64::from(page.len()), PAGE);
        }
    }

    #[test]
    fn layout_rejects_a_two_page_run() {
        assert!(vblk_layout(&region(0, PA, 2 * PAGE), q8()).is_none());
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
