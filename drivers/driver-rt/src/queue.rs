// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The split virtqueue: layout arithmetic and ring discipline (slice 6.2, spec T11).
//!
//! Everything here is pure logic over the [`QueueMem`] the driver shares with the
//! device, so the tests play the device's half by writing the rings by hand
//! through a second handle on the same memory (which only this crate's tests can
//! make). The queue holds a handle, not a `&mut [u8]`: the device writes that
//! memory, so a unique reference was never a contract the driver could keep
//! (slice 6.3, spec B8). Every ring access is an aligned volatile load or store
//! made by `dma.rs`, bounds-checked there, so this module contains no `unsafe`.
//!
//! The device is untrusted, and a faulty one is *reported*, never skipped: a used
//! element naming an out-of-range or not-in-flight head, or a used index further
//! ahead than the chains in flight, makes [`VirtQueue::pop_used`] return a
//! [`QueueFault`] (spec B7). The driver's own chain bookkeeping lives outside the
//! shared memory, so a device that scribbles on the descriptor table cannot make a
//! walk loop or leave the queue.
//!
//! Safe code cannot aim the device at an address outside the driver's DMA run,
//! and that rests on two things together. Descriptors are filled only by
//! [`VirtQueue::add`], from [`DmaBuf`]s, each a range inside the run. And nothing
//! else in the driver can write the descriptor table: the queue's memory arrives
//! as a `QueueMem`, which `DmaRegion::split_queue` guarantees is disjoint from
//! every `DmaBuf` that can ever exist, which is neither `Copy` nor `Clone`, and
//! which [`VirtQueue::new`] consumes. (Built over a plain `DmaBuf`, as 6.3 first
//! had it, the caller could keep a copy and overwrite a descriptor's address
//! through it.) Nothing here waits on the used ring.

use crate::dma::{DmaBuf, QueueMem};
use crate::transport::QueueAddrs;
use core::sync::atomic::{Ordering, fence};

pub use crate::dma::DmaRun;

/// Entries in the queue the driver builds (clamped to the device's maximum).
pub const QUEUE_SIZE: u16 = 8;
/// Descriptor flag: the chain continues in `next`.
pub const DESC_F_NEXT: u16 = 1;
/// Descriptor flag: the device writes this buffer.
pub const DESC_F_WRITE: u16 = 2;

const QS: usize = QUEUE_SIZE as usize;
const DESC_BYTES: usize = 16;
const USED_ELEM_BYTES: usize = 8;

/// Offsets of the three areas of one queue inside its memory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct QueueLayout {
    /// Queue size (entries).
    pub size: u16,
    /// Descriptor table offset (always 0).
    pub desc_off: usize,
    /// Available ring offset (2-aligned).
    pub avail_off: usize,
    /// Used ring offset (4-aligned).
    pub used_off: usize,
    /// Total bytes the queue occupies.
    pub bytes: usize,
}

/// `None` unless `size` is a power of two in `1..=32768`.
pub const fn queue_layout(size: u16) -> Option<QueueLayout> {
    if size == 0 || !size.is_power_of_two() || size > 32768 {
        return None;
    }
    let n = size as usize;
    // Every product and sum below is under ~1 MiB for size <= 32768, but stay checked.
    let Some(avail_off) = n.checked_mul(DESC_BYTES) else {
        return None;
    };
    // flags + idx + ring[n] + used_event, all u16.
    let Some(avail_end) = avail_off.checked_add(6 + 2 * n) else {
        return None;
    };
    let used_off = (avail_end + 3) & !3;
    // flags + idx + ring[n] of { id: u32, len: u32 } + avail_event.
    let Some(bytes) = used_off.checked_add(6 + USED_ELEM_BYTES * n) else {
        return None;
    };
    Some(QueueLayout {
        size,
        desc_off: 0,
        avail_off,
        used_off,
        bytes,
    })
}

impl QueueLayout {
    /// The three physical addresses for a queue placed at `base_pa`; `None` on overflow or when
    /// `base_pa` is not 16-aligned (the virtio alignment of the descriptor table).
    pub const fn addrs(&self, base_pa: u64) -> Option<QueueAddrs> {
        if !base_pa.is_multiple_of(16) {
            return None;
        }
        let Some(desc_pa) = base_pa.checked_add(self.desc_off as u64) else {
            return None;
        };
        let Some(avail_pa) = base_pa.checked_add(self.avail_off as u64) else {
            return None;
        };
        let Some(used_pa) = base_pa.checked_add(self.used_off as u64) else {
            return None;
        };
        // The whole queue must also fit below the top of the address space.
        if base_pa.checked_add(self.bytes as u64).is_none() {
            return None;
        }
        Some(QueueAddrs {
            desc_pa,
            avail_pa,
            used_pa,
        })
    }
}

