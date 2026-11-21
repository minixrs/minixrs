// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The split virtqueue: layout arithmetic and ring discipline (slice 6.2, spec T11).
//!
//! Everything here is pure logic over a byte buffer the driver shares with the
//! device, so the tests play the device's half by writing the rings by hand. The
//! device is untrusted: a used element naming an out-of-range or not-in-flight
//! head is skipped, and the driver's own chain bookkeeping lives outside the
//! shared memory, so a device that scribbles on the descriptor table cannot make
//! a walk loop or leave the queue. Nothing here waits on the used ring.

use crate::transport::QueueAddrs;
use core::sync::atomic::{Ordering, fence};

/// Entries in the queue the driver builds (clamped to the device's maximum).
pub const QUEUE_SIZE: u16 = 8;
/// Descriptor flag: the chain continues in `next`.
pub const DESC_F_NEXT: u16 = 1;
/// Descriptor flag: the device writes this buffer.
pub const DESC_F_WRITE: u16 = 2;

const QS: usize = QUEUE_SIZE as usize;
const DESC_BYTES: usize = 16;
const USED_ELEM_BYTES: usize = 8;

/// A DMA run: virtual address, physical address and length in bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DmaRun {
    /// Driver-visible virtual address.
    pub va: u64,
    /// Device-visible physical address.
    pub pa: u64,
    /// Length in bytes.
    pub len: u64,
}

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

/// One split virtqueue over shared memory.
pub struct VirtQueue<'a> {
    mem: &'a mut [u8],
    layout: QueueLayout,
    /// Driver-private chain links, never read back from shared memory.
    next: [u16; QS],
    /// Descriptors in the chain headed here (0 = not in flight).
    chain_len: [u16; QS],
    free_head: u16,
    free: u16,
    avail_idx: u16,
    last_used: u16,
}

macro_rules! volatile_rw {
    ($rd:ident, $wr:ident, $t:ty) => {
        fn $rd(&self, off: usize) -> Option<$t> {
            const N: usize = core::mem::size_of::<$t>();
            let p = self.ptr(off, N)?;
            if !(p as usize).is_multiple_of(N) {
                return None;
            }
            // SAFETY: `ptr` proved `off..off+N` lies inside `mem`; `p` is aligned for `$t`.
            Some(<$t>::from_le(unsafe {
                core::ptr::read_volatile(p as *const $t)
            }))
        }
        fn $wr(&mut self, off: usize, v: $t) -> Option<()> {
            const N: usize = core::mem::size_of::<$t>();
            let p = self.ptr_mut(off, N)?;
            if !(p as usize).is_multiple_of(N) {
                return None;
            }
            // SAFETY: `ptr_mut` derived `p` from `&mut self.mem` (unique access, write
            // provenance) and proved `off..off+N` lies inside it; `p` is aligned for `$t`.
            unsafe { core::ptr::write_volatile(p as *mut $t, v.to_le()) };
            Some(())
        }
    };
}

impl<'a> VirtQueue<'a> {
    /// `None` when `layout` is not exactly `queue_layout(layout.size)`, `layout.size`
    /// exceeds [`QUEUE_SIZE`] (the driver-private bookkeeping is sized for it), `mem` is
    /// shorter than `layout.bytes`, or `mem` is not 16-aligned (a torn index store must be
    /// impossible). Zeroes the queue memory and threads the free list.
    pub fn new(mem: &'a mut [u8], layout: QueueLayout) -> Option<Self> {
        if queue_layout(layout.size) != Some(layout)
            || layout.size > QUEUE_SIZE
            || mem.len() < layout.bytes
            || !(mem.as_ptr() as usize).is_multiple_of(16)
        {
            return None;
        }
        let mut q = VirtQueue {
            mem,
            layout,
            next: [0; QS],
            chain_len: [0; QS],
            free_head: 0,
            free: layout.size,
            avail_idx: 0,
            last_used: 0,
        };
        for off in 0..layout.bytes {
            q.wr8(off, 0)?;
        }
        for i in 0..layout.size {
            q.next[i as usize] = i.wrapping_add(1);
        }
        Some(q)
    }

    /// Pointer to `off..off+n` of the shared memory, or `None` if it is out of bounds.
    fn ptr(&self, off: usize, n: usize) -> Option<*const u8> {
        let end = off.checked_add(n)?;
        self.mem.get(off..end).map(|s| s.as_ptr())
    }

    /// Like [`Self::ptr`] but with write provenance, from the exclusive borrow.
    fn ptr_mut(&mut self, off: usize, n: usize) -> Option<*mut u8> {
        let end = off.checked_add(n)?;
        self.mem.get_mut(off..end).map(|s| s.as_mut_ptr())
    }

