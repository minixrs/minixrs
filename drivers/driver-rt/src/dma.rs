// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Typed handles on a driver's DMA run (slice 6.3, spec B8).
//!
//! The kernel maps one contiguous run of RAM into a virtio driver and reports its
//! virtual address, physical address and length. The device reads and writes that
//! memory too, so no Rust reference (`&` / `&mut`) to it may ever exist: a
//! reference promises the compiler the bytes do not change behind it, or that
//! nothing else writes them. Everything here is therefore a raw pointer and a
//! volatile access.
//!
//! [`DmaRegion::new`] is the one `unsafe` door: it turns the kernel's report into a
//! region. [`DmaRegion::buf`] then hands out bounds-checked [`DmaBuf`]s, and every
//! address a device is ever given (a queue's three areas, a descriptor's buffer)
//! comes out of a `DmaBuf`, so safe code cannot aim the device at memory outside
//! the run. `DmaBuf`s may overlap freely: none is a reference, and every access
//! goes through a raw pointer whose provenance is the region's.

use core::ptr::NonNull;

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

/// A kernel-reported DMA run, mapped in this process for its whole life.
#[derive(Debug)]
pub struct DmaRegion {
    va: NonNull<u8>,
    pa: u64,
    len: usize,
}

impl DmaRegion {
    /// Adopt the run the kernel reported.
    ///
    /// `None` for a null `va`, a zero `len`, a `va` or `len` that does not fit the
    /// address space (`len` must also fit `isize`), or a run whose `va + len` or
    /// `pa + len` overflows. Those refusals are decided from the three numbers
    /// alone, before anything relies on the contract below.
    ///
    /// # Safety
    /// `run` is what the kernel reported (`GET_DMA`): `run.len` bytes mapped
    /// read-write at `run.va` in this address space, backed by physical memory
    /// starting at `run.pa`, and never unmapped for the life of the process. No
    /// Rust reference (`&` / `&mut`) to that memory exists or is ever created: the
    /// device writes it too, so all access goes through this module's volatile
    /// raw-pointer accessors.
    pub unsafe fn new(run: DmaRun) -> Option<Self> {
        let addr = usize::try_from(run.va).ok()?;
        let len = usize::try_from(run.len).ok()?;
        // The address came from the kernel as an integer; the mapping it names was
        // made outside Rust, so its provenance is the exposed kind.
        let va = NonNull::new(core::ptr::with_exposed_provenance_mut::<u8>(addr))?;
        // SAFETY: the caller's contract, with `va` standing for `run.va`.
        unsafe { Self::from_ptr(va, run.pa, len) }
    }

    /// [`Self::new`] for a run already held as a pointer, with the same refusals.
    /// Split out so the host tests can hand in a pointer that kept its provenance,
    /// which lets Miri check every access against the exact allocation.
    ///
    /// # Safety
    /// As [`Self::new`], for the `len` bytes at `va` backed by `pa`; `va` has
    /// provenance over all of them, for reads and writes.
    unsafe fn from_ptr(va: NonNull<u8>, pa: u64, len: usize) -> Option<Self> {
        if len == 0 || isize::try_from(len).is_err() {
            return None;
        }
        va.as_ptr().addr().checked_add(len)?;
        pa.checked_add(u64::try_from(len).ok()?)?;
        Some(Self { va, pa, len })
    }

    /// The `len` bytes at `offset`, as a handle the device can be pointed at.
    /// `None` if `len` is 0 or above `u32::MAX` (a descriptor's length field), or
    /// the range leaves the region.
    pub fn buf(&self, offset: usize, len: usize) -> Option<DmaBuf> {
        let len32 = u32::try_from(len).ok()?;
        let end = offset.checked_add(len)?;
        if len == 0 || end > self.len {
            return None;
        }
        // `offset < self.len`, which `new` proved fits both address spaces.
        let pa = self.pa.checked_add(u64::try_from(offset).ok()?)?;
        // `wrapping_add` keeps the region's provenance and needs no `unsafe`; the
        // result is in bounds by the check above, hence also non-null.
        let va = NonNull::new(self.va.as_ptr().wrapping_add(offset))?;
        Some(DmaBuf { va, pa, len: len32 })
    }

    /// The run this region covers.
    pub fn run(&self) -> DmaRun {
        DmaRun {
            va: self.va.as_ptr().addr() as u64,
            pa: self.pa,
            len: self.len as u64,
        }
    }
}

/// A bounds-checked sub-range of a [`DmaRegion`]: what a descriptor may name.
#[derive(Clone, Copy, Debug)]
pub struct DmaBuf {
    va: NonNull<u8>,
    pa: u64,
    len: u32,
}