/// `min(QUEUE_SIZE, device_max)` rounded down to a power of two; `None` when the device reports 0.
pub const fn clamp_queue_size(device_max: u16) -> Option<u16> {
    if device_max == 0 {
        return None;
    }
    let m = if device_max < QUEUE_SIZE {
        device_max
    } else {
        QUEUE_SIZE
    };
    // Largest power of two not above `m` (the spec requires a power of two).
    Some(1 << (15 - m.leading_zeros()))
}

/// A device that broke the used-ring contract, as [`VirtQueue::pop_used`] reports it.
///
/// A fault is sticky: nothing was consumed, so the next `pop_used` reports it again.
/// The queue's own bookkeeping (free list, chains in flight) is untouched, but the
/// device can no longer be believed; the driver stops using it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QueueFault {
    /// The used element's id is not a descriptor index of this queue (`>=` the queue
    /// size, or not even a `u16`).
    BadId(u32),
    /// The used element's id is in range, but no chain in flight is headed there: a
    /// descriptor never submitted, a non-head descriptor, or a head completed twice.
    NotInFlight(u16),
    /// The device's used index is ahead of the driver's by more than the number of
    /// chains in flight — it claims completions nobody is waiting for. A completion
    /// with nothing in flight at all is this fault.
    Runaway {
        /// The used index the device published.
        used_idx: u16,
    },
}

/// One split virtqueue over shared memory.
pub struct VirtQueue {
    /// The queue's memory, taken out of the [`QueueMem`] `new` consumed and never
    /// handed on: no other handle on these bytes exists in the driver.
    mem: DmaBuf,
    layout: QueueLayout,
    /// Device-visible addresses of the three areas, validated by `new`.
    addrs: QueueAddrs,
    /// Driver-private chain links, never read back from shared memory.
    next: [u16; QS],
    /// Descriptors in the chain headed here (0 = not in flight).
    chain_len: [u16; QS],
    free_head: u16,
    free: u16,
    /// Chains submitted and not yet popped.
    in_flight: u16,
    avail_idx: u16,
    last_used: u16,
}

impl VirtQueue {
    /// `None` when `layout` is not exactly `queue_layout(layout.size)`, `layout.size`
    /// exceeds [`QUEUE_SIZE`] (the driver-private bookkeeping is sized for it), `mem` is
    /// shorter than `layout.bytes`, or `mem` is not 16-aligned at both its virtual and
    /// its physical address (a torn index store must be impossible, and the device
    /// requires the alignment). Zeroes the queue memory and threads the free list.
    ///
    /// `mem` is consumed either way: queue memory is never handed back as something
    /// a driver could write through.
    pub fn new(mem: QueueMem, layout: QueueLayout) -> Option<Self> {
        let QueueMem(mem) = mem;
        if queue_layout(layout.size) != Some(layout)
            || layout.size > QUEUE_SIZE
            || u64::from(mem.len()) < u64::try_from(layout.bytes).ok()?
            || !mem.va().is_multiple_of(16)
        {
            return None;
        }
        // Refuses a physical base that is not 16-aligned.
        let addrs = layout.addrs(mem.pa())?;
        let mut q = VirtQueue {
            mem,
            layout,
            addrs,
            next: [0; QS],
            chain_len: [0; QS],
            free_head: 0,
            free: layout.size,
            in_flight: 0,
            avail_idx: 0,
            last_used: 0,
        };
        for off in 0..layout.bytes {
            q.mem.wr8(off, 0)?;
        }
        for i in 0..layout.size {
            q.next[usize::from(i)] = i.wrapping_add(1);
        }
        Some(q)
    }

    /// Device-visible addresses of the descriptor table and the two rings.
    pub fn addrs(&self) -> QueueAddrs {
        self.addrs
    }

    /// Queue size in entries.
    pub fn size(&self) -> u16 {
        self.layout.size
    }

    // `new` accepted only a layout equal to `queue_layout(size)` with `size <= QUEUE_SIZE`
    // over a buffer at least `layout.bytes` long and 16-aligned, and every index below is
    // reduced modulo `size` first. So each offset these helpers produce is in bounds and
    // aligned for its field, and the `None` arms of the accesses built on them are
    // unreachable; the arithmetic is checked all the same.

    /// Offset of descriptor `i`.
    fn desc_off(&self, i: u16) -> Option<usize> {
        usize::from(i)
            .checked_mul(DESC_BYTES)?
            .checked_add(self.layout.desc_off)
    }