    fn wr8(&mut self, off: usize, v: u8) -> Option<()> {
        let p = self.ptr_mut(off, 1)?;
        // SAFETY: `ptr_mut` derived `p` from `&mut self.mem` and proved `off` is inside it.
        unsafe { core::ptr::write_volatile(p, v) };
        Some(())
    }

    volatile_rw!(rd16, wr16, u16);
    volatile_rw!(rd32, wr32, u32);

    // Offset arithmetic in `add` and `pop_used` cannot overflow: `new` accepted only a layout
    // equal to `queue_layout(size)` with `size <= QUEUE_SIZE` (so every offset is below
    // `layout.bytes`, a few hundred bytes), and every index is reduced modulo `size` first.

    /// Chain `out` (device-readable) then `inb` (device-writable) and publish the head.
    /// `None` if there are no buffers or too few free descriptors; nothing changes then.
    pub fn add(&mut self, out: &[(u64, u32)], inb: &[(u64, u32)]) -> Option<u16> {
        let total = out.len().checked_add(inb.len())?;
        if total == 0 || total > usize::from(self.free) {
            return None;
        }
        let head = self.free_head;
        let mut cur = head;
        let mut prev_next = self.next[usize::from(head)];
        let bufs = out
            .iter()
            .map(|b| (b, 0u16))
            .chain(inb.iter().map(|b| (b, DESC_F_WRITE)));
        for (i, (&(pa, len), wflag)) in bufs.enumerate() {
            let last = i + 1 == total;
            let nxt = prev_next;
            let flags = wflag | if last { 0 } else { DESC_F_NEXT };
            let d = self.layout.desc_off + usize::from(cur) * DESC_BYTES;
            self.wr32(d, pa as u32)?;
            self.wr32(d + 4, (pa >> 32) as u32)?;
            self.wr32(d + 8, len)?;
            self.wr16(d + 12, flags)?;
            self.wr16(d + 14, if last { 0 } else { nxt })?;
            if last {
                self.free_head = nxt;
            } else {
                cur = nxt;
                prev_next = self.next[usize::from(cur)];
            }
        }
        self.free -= total as u16;
        self.chain_len[usize::from(head)] = total as u16;

        let slot = usize::from(self.avail_idx % self.layout.size);
        let ring = self.layout.avail_off + 4 + 2 * slot;
        self.wr16(ring, head)?;
        // The ring entry must be visible before the index that publishes it.
        fence(Ordering::SeqCst);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        self.wr16(self.layout.avail_off + 2, self.avail_idx)?;
        fence(Ordering::SeqCst);
        Some(head)
    }

