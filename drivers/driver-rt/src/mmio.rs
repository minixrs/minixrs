// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The virtio-mmio **version 2** (modern) transport (slice 6.2, spec T10).
//!
//! The only `unsafe` in the crate: every access is a volatile, aligned, 32-bit
//! load or store, because the page is Device memory. 64-bit values travel as
//! low/high pairs. The legacy (version 1) layout is not supported.

use crate::transport::{QueueAddrs, Transport};

const MAGIC_VALUE: usize = 0x000;
const VERSION: usize = 0x004;
const DEVICE_ID: usize = 0x008;
const DEVICE_FEATURES: usize = 0x010;
const DEVICE_FEATURES_SEL: usize = 0x014;
const DRIVER_FEATURES: usize = 0x020;
const DRIVER_FEATURES_SEL: usize = 0x024;
const QUEUE_SEL: usize = 0x030;
const QUEUE_NUM_MAX: usize = 0x034;
const QUEUE_NUM: usize = 0x038;
const QUEUE_READY: usize = 0x044;
const QUEUE_NOTIFY: usize = 0x050;
const INTERRUPT_STATUS: usize = 0x060;
const INTERRUPT_ACK: usize = 0x064;
const STATUS: usize = 0x070;
const QUEUE_DESC_LOW: usize = 0x080;
const QUEUE_DESC_HIGH: usize = 0x084;
const QUEUE_DRIVER_LOW: usize = 0x090;
const QUEUE_DRIVER_HIGH: usize = 0x094;
const QUEUE_DEVICE_LOW: usize = 0x0A0;
const QUEUE_DEVICE_HIGH: usize = 0x0A4;
const CONFIG_GENERATION: usize = 0x0FC;
const CONFIG: usize = 0x100;
/// Size of one slot's register window.
const SLOT_SIZE: usize = 0x200;

/// The magic a virtio-mmio device answers at offset 0 ("virt", little-endian).
const MAGIC: u32 = 0x7472_6976;

/// Why [`MmioTransport::new`] refused a slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MmioError {
    /// `MagicValue` was not the virtio magic.
    BadMagic(u32),
    /// `Version` was not 2 (1 is the legacy layout).
    LegacyVersion(u32),
}

/// One virtio-mmio v2 slot mapped in this process.
pub struct MmioTransport {
    base: *mut u32,
}

impl MmioTransport {
    /// Check the magic and version of the slot at `base_va`.
    ///
    /// # Safety
    /// `base_va` is this process's mapping of one virtio-mmio slot, valid for
    /// 0x200 bytes, 4-byte aligned, and not accessed through any other path.
    pub unsafe fn new(base_va: u64) -> Result<Self, MmioError> {
        let t = Self {
            base: base_va as usize as *mut u32,
        };
        let magic = t.read(MAGIC_VALUE);
        if magic != MAGIC {
            return Err(MmioError::BadMagic(magic));
        }
        let version = t.read(VERSION);
        if version != 2 {
            return Err(MmioError::LegacyVersion(version));
        }
        Ok(t)
    }

    fn read(&self, off: usize) -> u32 {
        debug_assert!(off.is_multiple_of(4) && off < SLOT_SIZE);
        // SAFETY: `new`'s contract makes `base` valid for SLOT_SIZE bytes;
        // `off` is a 4-aligned register offset inside it. Volatile because the
        // access is a device register access the compiler must not elide.
        unsafe { self.base.byte_add(off).read_volatile() }
    }

    fn write(&mut self, off: usize, val: u32) {
        debug_assert!(off.is_multiple_of(4) && off < SLOT_SIZE);
        // SAFETY: as in `read`; the store is a register write, never elided.
        unsafe { self.base.byte_add(off).write_volatile(val) }
    }

    fn write64(&mut self, low: usize, high: usize, val: u64) {
        self.write(low, val as u32);
        self.write(high, (val >> 32) as u32);
    }
}

impl Transport for MmioTransport {
    fn device_id(&self) -> u32 {
        self.read(DEVICE_ID)
    }
    fn status(&self) -> u32 {
        self.read(STATUS)
    }
    fn set_status(&mut self, status: u32) {
        self.write(STATUS, status);
    }
    fn device_features(&mut self) -> u64 {
        self.write(DEVICE_FEATURES_SEL, 0);
        let low = u64::from(self.read(DEVICE_FEATURES));
        self.write(DEVICE_FEATURES_SEL, 1);
        let high = u64::from(self.read(DEVICE_FEATURES));
        (high << 32) | low
    }
    fn set_driver_features(&mut self, features: u64) {
        self.write(DRIVER_FEATURES_SEL, 0);
        self.write(DRIVER_FEATURES, features as u32);
        self.write(DRIVER_FEATURES_SEL, 1);
        self.write(DRIVER_FEATURES, (features >> 32) as u32);
    }
    fn queue_max(&mut self, queue: u16) -> u16 {
        self.write(QUEUE_SEL, u32::from(queue));
        // QueueNumMax is at most 32768, so it fits; saturate on a bad device.
        u16::try_from(self.read(QUEUE_NUM_MAX)).unwrap_or(u16::MAX)
    }
    fn queue_enable(&mut self, queue: u16, size: u16, addrs: &QueueAddrs) {
        self.write(QUEUE_SEL, u32::from(queue));
        self.write(QUEUE_NUM, u32::from(size));
        self.write64(QUEUE_DESC_LOW, QUEUE_DESC_HIGH, addrs.desc_pa);
        self.write64(QUEUE_DRIVER_LOW, QUEUE_DRIVER_HIGH, addrs.avail_pa);
        self.write64(QUEUE_DEVICE_LOW, QUEUE_DEVICE_HIGH, addrs.used_pa);
        self.write(QUEUE_READY, 1);
    }
    fn notify(&mut self, queue: u16) {
        self.write(QUEUE_NOTIFY, u32::from(queue));
    }
    fn interrupt_status(&self) -> u32 {
        self.read(INTERRUPT_STATUS)
    }
    fn interrupt_ack(&mut self, bits: u32) {
        self.write(INTERRUPT_ACK, bits);
    }
    fn config_generation(&self) -> u32 {
        self.read(CONFIG_GENERATION)
    }
    /// Aligned 32-bit accesses only, bytes taken little-endian from each word. Virtio
    /// permits that only for 32- and 64-bit config fields; 8- and 16-bit fields need
    /// width-typed reads that do not exist yet. Bytes outside the slot (or past an
    /// offset overflow) read as 0.
    fn config_read_raw(&self, offset: usize, buf: &mut [u8]) {
        for (i, byte) in buf.iter_mut().enumerate() {
            let reg = offset
                .checked_add(i)
                .and_then(|pos| CONFIG.checked_add(pos & !3).map(|reg| (pos, reg)));
            *byte = match reg {
                Some((pos, reg)) if reg < SLOT_SIZE => self.read(reg).to_le_bytes()[pos & 3],
                _ => 0,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::virtio::{
        VIRTIO_MMIO_DEVICE_ID_OFF, VIRTIO_MMIO_MAGIC, VIRTIO_MMIO_MAGIC_OFF,
        VIRTIO_MMIO_VERSION_OFF,
    };

    #[test]
    fn the_probe_offsets_match_kernel_shared() {
        assert_eq!(MAGIC_VALUE, VIRTIO_MMIO_MAGIC_OFF);
        assert_eq!(VERSION, VIRTIO_MMIO_VERSION_OFF);
        assert_eq!(DEVICE_ID, VIRTIO_MMIO_DEVICE_ID_OFF);
        assert_eq!(MAGIC, VIRTIO_MMIO_MAGIC);
    }
}