    /// Offset of the available-ring entry that index `idx` lands in.
    fn avail_ring_off(&self, idx: u16) -> Option<usize> {
        usize::from(idx % self.layout.size)
            .checked_mul(2)?
            .checked_add(4)?
            .checked_add(self.layout.avail_off)
    }

    /// Offset of the used-ring element that index `idx` lands in.
    fn used_elem_off(&self, idx: u16) -> Option<usize> {
        usize::from(idx % self.layout.size)
            .checked_mul(USED_ELEM_BYTES)?
            .checked_add(4)?
            .checked_add(self.layout.used_off)
    }

    /// Chain `out` (device-readable) then `inb` (device-writable) and publish the head.
    /// `None` if there are no buffers or too few free descriptors; nothing changes then.
    ///
    /// Each descriptor names its buffer's physical address and length, both taken from
    /// the [`DmaBuf`]: the device is only ever pointed inside the driver's DMA run,
    /// and never at this queue's own memory, which no `DmaBuf` covers.
    pub fn add(&mut self, out: &[DmaBuf], inb: &[DmaBuf]) -> Option<u16> {
        let total = u16::try_from(out.len().checked_add(inb.len())?).ok()?;
        if total == 0 {
            return None;
        }
        let free_after = self.free.checked_sub(total)?;
        let in_flight = self.in_flight.checked_add(1)?;
        let head = self.free_head;
        let mut cur = head;
        let mut left = total;
        let bufs = out
            .iter()
            .map(|b| (b, 0u16))
            .chain(inb.iter().map(|b| (b, DESC_F_WRITE)));
        for (buf, wflag) in bufs {
            left = left.checked_sub(1)?;
            let last = left == 0;
            let nxt = self.next[usize::from(cur)];
            let flags = wflag | if last { 0 } else { DESC_F_NEXT };
            let d = self.desc_off(cur)?;
            let pa = buf.pa();
            self.mem.wr32(d, pa as u32)?;
            self.mem.wr32(d.checked_add(4)?, (pa >> 32) as u32)?;
            self.mem.wr32(d.checked_add(8)?, buf.len())?;
            self.mem.wr16(d.checked_add(12)?, flags)?;
            self.mem
                .wr16(d.checked_add(14)?, if last { 0 } else { nxt })?;
            if last {
                self.free_head = nxt;
            } else {
                cur = nxt;
            }
        }
        self.free = free_after;
        self.in_flight = in_flight;
        self.chain_len[usize::from(head)] = total;

        let ring = self.avail_ring_off(self.avail_idx)?;
        self.mem.wr16(ring, head)?;
        // The ring entry must be visible before the index that publishes it.
        fence(Ordering::SeqCst);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        self.mem
            .wr16(self.layout.avail_off.checked_add(2)?, self.avail_idx)?;
        fence(Ordering::SeqCst);
        Some(head)
    }

    /// The device's used index.
    fn used_idx(&self) -> Option<u16> {
        self.mem.rd16(self.layout.used_off.checked_add(2)?)
    }

    /// The `(id, len)` of the used element that index `idx` lands in.
    fn used_elem(&self, idx: u16) -> Option<(u32, u32)> {
        let e = self.used_elem_off(idx)?;
        Some((self.mem.rd32(e)?, self.mem.rd32(e.checked_add(4)?)?))
    }

    /// Next completed `(head, written)`, returning its chain to the free list;
    /// `Ok(None)` when the device has published nothing new.
    ///
    /// `written` is the device-supplied length, unvalidated: callers must clamp it to the
    /// size of the buffer they submitted.
    ///
    /// A device that breaks the contract is reported, never skipped: `Err` with the
    /// [`QueueFault`]. A fault consumes nothing (the same call faults again) and leaves
    /// the free list and the chains in flight as they were.
    pub fn pop_used(&mut self) -> Result<Option<(u16, u32)>, QueueFault> {
        // The two `Ok(None)`s on a failed access are unreachable (see the note above
        // `desc_off`); they are not a way to skip an element.
        let Some(used_idx) = self.used_idx() else {
            return Ok(None);
        };
        // Distance modulo 2^16: both indices are free-running.
        let pending = used_idx.wrapping_sub(self.last_used);
        if pending == 0 {
            return Ok(None);
        }
        if pending > self.in_flight {
            return Err(QueueFault::Runaway { used_idx });
        }
        // Read the element only after seeing the index that publishes it.
        fence(Ordering::SeqCst);
        let Some((id, len)) = self.used_elem(self.last_used) else {
            return Ok(None);
        };
        let head = match u16::try_from(id) {
            Ok(head) if head < self.layout.size => head,
            _ => return Err(QueueFault::BadId(id)),
        };
        let n = self.chain_len[usize::from(head)];
        if n == 0 {
            return Err(QueueFault::NotInFlight(head));
        }
        self.last_used = self.last_used.wrapping_add(1);
        // Splice the chain (n descriptors, bounded by what `add` recorded) onto the free list.
        let mut tail = head;
        for _ in 1..n {
            tail = self.next[usize::from(tail)];
        }
        self.next[usize::from(tail)] = self.free_head;
        self.free_head = head;
        // Neither can leave its range: `add` took exactly `n` descriptors and one
        // in-flight count for this chain.
        self.free = self.free.saturating_add(n);
        self.in_flight = self.in_flight.saturating_sub(1);
        self.chain_len[usize::from(head)] = 0;
        Ok(Some((head, len)))
    }

