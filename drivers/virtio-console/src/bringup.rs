// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The pure half of virtio-console's bring-up (slice 6.6, spec C3/C5): where the
//! two queues and the two buffers sit in the DMA run, the feature word it asks
//! for, and the DMA cookie check. Host-tested; `main.rs` does the kernel calls
//! and register accesses around it.

// Only `main.rs`'s `not(test)`-gated paths reach most of this, so under
// `cargo test` (which drops `_start` and `main`) it would read as dead code.
#![cfg_attr(test, allow(dead_code))]

use minixrs_driver_rt::{DmaBuf, DmaRegion, QueueLayout, QueueMem};
use minixrs_kernel_shared::callnr::CDEV_MAX_IO;
use minixrs_kernel_shared::message::USER_PAGE_SIZE;
use minixrs_kernel_shared::uspace::DMA_COOKIE;

/// The bit virtio-console's interrupt notifications carry in the pending word.
pub const VCON_IRQ_NOTIFY_ID: u8 = 0;
/// Optional features requested: none (spec C3). `negotiate` then accepts
/// `VIRTIO_F_VERSION_1` alone, and with no `F_SIZE`, `F_MULTIPORT` or
/// `F_EMERG_WRITE` no field of the device configuration is valid, so none is
/// read.
pub const VCON_WANTED_FEATURES: u64 = 0;
/// Queue index of `receiveq`, port 0's device-to-driver queue.
pub const RECEIVE_QUEUE: u16 = 0;
/// Queue index of `transmitq`, port 0's driver-to-device queue.
pub const TRANSMIT_QUEUE: u16 = 1;

/// Page of the DMA run holding `receiveq` (descriptor table and both rings).
pub const VCON_RXQ_PAGE: u64 = 0;
/// Page of the DMA run holding `transmitq`.
pub const VCON_TXQ_PAGE: u64 = 1;
/// Page of the DMA run holding the receive buffer (the device writes it).
pub const VCON_RX_PAGE: u64 = 2;
/// Page of the DMA run holding the transmit buffer (the device reads it).
pub const VCON_TX_PAGE: u64 = 3;
/// Pages of the DMA run the layout uses; a shorter run is refused.
pub const VCON_PAGES: u64 = 4;
/// Bytes in each of the two buffers: one CDEV request's worth.
pub const VCON_BUF_LEN: usize = CDEV_MAX_IO;

// The two queues are split off the front of the run, receiveq first, so their
// pages must be the first two, in that order, and both buffers' pages must come
// after them, on a page each, inside the run.
const _: () = assert!(VCON_RXQ_PAGE == 0 && VCON_TXQ_PAGE == 1);
const _: () = assert!(VCON_RX_PAGE > VCON_TXQ_PAGE && VCON_TX_PAGE > VCON_TXQ_PAGE);
const _: () = assert!(VCON_RX_PAGE != VCON_TX_PAGE);
const _: () = assert!(VCON_RX_PAGE < VCON_PAGES && VCON_TX_PAGE < VCON_PAGES);
// A buffer starts at its page's first byte and must end inside that page.
const _: () = assert!(VCON_BUF_LEN as u64 <= USER_PAGE_SIZE);

/// Where virtio-console's four areas sit in its DMA run: a page each.
///
/// Not `Copy` or `Clone`: `rxq` and `txq` are the one handle on each queue page
/// and move into `VirtQueue::new`. The two buffers are `Copy` and come from the
/// rest of the run, so neither can reach a queue page.
#[derive(Debug)]
pub struct VconLayout {
    /// `receiveq`'s page (queue [`RECEIVE_QUEUE`]).
    pub rxq: QueueMem,
    /// `transmitq`'s page (queue [`TRANSMIT_QUEUE`]).
    pub txq: QueueMem,
    /// The receive buffer: [`VCON_BUF_LEN`] bytes the device writes.
    pub rx: DmaBuf,
    /// The transmit buffer: [`VCON_BUF_LEN`] bytes the device reads.
    pub tx: DmaBuf,
}