macro_rules! volatile_rw {
    ($rd:ident, $wr:ident, $t:ty) => {
        /// One aligned little-endian volatile load at `off`; `None` when the range
        /// leaves the buffer or `off` is misaligned for the type.
        pub(crate) fn $rd(&self, off: usize) -> Option<$t> {
            let p = self.ptr(off, core::mem::size_of::<$t>())?.cast::<$t>();
            if !p.is_aligned() {
                return None;
            }
            // SAFETY: `ptr` proved the whole access lies inside this buffer, which
            // `DmaRegion::buf` proved lies inside a region mapped read-write for the
            // life of the process (`DmaRegion::new`'s contract); `p` carries the
            // region's provenance and is aligned for the type. Volatile, and no
            // reference is formed, because the device writes this memory.
            Some(<$t>::from_le(unsafe { p.read_volatile() }))
        }

        /// One aligned little-endian volatile store at `off` (a single store of
        /// the type's width, never torn); `None` as for the load.
        pub(crate) fn $wr(&self, off: usize, v: $t) -> Option<()> {
            let p = self.ptr(off, core::mem::size_of::<$t>())?.cast::<$t>();
            if !p.is_aligned() {
                return None;
            }
            // SAFETY: as for the load; the region is mapped writable, and the
            // pointer has write provenance (it never passed through a reference).
            unsafe { p.write_volatile(v.to_le()) };
            Some(())
        }
    };
}

impl DmaBuf {
    /// Device-visible physical address of the first byte.
    pub fn pa(&self) -> u64 {
        self.pa
    }

    /// Length in bytes (never 0).
    pub fn len(&self) -> u32 {
        self.len
    }

    /// Always `false`: [`DmaRegion::buf`] refuses an empty range.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Driver-visible virtual address of the first byte (for example, to name
    /// this buffer in a `sys_safecopy`).
    pub fn va(&self) -> u64 {
        self.va.as_ptr().addr() as u64
    }

    /// Pointer to `off..off + n` of the buffer, or `None` if that leaves it.
    fn ptr(&self, off: usize, n: usize) -> Option<*mut u8> {
        let end = off.checked_add(n)?;
        if end > self.len as usize {
            return None;
        }
        Some(self.va.as_ptr().wrapping_add(off))
    }

    /// Copy `dst.len()` bytes from `offset` into `dst`, one volatile byte at a
    /// time. `false`, with nothing copied, when the range leaves the buffer.
    #[must_use]
    pub fn read_into(&self, offset: usize, dst: &mut [u8]) -> bool {
        let Some(p) = self.ptr(offset, dst.len()) else {
            return false;
        };
        for (i, byte) in dst.iter_mut().enumerate() {
            // SAFETY: `ptr` proved `offset..offset + dst.len()` lies inside this
            // buffer, itself inside a region mapped for the life of the process
            // (`DmaRegion::new`'s contract), and `i < dst.len()`. `dst` is a Rust
            // reference, so by that same contract it is not this memory. Volatile
            // because the device writes it.
            *byte = unsafe { p.wrapping_add(i).read_volatile() };
        }
        true
    }

    /// Copy `src` to `offset`, one volatile byte at a time. `false`, with nothing
    /// copied, when the range leaves the buffer.
    #[must_use]
    pub fn write_from(&self, offset: usize, src: &[u8]) -> bool {
        let Some(p) = self.ptr(offset, src.len()) else {
            return false;
        };
        for (i, byte) in src.iter().enumerate() {
            // SAFETY: as in `read_into`; the region is mapped writable and the
            // pointer has write provenance (it never passed through a reference).
            unsafe { p.wrapping_add(i).write_volatile(*byte) };
        }
        true
    }

    /// One volatile byte store at `off`; `None` when `off` is outside the buffer.
    pub(crate) fn wr8(&self, off: usize, v: u8) -> Option<()> {
        let p = self.ptr(off, 1)?;
        // SAFETY: as in `write_from`, for the one byte `ptr` proved in bounds.
        unsafe { p.write_volatile(v) };
        Some(())
    }

    volatile_rw!(rd16, wr16, u16);
    volatile_rw!(rd32, wr32, u32);
}

/// Host memory standing in for a DMA run, for this crate's tests.
#[cfg(test)]
pub(crate) mod testmem {
    use super::{DmaBuf, DmaRegion};
    use core::ptr::NonNull;

    /// Bytes in a [`HostDma`].
    pub(crate) const HOST_DMA_BYTES: usize = 2 * 4096;

    #[repr(C, align(4096))]
    struct Pages([u8; HOST_DMA_BYTES]);

