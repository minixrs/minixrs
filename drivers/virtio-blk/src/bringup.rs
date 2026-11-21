// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The pure half of virtio-blk's bring-up (slice 6.2, spec T11/T12): where the
//! queue, the request header and the bounce buffer sit in the DMA run, how the
//! capacity is decoded, and the DMA cookie check. Host-tested; `main.rs` does the
//! kernel calls and register accesses around it.

// Only `main.rs`'s `not(test)`-gated paths reach most of this, so under
// `cargo test` (which drops `_start` and `main`) it would read as dead code.
#![cfg_attr(test, allow(dead_code))]

use minixrs_driver_rt::{DmaRun, QueueAddrs, QueueLayout};
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

/// Where virtio-blk's three areas sit in its DMA run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VblkLayout {
    /// Driver-visible address of the queue page (the `VirtQueue`'s memory).
    pub queue_va: u64,
    /// Device-visible addresses of the queue's three areas.
    pub queue: QueueAddrs,
    /// Device-visible address of the request header page.
    pub header_pa: u64,
    /// Device-visible address of the bounce-buffer page.
    pub data_pa: u64,
}

/// Lay virtio-blk out in `run`. `None` unless the run is page-aligned at both
/// ends of the translation, holds the three pages, and `queue` fits in page 0.
pub fn vblk_layout(run: DmaRun, queue: QueueLayout) -> Option<VblkLayout> {
    if !run.va.is_multiple_of(USER_PAGE_SIZE) || !run.pa.is_multiple_of(USER_PAGE_SIZE) {
        return None;
    }
    // The highest page used is the data page; the run must reach its end, and
    // neither end of the translation may wrap.
    let needed = VBLK_DATA_PAGE.checked_add(1)?.checked_mul(USER_PAGE_SIZE)?;
    if run.len < needed || queue.bytes as u64 > USER_PAGE_SIZE {
        return None;
    }
    run.va.checked_add(run.len)?;
    run.pa.checked_add(run.len)?;
    let page_pa = |page: u64| run.pa.checked_add(page.checked_mul(USER_PAGE_SIZE)?);
    let queue_va = run
        .va
        .checked_add(VBLK_QUEUE_PAGE.checked_mul(USER_PAGE_SIZE)?)?;
    Some(VblkLayout {
        queue_va,
        queue: queue.addrs(page_pa(VBLK_QUEUE_PAGE)?)?,
        header_pa: page_pa(VBLK_HEADER_PAGE)?,
        data_pa: page_pa(VBLK_DATA_PAGE)?,
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
    use minixrs_driver_rt::{QUEUE_SIZE, queue_layout};

    const RUN: DmaRun = DmaRun {
        va: 0xC000_0000,
        pa: 0x4800_0000,
        len: 64 * 1024,
    };

    fn q8() -> QueueLayout {
        queue_layout(QUEUE_SIZE).expect("size 8 is a valid queue")
    }

    #[test]
    fn layout_places_queue_header_and_data_on_separate_pages() {
        let l = vblk_layout(RUN, q8()).expect("a 64 KiB run holds three pages");
        assert_eq!(l.queue_va, 0xC000_0000);
        assert_eq!(
            l.queue,
            QueueAddrs {
                desc_pa: 0x4800_0000,
                avail_pa: 0x4800_0080,
                used_pa: 0x4800_0098,
            }
        );
        assert_eq!(l.header_pa, 0x4800_1000);
        assert_eq!(l.data_pa, 0x4800_2000);
    }

    #[test]
    fn layout_rejects_a_two_page_run() {
        let run = DmaRun { len: 0x2000, ..RUN };
        assert_eq!(vblk_layout(run, q8()), None);
        // Exactly three pages is enough.
        let run = DmaRun { len: 0x3000, ..RUN };
        assert!(vblk_layout(run, q8()).is_some());
    }

    #[test]
    fn layout_rejects_a_queue_larger_than_its_page() {
        let big = queue_layout(256).expect("256 is a valid size");
        assert!(big.bytes > 0x1000);
        assert_eq!(vblk_layout(RUN, big), None);
    }

    #[test]
    fn layout_rejects_an_unaligned_or_wrapping_run() {
        assert_eq!(
            vblk_layout(
                DmaRun {
                    pa: RUN.pa + 16,
                    ..RUN
                },
                q8()
            ),
            None
        );
        assert_eq!(
            vblk_layout(
                DmaRun {
                    va: RUN.va + 16,
                    ..RUN
                },
                q8()
            ),
            None
        );
        let top = DmaRun {
            va: RUN.va,
            pa: u64::MAX - 0xFFF,
            len: RUN.len,
        };
        assert_eq!(vblk_layout(top, q8()), None);
        let top = DmaRun {
            va: u64::MAX - 0xFFF,
            pa: RUN.pa,
            len: RUN.len,
        };
        assert_eq!(vblk_layout(top, q8()), None);
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