    /// Next completed `(head, written)`, returning its chain to the free list.
    ///
    /// `written` is the device-supplied length, unvalidated: callers must clamp it to the
    /// size of the buffer they submitted.
    ///
    /// Elements whose id is out of range or not in flight (a faulty device) are
    /// consumed and skipped; at most one queue's worth are examined per call.
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        for _ in 0..self.layout.size {
            let used_idx = self.rd16(self.layout.used_off + 2)?;
            if used_idx == self.last_used {
                return None;
            }
            // Read the element only after seeing the index that publishes it.
            fence(Ordering::SeqCst);
            let slot = usize::from(self.last_used % self.layout.size);
            let e = self.layout.used_off + 4 + USED_ELEM_BYTES * slot;
            let id = self.rd32(e)?;
            let len = self.rd32(e + 4)?;
            self.last_used = self.last_used.wrapping_add(1);
            let Ok(head) = u16::try_from(id) else {
                continue;
            };
            if head >= self.layout.size {
                continue;
            }
            let n = self.chain_len[usize::from(head)];
            if n == 0 {
                continue;
            }
            // Splice the chain (n descriptors, bounded by what `add` recorded) onto the free list.
            let mut tail = head;
            for _ in 1..n {
                tail = self.next[usize::from(tail)];
            }
            self.next[usize::from(tail)] = self.free_head;
            self.free_head = head;
            self.free += n;
            self.chain_len[usize::from(head)] = 0;
            return Some((head, len));
        }
        None
    }

    /// Descriptors currently free.
    pub fn num_free(&self) -> u16 {
        self.free
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(align(16))]
    struct Mem([u8; 4096]);

    fn mem() -> Mem {
        Mem([0; 4096])
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

    fn rd16(m: &[u8], o: usize) -> u16 {
        u16::from_le_bytes([m[o], m[o + 1]])
    }

    fn device_complete(m: &mut [u8], l: &QueueLayout, n: u16, id: u32, len: u32) {
        let slot = usize::from(n % l.size);
        let e = l.used_off + 4 + 8 * slot;
        m[e..e + 4].copy_from_slice(&id.to_le_bytes());
        m[e + 4..e + 8].copy_from_slice(&len.to_le_bytes());
        m[l.used_off + 2..l.used_off + 4].copy_from_slice(&n.wrapping_add(1).to_le_bytes());
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
        let mut buf = mem();
        let m = &mut buf.0[..l.bytes];
        let mut q = VirtQueue::new(m, l).unwrap();
        let (start, rounds) = wrap_plan();
        q.avail_idx = start as u16;
        q.last_used = start as u16;
        for round in 0..rounds {
            let n = start.wrapping_add(round) as u16;
            let head = q.add(&[(0x1000, 1)], &[]).unwrap();
            assert_eq!(rd16(q.mem, l.avail_off + 4 + 2 * usize::from(n % 4)), head);
            device_complete(q.mem, &l, n, u32::from(head), 1);
            assert_eq!(q.pop_used(), Some((head, 1)));
        }
        for _ in 0..4 {
            q.add(&[(0x1000, 1)], &[]).unwrap();
        }
        assert_eq!(q.add(&[(0x1000, 1)], &[]), None);
    }

    #[test]
    fn new_rejects_a_short_buffer() {
        let l = layout8();
        let mut buf = mem();
        assert!(VirtQueue::new(&mut buf.0[..l.bytes - 1], l).is_none());
        assert!(VirtQueue::new(&mut buf.0[..l.bytes], l).is_some());
        let big = queue_layout(16).unwrap();
        assert!(VirtQueue::new(&mut buf.0[..big.bytes], big).is_none());
    }

    #[test]
    fn new_rejects_an_unaligned_buffer() {
        let l = layout8();
        let mut buf = mem();
        assert!(VirtQueue::new(&mut buf.0[1..=l.bytes], l).is_none());
        assert!(VirtQueue::new(&mut buf.0[8..8 + l.bytes], l).is_none());
        assert!(VirtQueue::new(&mut buf.0[16..16 + l.bytes], l).is_some());
    }

    #[test]
    fn new_rejects_an_inconsistent_layout() {
        let good = layout8();
        let mut buf = mem();
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
            assert!(VirtQueue::new(&mut buf.0[..], bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn addrs_rejects_an_unaligned_base() {
        let l = layout8();
        assert_eq!(l.addrs(0x1008), None);
        assert!(l.addrs(0x1010).is_some());
    }

    #[test]
    fn add_builds_an_out_then_in_chain() {
        let l = layout8();
        let mut mem_buf = mem();
        let mem = &mut mem_buf.0[..l.bytes];
        let mut q = VirtQueue::new(mem, l).unwrap();
        let head = q
            .add(&[(0x1_0000_2000, 16)], &[(0x3000, 4096), (0x4000, 1)])
            .unwrap();
        assert_eq!(q.num_free(), 5);
        let m = &mem[..];
        let d = |i: usize| &m[i * 16..i * 16 + 16];
        let flags = |i: usize| rd16(d(i), 12);
        let h = usize::from(head);
        assert_eq!(flags(h), DESC_F_NEXT);
        assert_eq!(
            u64::from_le_bytes(d(h)[0..8].try_into().unwrap()),
            0x1_0000_2000
        );
        let n1 = usize::from(rd16(d(h), 14));
        assert_eq!(flags(n1), DESC_F_WRITE | DESC_F_NEXT);
        let n2 = usize::from(rd16(d(n1), 14));
        assert_eq!(flags(n2), DESC_F_WRITE);
        assert_eq!(u32::from_le_bytes(d(n2)[8..12].try_into().unwrap()), 1);
        assert_eq!(rd16(m, l.avail_off + 4), head);
        assert_eq!(rd16(m, l.avail_off + 2), 1);
    }

    #[test]
    fn add_without_enough_descriptors_is_none_and_changes_nothing() {
        let l = layout8();
        let mut mem_buf = mem();
        let mem = &mut mem_buf.0[..l.bytes];
        let mut q = VirtQueue::new(mem, l).unwrap();
        q.add(&[(1, 1); 3], &[(2, 1); 3]).unwrap();
        assert_eq!(q.num_free(), 2);
        assert_eq!(q.add(&[(1, 1); 3], &[]), None);
        assert_eq!(q.num_free(), 2);
        // The free list is intact: exactly two more descriptors can be had.
        assert!(q.add(&[(1, 1)], &[(2, 1)]).is_some());
        assert_eq!(q.num_free(), 0);
        assert_eq!(q.add(&[(1, 1)], &[]), None);
        assert_eq!(rd16(mem, l.avail_off + 2), 2);
    }

    #[test]
    fn add_with_no_buffers_is_none() {
        let l = layout8();
        let mut mem_buf = mem();
        let mem = &mut mem_buf.0[..l.bytes];
        let mut q = VirtQueue::new(mem, l).unwrap();
        assert_eq!(q.add(&[], &[]), None);
        assert_eq!(q.num_free(), 8);
    }

    #[test]
    fn pop_used_returns_the_chain_to_the_free_list() {
        let l = layout8();
        let mut mem_buf = mem();
        let mem = &mut mem_buf.0[..l.bytes];
        let mut q = VirtQueue::new(mem, l).unwrap();
        let head = q
            .add(&[(0x1000, 16)], &[(0x2000, 512), (0x3000, 1)])
            .unwrap();
        device_complete(q.mem, &l, 0, u32::from(head), 513);
        assert_eq!(q.pop_used(), Some((head, 513)));
        assert_eq!(q.num_free(), 8);
        assert_eq!(q.pop_used(), None);
    }

    #[test]
    fn indices_wrap_modulo_two_to_the_sixteen() {
        let l = layout8();
        let mut mem_buf = mem();
        let mem = &mut mem_buf.0[..l.bytes];
        let mut q = VirtQueue::new(mem, l).unwrap();
        let (start, rounds) = wrap_plan();
        q.avail_idx = start as u16;
        q.last_used = start as u16;
        for round in 0..rounds {
            let n = start.wrapping_add(round) as u16;
            let head = q.add(&[(0x1000, 1)], &[]).unwrap();
            let slot = usize::from(n % 8);
            let ring = l.avail_off + 4 + 2 * slot;
            assert_eq!(rd16(q.mem, ring), head, "round {round}");
            assert_eq!(rd16(q.mem, l.avail_off + 2), n.wrapping_add(1));
            device_complete(q.mem, &l, n, u32::from(head), 1);
            assert_eq!(q.pop_used(), Some((head, 1)), "round {round}");
            assert_eq!(q.num_free(), 8);
        }
        assert_eq!(
            rd16(q.mem, l.avail_off + 2),
            start.wrapping_add(rounds) as u16
        );
        assert_eq!(q.pop_used(), None);
    }

    #[test]
    fn out_of_range_or_stale_used_ids_are_skipped() {
        let l = layout8();
        let mut mem_buf = mem();
        let mem = &mut mem_buf.0[..l.bytes];
        let mut q = VirtQueue::new(mem, l).unwrap();
        let head = q.add(&[(0x1000, 1)], &[]).unwrap();
        device_complete(q.mem, &l, 0, 8, 0);
        device_complete(q.mem, &l, 1, 0xFFFF_FFFF, 0);
        device_complete(q.mem, &l, 2, 7, 0); // in range but never added
        device_complete(q.mem, &l, 3, u32::from(head), 9);
        assert_eq!(q.pop_used(), Some((head, 9)));
        assert_eq!(q.num_free(), 8);
        // The same head again is stale now.
        device_complete(q.mem, &l, 4, u32::from(head), 9);
        assert_eq!(q.pop_used(), None);
        assert_eq!(q.num_free(), 8);
    }

    #[test]
    fn a_looping_descriptor_chain_cannot_hang_or_corrupt_the_free_list() {
        let l = layout8();
        let mut mem_buf = mem();
        let mem = &mut mem_buf.0[..l.bytes];
        let mut q = VirtQueue::new(mem, l).unwrap();
        let head = q.add(&[(1, 1), (2, 1)], &[(3, 1)]).unwrap();
        // The "device" rewrites every descriptor's next to point at itself.
        for i in 0..8usize {
            q.mem[i * 16 + 12..i * 16 + 14].copy_from_slice(&DESC_F_NEXT.to_le_bytes());
            q.mem[i * 16 + 14..i * 16 + 16].copy_from_slice(&(i as u16).to_le_bytes());
        }
        device_complete(q.mem, &l, 0, u32::from(head), 1);
        assert_eq!(q.pop_used(), Some((head, 1)));
        assert_eq!(q.num_free(), 8);
        // Every descriptor is still allocatable exactly once.
        assert!(q.add(&[(1, 1); 4], &[(2, 1); 4]).is_some());
        assert_eq!(q.num_free(), 0);
    }

    #[test]
    fn a_runaway_used_index_is_bounded() {
        let l = layout8();
        let mut mem_buf = mem();
        let mem = &mut mem_buf.0[..l.bytes];
        let mut q = VirtQueue::new(mem, l).unwrap();
        q.mem[l.used_off + 2..l.used_off + 4].copy_from_slice(&60_000u16.to_le_bytes());
        // Slot contents are all id 0, never in flight: skipped, one queue's worth per call.
        assert_eq!(q.pop_used(), None);
    }
}