    /// Two zeroed, page-aligned host pages behind a [`DmaRegion`] with a made-up
    /// physical address. Freed on drop, so no handle may be used after that — the
    /// tests' stand-in for "never unmapped for the life of the process".
    pub(crate) struct HostDma {
        raw: *mut Pages,
        pub(crate) region: DmaRegion,
    }

    impl HostDma {
        pub(crate) fn new(pa: u64) -> Self {
            // `Box::into_raw` yields a pointer with write provenance and no live
            // reference. It is handed over as a pointer, not as an integer, so
            // Miri tracks every access against this exact allocation.
            let raw = Box::into_raw(Box::new(Pages([0; HOST_DMA_BYTES])));
            let va = NonNull::new(raw.cast::<u8>()).expect("a box is never null");
            // SAFETY: `raw` is `HOST_DMA_BYTES` of live, writable heap memory that
            // stays allocated until `drop`, and no reference to it exists: the box
            // was turned into a raw pointer and is touched again only by `drop`.
            let region =
                unsafe { DmaRegion::from_ptr(va, pa, HOST_DMA_BYTES) }.expect("a valid host run");
            Self { raw, region }
        }

        /// A handle on everything: the tests' "device side" view of the memory.
        pub(crate) fn all(&self) -> DmaBuf {
            self.region
                .buf(0, HOST_DMA_BYTES)
                .expect("the whole region")
        }
    }