    /// Descriptors currently free.
    pub fn num_free(&self) -> u16 {
        self.free
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dma::testmem::HostDma;

    /// Made-up physical base, above 4 GiB so both descriptor address words matter.
    const PA: u64 = 0x1_0000_0000;
    const PAGE: usize = 4096;

    /// The queue lives in page 0 of the host run; page 1 supplies request buffers.
    fn queue(host: &HostDma, l: QueueLayout) -> VirtQueue {
        VirtQueue::new(host.queue_mem(0, PAGE), l).unwrap()
    }

    /// A request buffer: `len` bytes at `off` in page 1.
    fn data(host: &HostDma, off: usize, len: usize) -> DmaBuf {
        host.region.buf(PAGE + off, len).unwrap()
    }

    /// `(start index, rounds)`: the full 65 540 rounds from 0, or under Miri (too slow for
    /// that) a start just below 2^16 so the same boundary is still crossed.
    fn wrap_plan() -> (u32, u32) {
        if cfg!(miri) {
            (65_528, 16)
        } else {
            (0, 65_540)
        }
    }

    fn layout8() -> QueueLayout {
        queue_layout(8).unwrap()
    }

    // The device's half: `dev` is a second handle on the same memory as the queue's.

    fn rd16(dev: &DmaBuf, o: usize) -> u16 {
        let mut b = [0u8; 2];
        assert!(dev.read_into(o, &mut b));
        u16::from_le_bytes(b)
    }

    fn rd32(dev: &DmaBuf, o: usize) -> u32 {
        let mut b = [0u8; 4];
        assert!(dev.read_into(o, &mut b));
        u32::from_le_bytes(b)
    }

    fn rd64(dev: &DmaBuf, o: usize) -> u64 {
        let mut b = [0u8; 8];
        assert!(dev.read_into(o, &mut b));
        u64::from_le_bytes(b)
    }

    fn set_used_idx(dev: &DmaBuf, l: &QueueLayout, idx: u16) {
        assert!(dev.write_from(l.used_off + 2, &idx.to_le_bytes()));
    }

    /// Write used element number `n` and publish it (used index `n + 1`).
    fn device_complete(dev: &DmaBuf, l: &QueueLayout, n: u16, id: u32, len: u32) {
        let slot = usize::from(n % l.size);
        let e = l.used_off + 4 + 8 * slot;
        assert!(dev.write_from(e, &id.to_le_bytes()));
        assert!(dev.write_from(e + 4, &len.to_le_bytes()));
        set_used_idx(dev, l, n.wrapping_add(1));
    }

    #[test]
    fn a_queue_over_split_off_memory_is_out_of_reach_of_every_buffer() {
        // The production path: no test-only handle on the queue's bytes.
        let l = layout8();
        let host = HostDma::new(PA);
        let dev = host.all();
        let (mem, rest) = host.fresh_region().split_queue(PAGE).unwrap();
        let mut q = VirtQueue::new(mem, l).unwrap();
        assert_eq!(Some(q.addrs()), l.addrs(PA));
        // The driver's largest possible buffer starts after the queue, so filling
        // it cannot touch a descriptor...
        let all = rest.buf(0, PAGE).unwrap();
        assert_eq!(all.pa(), PA + PAGE as u64);
        assert!(all.write_from(0, &[0xFF; PAGE]));
        let head = q.add(&[rest.buf(0, 16).unwrap()], &[]).unwrap();
        // ...and the address the device reads is the one `add` took from the buffer.
        assert_eq!(rd64(&dev, usize::from(head) * 16), PA + PAGE as u64);
        assert_eq!(rd32(&dev, usize::from(head) * 16 + 8), 16);
        device_complete(&dev, &l, 0, u32::from(head), 0);
        assert_eq!(q.pop_used(), Ok(Some((head, 0))));
    }

    #[test]
    fn layout_for_eight_entries_fits_one_page() {
        let l = layout8();
        assert_eq!(
            (l.desc_off, l.avail_off, l.used_off, l.bytes),
            (0, 128, 152, 222)
        );
        assert!(l.bytes < 256);
    }

    #[test]
    fn layout_rejects_zero_and_non_powers_of_two() {
        for s in [0u16, 3, 6, 40000] {
            assert_eq!(queue_layout(s), None);
        }
        assert!(queue_layout(32768).is_some());
    }

    #[test]
    fn addrs_overflow_is_none() {
        let l = layout8();
        assert_eq!(l.addrs(u64::MAX - 8), None);
        let a = l.addrs(0x1000).unwrap();
        assert_eq!((a.desc_pa, a.avail_pa, a.used_pa), (0x1000, 0x1080, 0x1098));
    }

    #[test]
    fn clamp_queue_size_handles_zero_small_and_large() {
        assert_eq!(clamp_queue_size(0), None);
        assert_eq!(clamp_queue_size(1), Some(1));
        assert_eq!(clamp_queue_size(4), Some(4));
        assert_eq!(clamp_queue_size(5), Some(4));
        assert_eq!(clamp_queue_size(7), Some(4));
        assert_eq!(clamp_queue_size(8), Some(8));
        assert_eq!(clamp_queue_size(256), Some(8));
        assert_eq!(clamp_queue_size(u16::MAX), Some(8));
    }

    #[test]
    fn small_queue_runs_past_the_index_wrap() {
        // A device capped at 4 entries: memory exactly the layout size, add/complete rounds
        // crossing the u16 index boundary, and exhaustion at the end.
        let l = queue_layout(clamp_queue_size(4).unwrap()).unwrap();
        assert_eq!(l.size, 4);
        let host = HostDma::new(PA);
        let dev = host.all();
        let mut q = VirtQueue::new(host.queue_mem(0, l.bytes), l).unwrap();
        assert_eq!(q.size(), 4);
        let one = data(&host, 0, 1);
        let (start, rounds) = wrap_plan();
        q.avail_idx = start as u16;
        q.last_used = start as u16;
        for round in 0..rounds {
            let n = start.wrapping_add(round) as u16;
            let head = q.add(&[one], &[]).unwrap();
            assert_eq!(rd16(&dev, l.avail_off + 4 + 2 * usize::from(n % 4)), head);
            device_complete(&dev, &l, n, u32::from(head), 1);
            assert_eq!(q.pop_used(), Ok(Some((head, 1))));
        }
        for _ in 0..4 {
            q.add(&[one], &[]).unwrap();
        }
        assert_eq!(q.add(&[one], &[]), None);
    }

    #[test]
    fn new_rejects_a_short_buffer() {
        let l = layout8();
        let host = HostDma::new(PA);
        let buf = |len| host.queue_mem(0, len);
        assert!(VirtQueue::new(buf(l.bytes - 1), l).is_none());
        assert!(VirtQueue::new(buf(l.bytes), l).is_some());
        let big = queue_layout(16).unwrap();
        assert!(VirtQueue::new(buf(big.bytes), big).is_none());
    }

    #[test]
    fn new_rejects_an_unaligned_buffer() {
        let l = layout8();
        let host = HostDma::new(PA);
        let at = |off| host.queue_mem(off, l.bytes);
        assert!(VirtQueue::new(at(1), l).is_none());
        assert!(VirtQueue::new(at(8), l).is_none());
        assert!(VirtQueue::new(at(16), l).is_some());
    }

    #[test]
    fn new_checks_the_virtual_and_the_physical_alignment_separately() {
        // A run whose physical base is 8 off: the two addresses of any buffer in it
        // are never 16-aligned together.
        let l = layout8();
        let host = HostDma::new(PA + 8);
        let at = |off| host.queue_mem(off, l.bytes);
        let (va_only, pa_only) = (at(0), at(8));
        assert!(va_only.va().is_multiple_of(16) && !va_only.pa().is_multiple_of(16));
        assert!(!pa_only.va().is_multiple_of(16) && pa_only.pa().is_multiple_of(16));
        assert!(VirtQueue::new(va_only, l).is_none());
        assert!(VirtQueue::new(pa_only, l).is_none());
    }

    #[test]
    fn new_rejects_an_inconsistent_layout() {
        let good = layout8();
        let host = HostDma::new(PA);
        for bad in [
            QueueLayout {
                used_off: good.used_off + 4,
                ..good
            },
            QueueLayout {
                avail_off: 0,
                ..good
            },
            QueueLayout {
                desc_off: 16,
                ..good
            },
            QueueLayout { bytes: 8, ..good },
            QueueLayout {
                bytes: usize::MAX,
                ..good
            },
            QueueLayout { size: 3, ..good },
        ] {
            let all = host.queue_mem(0, crate::dma::testmem::HOST_DMA_BYTES);
            assert!(VirtQueue::new(all, bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn new_zeroes_the_queue_and_nothing_past_it() {
        let l = layout8();
        let host = HostDma::new(PA);
        let dev = host.all();
        assert!(dev.write_from(0, &[0xFF; 256]));
        let _q = queue(&host, l);
        let mut seen = [0u8; 256];
        assert!(dev.read_into(0, &mut seen));
        assert!(seen[..l.bytes].iter().all(|&b| b == 0));
        assert!(seen[l.bytes..].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn addrs_rejects_an_unaligned_base() {
        let l = layout8();
        assert_eq!(l.addrs(0x1008), None);
        assert!(l.addrs(0x1010).is_some());
    }

    #[test]
    fn the_queue_reports_its_size_and_device_addresses() {
        let l = layout8();
        let host = HostDma::new(PA);
        let q = VirtQueue::new(host.queue_mem(PAGE, PAGE), l).unwrap();
        assert_eq!(q.size(), 8);
        let base = PA + PAGE as u64;
        assert_eq!(
            q.addrs(),
            QueueAddrs {
                desc_pa: base,
                avail_pa: base + 0x80,
                used_pa: base + 0x98,
            }
        );
        assert_eq!(Some(q.addrs()), l.addrs(base));
    }

    #[test]
    fn add_builds_an_out_then_in_chain() {
        let l = layout8();
        let host = HostDma::new(PA);
        let dev = host.all();
        let mut q = queue(&host, l);
        let (hdr, blk, status) = (
            data(&host, 0, 16),
            data(&host, 512, 2048),
            data(&host, 16, 1),
        );
        let head = q.add(&[hdr], &[blk, status]).unwrap();
        assert_eq!(q.num_free(), 5);
        let flags = |i: usize| rd16(&dev, i * 16 + 12);
        let next = |i: usize| usize::from(rd16(&dev, i * 16 + 14));
        let h = usize::from(head);
        assert_eq!(flags(h), DESC_F_NEXT);
        // Address and length come from the buffer: the made-up base is above 4 GiB,
        // so this reads both address words.
        assert_eq!(rd64(&dev, h * 16), PA + PAGE as u64);
        assert_eq!(rd64(&dev, h * 16), hdr.pa());
        assert_eq!(rd32(&dev, h * 16 + 8), 16);
        let n1 = next(h);
        assert_eq!(flags(n1), DESC_F_WRITE | DESC_F_NEXT);
        assert_eq!(rd64(&dev, n1 * 16), blk.pa());
        assert_eq!(rd32(&dev, n1 * 16 + 8), 2048);
        let n2 = next(n1);
        assert_eq!(flags(n2), DESC_F_WRITE);
        assert_eq!(rd64(&dev, n2 * 16), status.pa());
        assert_eq!(rd32(&dev, n2 * 16 + 8), 1);
        assert_eq!(rd16(&dev, l.avail_off + 4), head);
        assert_eq!(rd16(&dev, l.avail_off + 2), 1);
    }

    #[test]
    fn add_without_enough_descriptors_is_none_and_changes_nothing() {
        let l = layout8();
        let host = HostDma::new(PA);
        let dev = host.all();
        let mut q = queue(&host, l);
        let (a, b) = (data(&host, 0, 1), data(&host, 1, 1));
        q.add(&[a; 3], &[b; 3]).unwrap();
        assert_eq!(q.num_free(), 2);
        assert_eq!(q.add(&[a; 3], &[]), None);
        assert_eq!(q.num_free(), 2);
        // The free list is intact: exactly two more descriptors can be had.
        assert!(q.add(&[a], &[b]).is_some());
        assert_eq!(q.num_free(), 0);
        assert_eq!(q.add(&[a], &[]), None);
        assert_eq!(rd16(&dev, l.avail_off + 2), 2);
    }

    #[test]
    fn add_with_no_buffers_is_none() {
        let l = layout8();
        let host = HostDma::new(PA);
        let mut q = queue(&host, l);
        assert_eq!(q.add(&[], &[]), None);
        assert_eq!(q.num_free(), 8);
    }

    #[test]
    fn pop_used_returns_the_chain_to_the_free_list() {
        let l = layout8();
        let host = HostDma::new(PA);
        let dev = host.all();
        let mut q = queue(&host, l);
        let head = q
            .add(
                &[data(&host, 0, 16)],
                &[data(&host, 512, 512), data(&host, 16, 1)],
            )
            .unwrap();
        assert_eq!(q.pop_used(), Ok(None));
        device_complete(&dev, &l, 0, u32::from(head), 513);
        assert_eq!(q.pop_used(), Ok(Some((head, 513))));
        assert_eq!(q.num_free(), 8);
        assert_eq!(q.pop_used(), Ok(None));
    }

    #[test]
    fn two_chains_in_flight_complete_in_the_devices_order() {
        let l = layout8();
        let host = HostDma::new(PA);
        let dev = host.all();
        let mut q = queue(&host, l);
        let one = data(&host, 0, 1);
        let first = q.add(&[one], &[one]).unwrap();
        let second = q.add(&[one], &[]).unwrap();
        // Both published at once, the later submission first.
        device_complete(&dev, &l, 0, u32::from(second), 0);
        device_complete(&dev, &l, 1, u32::from(first), 1);
        assert_eq!(q.pop_used(), Ok(Some((second, 0))));
        assert_eq!(q.pop_used(), Ok(Some((first, 1))));
        assert_eq!(q.pop_used(), Ok(None));
        assert_eq!(q.num_free(), 8);
    }

    #[test]
    fn indices_wrap_modulo_two_to_the_sixteen() {
        let l = layout8();
        let host = HostDma::new(PA);
        let dev = host.all();
        let mut q = queue(&host, l);
        let one = data(&host, 0, 1);
        let (start, rounds) = wrap_plan();
        q.avail_idx = start as u16;
        q.last_used = start as u16;
        for round in 0..rounds {
            let n = start.wrapping_add(round) as u16;
            let head = q.add(&[one], &[]).unwrap();
            let slot = usize::from(n % 8);
            let ring = l.avail_off + 4 + 2 * slot;
            assert_eq!(rd16(&dev, ring), head, "round {round}");
            assert_eq!(rd16(&dev, l.avail_off + 2), n.wrapping_add(1));
            device_complete(&dev, &l, n, u32::from(head), 1);
            assert_eq!(q.pop_used(), Ok(Some((head, 1))), "round {round}");
            assert_eq!(q.num_free(), 8);
        }
        assert_eq!(
            rd16(&dev, l.avail_off + 2),
            start.wrapping_add(rounds) as u16
        );
        assert_eq!(q.pop_used(), Ok(None));
    }

    #[test]
    fn an_out_of_range_used_id_is_a_fault() {
        let l = layout8();
        let host = HostDma::new(PA);
        let dev = host.all();
        let mut q = queue(&host, l);
        let head = q.add(&[data(&host, 0, 1)], &[]).unwrap();
        // One past the last descriptor, then an id that is not even a u16.
        device_complete(&dev, &l, 0, 8, 0);
        assert_eq!(q.pop_used(), Err(QueueFault::BadId(8)));
        device_complete(&dev, &l, 0, 0xFFFF_FFFF, 0);
        assert_eq!(q.pop_used(), Err(QueueFault::BadId(0xFFFF_FFFF)));
        device_complete(&dev, &l, 0, 0x1_0000 | u32::from(head), 0);
        assert_eq!(
            q.pop_used(),
            Err(QueueFault::BadId(0x1_0000 | u32::from(head)))
        );
        // Nothing was consumed or freed: the chain is still in flight, and the fault
        // was not skipped over to reach a later, valid element.
        assert_eq!(q.num_free(), 7);
        device_complete(&dev, &l, 1, u32::from(head), 9);
        assert_eq!(
            q.pop_used(),
            Err(QueueFault::Runaway { used_idx: 2 }),
            "the bad element still heads the ring"
        );
    }

    #[test]
    fn a_used_id_with_no_chain_headed_there_is_a_fault() {
        let l = layout8();
        let host = HostDma::new(PA);
        let dev = host.all();
        let mut q = queue(&host, l);
        let one = data(&host, 0, 1);
        let a = q.add(&[one, one], &[one]).unwrap();
        let b = q.add(&[one], &[]).unwrap();
        // In range but never submitted.
        device_complete(&dev, &l, 0, 7, 0);
        assert_eq!(q.pop_used(), Err(QueueFault::NotInFlight(7)));
        // Sticky: the same element faults again.
        assert_eq!(q.pop_used(), Err(QueueFault::NotInFlight(7)));
        // A descriptor inside a chain in flight, but not its head.
        let inner = q.next[usize::from(a)];
        device_complete(&dev, &l, 0, u32::from(inner), 0);
        assert_eq!(q.pop_used(), Err(QueueFault::NotInFlight(inner)));
        assert_eq!(q.num_free(), 4);
        // A head completed twice: the first is good, the repeat is stale.
        device_complete(&dev, &l, 0, u32::from(a), 3);
        assert_eq!(q.pop_used(), Ok(Some((a, 3))));
        device_complete(&dev, &l, 1, u32::from(a), 3);
        assert_eq!(q.pop_used(), Err(QueueFault::NotInFlight(a)));
        // The fault left the bookkeeping intact: `b` is still in flight, `a`'s three
        // descriptors are free, and all seven free ones are allocatable exactly once.
        assert_eq!(q.num_free(), 7);
        assert!(q.add(&[one; 7], &[]).is_some());
        assert_eq!(q.add(&[one], &[]), None);
        assert_eq!(q.chain_len[usize::from(b)], 1);
    }

    #[test]
    fn a_runaway_used_index_is_a_fault() {
        let l = layout8();
        let host = HostDma::new(PA);
        let dev = host.all();
        let mut q = queue(&host, l);
        let one = data(&host, 0, 1);
        let head = q.add(&[one], &[]).unwrap();
        // One chain in flight, two completions claimed — even though the first
        // element is a perfectly good one, the index cannot be believed.
        device_complete(&dev, &l, 0, u32::from(head), 1);
        set_used_idx(&dev, &l, 2);
        assert_eq!(q.pop_used(), Err(QueueFault::Runaway { used_idx: 2 }));
        // Far ahead, and behind (which is "ahead" by almost 2^16 modulo the wrap).
        set_used_idx(&dev, &l, 60_000);
        assert_eq!(q.pop_used(), Err(QueueFault::Runaway { used_idx: 60_000 }));
        set_used_idx(&dev, &l, u16::MAX);
        assert_eq!(
            q.pop_used(),
            Err(QueueFault::Runaway { used_idx: u16::MAX })
        );
        // Nothing was consumed: the chain is still in flight.
        assert_eq!(q.num_free(), 7);
        assert_eq!(q.last_used, 0);
        // The distance is taken modulo 2^16, so an index that wrapped is not a fault.
        set_used_idx(&dev, &l, 1);
        assert_eq!(q.pop_used(), Ok(Some((head, 1))));
    }

    #[test]
    fn a_completion_with_nothing_in_flight_is_a_fault() {
        let l = layout8();
        let host = HostDma::new(PA);
        let dev = host.all();
        let mut q = queue(&host, l);
        // Never submitted anything; the device publishes an element anyway.
        device_complete(&dev, &l, 0, 0, 0);
        assert_eq!(q.pop_used(), Err(QueueFault::Runaway { used_idx: 1 }));
        assert_eq!(q.pop_used(), Err(QueueFault::Runaway { used_idx: 1 }));
        assert_eq!(q.num_free(), 8);
        // The same after a request that came and went.
        let mut q = queue(&host, l);
        let head = q.add(&[data(&host, 0, 1)], &[]).unwrap();
        device_complete(&dev, &l, 0, u32::from(head), 1);
        assert_eq!(q.pop_used(), Ok(Some((head, 1))));
        device_complete(&dev, &l, 1, u32::from(head), 1);
        assert_eq!(q.pop_used(), Err(QueueFault::Runaway { used_idx: 2 }));
        // The queue's own bookkeeping survived: every descriptor is allocatable once.
        assert_eq!(q.num_free(), 8);
        let one = data(&host, 0, 1);
        assert!(q.add(&[one; 4], &[one; 4]).is_some());
        assert_eq!(q.num_free(), 0);
    }

    #[test]
    fn a_looping_descriptor_chain_cannot_hang_or_corrupt_the_free_list() {
        let l = layout8();
        let host = HostDma::new(PA);
        let dev = host.all();
        let mut q = queue(&host, l);
        let one = data(&host, 0, 1);
        let head = q.add(&[one, one], &[one]).unwrap();
        // The "device" rewrites every descriptor's next to point at itself.
        for i in 0..8usize {
            assert!(dev.write_from(i * 16 + 12, &DESC_F_NEXT.to_le_bytes()));
            assert!(dev.write_from(i * 16 + 14, &(i as u16).to_le_bytes()));
        }
        device_complete(&dev, &l, 0, u32::from(head), 1);
        assert_eq!(q.pop_used(), Ok(Some((head, 1))));
        assert_eq!(q.num_free(), 8);
        // Every descriptor is still allocatable exactly once.
        assert!(q.add(&[one; 4], &[one; 4]).is_some());
        assert_eq!(q.num_free(), 0);
    }
}