/// Lay virtio-console out in `region`, consuming it. `None` unless the run is
/// page-aligned at both ends of the translation, holds all [`VCON_PAGES`]
/// pages, each of `rxq` and `txq` fits in one page, and no buffer has been
/// handed out of `region` yet. Both queue pages are split off before either
/// buffer is taken, which is what keeps every buffer away from both rings.
pub fn vcon_layout(region: DmaRegion, rxq: QueueLayout, txq: QueueLayout) -> Option<VconLayout> {
    let run = region.run();
    if !run.va.is_multiple_of(USER_PAGE_SIZE) || !run.pa.is_multiple_of(USER_PAGE_SIZE) {
        return None;
    }
    if run.len < VCON_PAGES.checked_mul(USER_PAGE_SIZE)? {
        return None;
    }
    if u64::try_from(rxq.bytes).ok()? > USER_PAGE_SIZE
        || u64::try_from(txq.bytes).ok()? > USER_PAGE_SIZE
    {
        return None;
    }
    let page_len = usize::try_from(USER_PAGE_SIZE).ok()?;
    // Pages 0 and 1 of the run (`VCON_RXQ_PAGE`, `VCON_TXQ_PAGE`, asserted
    // above) become the queues'; `rest` then starts at page 2, so a page of the
    // run is two pages earlier in it.
    let (rxq_mem, rest) = region.split_queue(page_len)?;
    let (txq_mem, rest) = rest.split_queue(page_len)?;
    let page_off = |page: u64| {
        let in_rest = page.checked_sub(VCON_TXQ_PAGE.checked_add(1)?)?;
        usize::try_from(in_rest.checked_mul(USER_PAGE_SIZE)?).ok()
    };
    // `DmaRegion::buf` refuses a range the remainder does not wholly hold.
    Some(VconLayout {
        rxq: rxq_mem,
        txq: txq_mem,
        rx: rest.buf(page_off(VCON_RX_PAGE)?, VCON_BUF_LEN)?,
        tx: rest.buf(page_off(VCON_TX_PAGE)?, VCON_BUF_LEN)?,
    })
}