    impl Drop for HostDma {
        fn drop(&mut self) {
            // SAFETY: `raw` came from `Box::into_raw` in `new` and is freed once.
            drop(unsafe { Box::from_raw(self.raw) });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testmem::{HOST_DMA_BYTES, HostDma};
    use super::*;

    const PA: u64 = 0x4800_0000;

    /// `DmaRegion::new` on a run it must refuse from the numbers alone.
    fn refused(run: DmaRun) -> bool {
        // SAFETY: every caller passes a run `new` rejects in its documented
        // up-front checks, before the contract is relied on; nothing is accessed.
        unsafe { DmaRegion::new(run) }.is_none()
    }

    #[test]
    fn new_refuses_null_empty_and_overflowing_runs() {
        let ok = DmaRun {
            va: 0x1000,
            pa: PA,
            len: 0x1000,
        };
        assert!(refused(DmaRun { va: 0, ..ok }));
        assert!(refused(DmaRun { len: 0, ..ok }));
        assert!(refused(DmaRun {
            len: u64::MAX,
            ..ok
        }));
        assert!(refused(DmaRun {
            len: isize::MAX as u64 + 1,
            ..ok
        }));
        assert!(refused(DmaRun {
            va: u64::MAX - 0xFFF,
            len: 0x2000,
            ..ok
        }));
        assert!(refused(DmaRun {
            pa: u64::MAX - 0xFFF,
            len: 0x2000,
            ..ok
        }));
    }

    #[test]
    fn new_adopts_a_run_reported_as_integers() {
        // The production door: the address arrives as a number, as it does from the
        // kernel, and the region must be usable through it.
        let raw = Box::into_raw(Box::new([0u64; 8]));
        let run = DmaRun {
            va: raw.expose_provenance() as u64,
            pa: PA,
            len: 64,
        };
        // SAFETY: 64 bytes of live, writable heap memory with no reference to it;
        // it is freed only after the last access through the region, below.
        let region = unsafe { DmaRegion::new(run) }.unwrap();
        assert_eq!(region.run(), run);
        let b = region.buf(56, 8).unwrap();
        assert_eq!((b.va(), b.pa(), b.len()), (run.va + 56, PA + 56, 8));
        assert!(b.write_from(0, &0x1122_3344_5566_7788u64.to_le_bytes()));
        assert_eq!(b.rd32(4), Some(0x1122_3344));
        let mut got = [0u8; 8];
        assert!(region.buf(0, 64).unwrap().read_into(56, &mut got));
        assert_eq!(u64::from_le_bytes(got), 0x1122_3344_5566_7788);
        // SAFETY: `raw` came from `Box::into_raw` above and is freed once.
        drop(unsafe { Box::from_raw(raw) });
    }

    #[test]
    fn run_reports_what_the_region_was_built_from() {
        let host = HostDma::new(PA);
        let run = host.region.run();
        assert_eq!(run.pa, PA);
        assert_eq!(run.len, HOST_DMA_BYTES as u64);
        assert_eq!(run.va, host.all().va());
        assert!(run.va.is_multiple_of(4096));
    }

    #[test]
    fn buf_translates_offset_to_both_addresses() {
        let host = HostDma::new(PA);
        let base = host.region.run().va;
        let b = host.region.buf(4096, 512).unwrap();
        assert_eq!(b.pa(), PA + 4096);
        assert_eq!(b.va(), base + 4096);
        assert_eq!(b.len(), 512);
        assert!(!b.is_empty());
        // The last byte is reachable; the whole region is one valid buffer.
        assert_eq!(
            host.region.buf(HOST_DMA_BYTES - 1, 1).unwrap().pa(),
            PA + HOST_DMA_BYTES as u64 - 1
        );
        assert_eq!(host.all().len() as usize, HOST_DMA_BYTES);
    }

    #[test]
    fn buf_refuses_empty_outside_and_overflowing_ranges() {
        let host = HostDma::new(PA);
        let r = &host.region;
        assert!(r.buf(0, 0).is_none());
        assert!(r.buf(HOST_DMA_BYTES, 0).is_none());
        assert!(r.buf(HOST_DMA_BYTES, 1).is_none());
        assert!(r.buf(0, HOST_DMA_BYTES + 1).is_none());
        assert!(r.buf(HOST_DMA_BYTES - 1, 2).is_none());
        assert!(r.buf(usize::MAX, 1).is_none());
        assert!(r.buf(usize::MAX, usize::MAX).is_none());
        assert!(r.buf(1, usize::MAX).is_none());
        // Longer than a descriptor's 32-bit length field, whatever the region.
        assert!(r.buf(0, u32::MAX as usize + 1).is_none());
    }

    #[test]
    fn bytes_round_trip_and_overlapping_buffers_see_each_other() {
        let host = HostDma::new(PA);
        let a = host.region.buf(16, 32).unwrap();
        let b = host.region.buf(24, 8).unwrap();
        assert!(a.write_from(8, &[1, 2, 3, 4, 5, 6, 7, 8]));
        let mut got = [0u8; 8];
        assert!(b.read_into(0, &mut got));
        assert_eq!(got, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(b.write_from(7, &[0xEE]));
        let mut tail = [0u8; 2];
        assert!(a.read_into(14, &mut tail));
        assert_eq!(tail, [7, 0xEE]);
    }

    #[test]
    fn copies_refuse_a_range_outside_the_buffer_and_copy_nothing() {
        let host = HostDma::new(PA);
        let b = host.region.buf(64, 8).unwrap();
        assert!(b.write_from(0, &[0xAA; 8]));
        // One byte too far, a start past the end, and an offset that overflows.
        let mut dst = [0x55u8; 8];
        assert!(!b.read_into(1, &mut dst));
        assert!(!b.read_into(9, &mut dst[..1]));
        assert!(!b.read_into(usize::MAX, &mut dst));
        assert_eq!(dst, [0x55; 8]);
        assert!(!b.write_from(1, &[0; 8]));
        assert!(!b.write_from(usize::MAX, &[0; 2]));
        // Neither the buffer nor the byte after it was touched.
        let mut seen = [0u8; 9];
        assert!(host.all().read_into(64, &mut seen));
        assert_eq!(seen, [0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0]);
        // An empty copy at the very end is in range.
        assert!(b.read_into(8, &mut []));
        assert!(b.write_from(8, &[]));
    }

    #[test]
    fn typed_accesses_are_little_endian_bounded_and_aligned() {
        let host = HostDma::new(PA);
        let b = host.region.buf(0, 8).unwrap();
        b.wr32(4, 0x1122_3344).unwrap();
        b.wr16(2, 0xBEEF).unwrap();
        b.wr8(0, 0x7F).unwrap();
        let mut raw = [0u8; 8];
        assert!(b.read_into(0, &mut raw));
        assert_eq!(raw, [0x7F, 0, 0xEF, 0xBE, 0x44, 0x33, 0x22, 0x11]);
        assert_eq!(b.rd16(2), Some(0xBEEF));
        assert_eq!(b.rd32(4), Some(0x1122_3344));
        // Misaligned for the type.
        assert_eq!(b.rd16(1), None);
        assert_eq!(b.rd32(2), None);
        assert_eq!(b.wr16(3, 0), None);
        assert_eq!(b.wr32(6, 0), None);
        // Past the end, straddling it, and overflowing.
        assert_eq!(b.rd16(8), None);
        assert_eq!(b.rd32(8), None);
        assert_eq!(b.wr32(8, 0), None);
        assert_eq!(b.wr8(8, 0), None);
        assert_eq!(b.rd32(usize::MAX - 3), None);
        assert_eq!(b.wr16(usize::MAX - 1, 0), None);
        assert_eq!(b.wr8(usize::MAX, 0), None);
        // A buffer that starts at an odd address: alignment is the address's.
        let odd = host.region.buf(1, 7).unwrap();
        assert_eq!(odd.rd16(0), None);
        assert_eq!(odd.rd16(1), Some(0xBEEF));
    }
}