/// Whether `word`, read at the run's start, is the kernel's stamp for `pa`
/// (`pa ^ DMA_COOKIE`) — i.e. the VA the driver holds and the PA it was told
/// name the same memory.
// A copy of `virtio-blk`'s `bringup::cookie_ok`, duplicated on purpose: three
// lines are not worth an edit to `virtio-blk` or `driver-rt` in this slice.
pub const fn cookie_ok(word: u64, pa: u64) -> bool {
    word == pa ^ DMA_COOKIE
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_driver_rt::transport::accept_features;
    use minixrs_driver_rt::{DmaRun, QUEUE_SIZE, queue_layout};

    const PA: u64 = 0x4800_0000;
    const PAGE: u64 = USER_PAGE_SIZE;
    /// Host pages behind each test region.
    const HOST_PAGES: u64 = 6;

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

    fn layout(r: DmaRegion) -> Option<VconLayout> {
        vcon_layout(r, q8(), q8())
    }

    /// Whether `[a, a + a_len)` and `[b, b + b_len)` share a byte.
    fn overlap(a: u64, a_len: u32, b: u64, b_len: u32) -> bool {
        a < b + u64::from(b_len) && b < a + u64::from(a_len)
    }

    #[test]
    fn the_page_size_is_4096_and_a_buffer_is_256_bytes() {
        // Every explicit number below rests on these two.
        assert_eq!(USER_PAGE_SIZE, 4096);
        assert_eq!(VCON_BUF_LEN, 256);
        assert_eq!(VCON_PAGES * USER_PAGE_SIZE, 16384);
    }

    #[test]
    fn layout_puts_each_queue_on_its_own_page() {
        let r = region(0, PA, HOST_PAGES * PAGE);
        let va = r.run().va;
        let l = layout(r).expect("a six-page run holds four pages");
        assert_eq!((l.rxq.pa(), l.rxq.va()), (0x4800_0000, va));
        assert_eq!((l.txq.pa(), l.txq.va()), (0x4800_1000, va + 0x1000));
        assert_eq!(l.rxq.len(), 4096);
        assert_eq!(l.txq.len(), 4096);
        // receiveq ends exactly where transmitq starts.
        assert_eq!(l.rxq.pa() + u64::from(l.rxq.len()), l.txq.pa());
        assert!(!overlap(l.rxq.pa(), l.rxq.len(), l.txq.pa(), l.txq.len()));
    }

    #[test]
    fn layout_puts_each_buffer_on_its_own_page_after_the_queues() {
        let r = region(0, PA, HOST_PAGES * PAGE);
        let va = r.run().va;
        let l = layout(r).unwrap();
        assert_eq!((l.rx.pa(), l.rx.va()), (0x4800_2000, va + 0x2000));
        assert_eq!((l.tx.pa(), l.tx.va()), (0x4800_3000, va + 0x3000));
        assert_eq!(l.rx.len(), 256);
        assert_eq!(l.tx.len(), 256);
    }

    #[test]
    fn neither_buffer_overlaps_a_queue_page_or_the_other_buffer() {
        let l = layout(region(0, PA, HOST_PAGES * PAGE)).unwrap();
        for buf in [l.rx, l.tx] {
            for q in [&l.rxq, &l.txq] {
                assert!(
                    !overlap(buf.pa(), buf.len(), q.pa(), q.len()),
                    "{buf:?} {q:?}"
                );
                assert!(
                    !overlap(buf.va(), buf.len(), q.va(), q.len()),
                    "{buf:?} {q:?}"
                );
            }
            // Past the end of the second queue page, by number.
            assert!(buf.pa() >= 0x4800_2000, "{buf:?}");
        }
        assert!(!overlap(l.rx.pa(), l.rx.len(), l.tx.pa(), l.tx.len()));
        assert!(!overlap(l.rx.va(), l.rx.len(), l.tx.va(), l.tx.len()));
        // The receive buffer ends 0x100 into page 2; transmit starts at page 3.
        assert_eq!(l.rx.pa() + u64::from(l.rx.len()), 0x4800_2100);
        assert!(l.rx.pa() + u64::from(l.rx.len()) <= l.tx.pa());
    }

    #[test]
    fn layout_keeps_the_placement_relative_to_the_run() {
        // receiveq = run page 0, transmitq = page 1, receive buffer = page 2,
        // transmit buffer = page 3, wherever the run starts: here one page into
        // the host memory, at another PA.
        let pa = PA + 0x7000;
        let r = region(PAGE, pa, 4 * PAGE);
        let run = r.run();
        assert_eq!(run.pa, pa);
        let l = layout(r).unwrap();
        assert_eq!((l.rxq.pa(), l.rxq.va()), (run.pa, run.va));
        assert_eq!((l.txq.pa(), l.txq.va()), (run.pa + 4096, run.va + 4096));
        assert_eq!((l.rx.pa(), l.rx.va()), (run.pa + 8192, run.va + 8192));
        assert_eq!((l.tx.pa(), l.tx.va()), (run.pa + 12288, run.va + 12288));
        // The same numbers through the page constants.
        assert_eq!(l.rxq.pa(), run.pa + VCON_RXQ_PAGE * PAGE);
        assert_eq!(l.txq.pa(), run.pa + VCON_TXQ_PAGE * PAGE);
        assert_eq!(l.rx.pa(), run.pa + VCON_RX_PAGE * PAGE);
        assert_eq!(l.tx.pa(), run.pa + VCON_TX_PAGE * PAGE);
    }

    #[test]
    fn layout_buffers_refuse_an_access_past_their_end() {
        let l = layout(region(0, PA, HOST_PAGES * PAGE)).unwrap();
        for buf in [l.rx, l.tx] {
            assert!(buf.write_from(0, &[0xAA; 256]));
            assert!(!buf.write_from(0, &[0u8; 257]));
            assert!(!buf.write_from(256, &[0]));
        }
        // Writing one leaves the other alone.
        assert!(l.tx.write_from(0, &[0x55; 256]));
        let mut b = [0u8; 256];
        assert!(l.rx.read_into(0, &mut b));
        assert_eq!(b, [0xAA; 256]);
    }

    #[test]
    fn layout_refuses_a_region_that_already_lent_a_buffer() {
        // Any handle made before the splits could cover a queue page.
        let r = region(0, PA, HOST_PAGES * PAGE);
        assert!(r.buf(0, 8).is_some());
        assert!(layout(r).is_none());
        // A peek is not a handle: the cookie check leaves the region whole.
        let r = region(0, PA, HOST_PAGES * PAGE);
        let mut word = [0u8; 8];
        assert!(r.peek(0, &mut word));
        assert!(layout(r).is_some());
    }

    #[test]
    fn layout_rejects_a_three_page_run() {
        assert!(layout(region(0, PA, 3 * PAGE)).is_none());
        // receiveq's page alone, and both queue pages with nothing after them.
        assert!(layout(region(0, PA, PAGE)).is_none());
        assert!(layout(region(0, PA, 2 * PAGE)).is_none());
        // Three pages and the transmit buffer's 256 bytes: the bytes would fit,
        // but the layout owns whole pages.
        assert!(layout(region(0, PA, 3 * PAGE + 256)).is_none());
        // One byte short of the fourth page's end.
        assert!(layout(region(0, PA, 4 * PAGE - 1)).is_none());
        // Exactly four pages is enough.
        assert!(layout(region(0, PA, 4 * PAGE)).is_some());
    }

    #[test]
    fn layout_rejects_a_queue_larger_than_its_page() {
        let big = queue_layout(256).expect("256 is a valid size");
        assert!(big.bytes > 0x1000);
        let whole = || region(0, PA, HOST_PAGES * PAGE);
        assert!(vcon_layout(whole(), big, q8()).is_none());
        assert!(vcon_layout(whole(), q8(), big).is_none());
        assert!(vcon_layout(whole(), big, big).is_none());
        assert!(vcon_layout(whole(), q8(), q8()).is_some());
    }

    #[test]
    fn layout_rejects_an_unaligned_run() {
        let len = 4 * PAGE;
        assert!(layout(region(0, PA + 16, len)).is_none());
        assert!(layout(region(16, PA, len)).is_none());
        assert!(layout(region(16, PA + 16, len)).is_none());
        assert!(layout(region(PAGE, PA, len)).is_some());
    }

    #[test]
    fn the_queue_indices_are_receive_0_and_transmit_1() {
        assert_eq!(RECEIVE_QUEUE, 0);
        assert_eq!(TRANSMIT_QUEUE, 1);
    }

    #[test]
    fn no_feature_is_wanted_so_only_version_1_is_accepted() {
        const VERSION_1: u64 = 1 << 32;
        assert_eq!(VCON_WANTED_FEATURES, 0);
        let accept = |offered| accept_features(offered, VCON_WANTED_FEATURES);
        assert_eq!(accept(VERSION_1), Some(0x1_0000_0000));
        // F_SIZE, F_MULTIPORT and F_EMERG_WRITE (bits 0-2) offered: none taken.
        assert_eq!(accept(VERSION_1 | 0b111), Some(0x1_0000_0000));
        assert_eq!(accept(u64::MAX), Some(0x1_0000_0000));
        assert_eq!(accept(0b111), None);
    }

    #[test]
    fn cookie_matches_only_its_own_pa() {
        let pa = 0x4800_0000;
        assert!(cookie_ok(pa ^ DMA_COOKIE, pa));
        assert!(!cookie_ok(pa ^ DMA_COOKIE, pa + 0x1000));
        assert!(!cookie_ok((pa + 0x1000) ^ DMA_COOKIE, pa));
        assert!(!cookie_ok(0, pa));
        assert!(!cookie_ok(pa, pa));
    }
}
