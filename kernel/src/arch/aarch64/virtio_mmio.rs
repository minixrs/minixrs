// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! virtio-mmio boot probe (slice 6.2, spec T4 and T7).
//!
//! QEMU `virt` places 32 virtio-mmio transports at `0x0A00_0000`, `0x200` apart,
//! with SPI `16 + slot` (INTID `48 + slot`). At boot [`probe`] reads
//! `MagicValue`, `Version` and `DeviceID` of every slot through the HHDM, hands
//! the triples to the pure [`assign`] in `kernel-shared`, prints one `[virtio]`
//! line per present device, and for each assigned driver reserves a
//! [`DMA_RUN_PAGES`]-page contiguous run, stamps its first eight bytes with
//! `pa ^ DMA_COOKIE`, and records a [`VirtioDev`]. Later slices map the page and
//! the run into the driver; this module only probes, reserves and records.
//!
//! The kernel does not judge `Version`: it prints it and passes it on.

use core::cell::UnsafeCell;
use core::fmt::Write;
use core::ptr::{read_volatile, write_volatile};

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::uspace::{DMA_COOKIE, DMA_RUN_PAGES};
use minixrs_kernel_shared::virtio::{
    NR_VIRTIO_DRIVERS, SlotProbe, SlotVerdict, VIRTIO_DRIVERS, VIRTIO_MMIO_DEVICE_ID_OFF,
    VIRTIO_MMIO_MAGIC_OFF, VIRTIO_MMIO_VERSION_OFF, assign, virtio_driver_index,
};

use crate::arch::aarch64::uart::Pl011;

/// Physical base of virtio-mmio slot 0 on QEMU `virt`.
pub const VIRTIO_MMIO_PHYS_BASE: u64 = 0x0A00_0000;
/// Distance between consecutive slots.
pub const VIRTIO_MMIO_SLOT_STRIDE: u64 = 0x200;
/// Number of transport slots.
pub const NR_VIRTIO_MMIO_SLOTS: usize = 32;
/// INTID of slot 0 (SPI 16); slot `n` is `48 + n`.
pub const VIRTIO_MMIO_SLOT0_INTID: u32 = 48;

/// Slots sharing one 4 KiB page.
const SLOTS_PER_PAGE: usize = 4096 / VIRTIO_MMIO_SLOT_STRIDE as usize;

/// What the probe decided for one driver: where its device is and its DMA run.
#[derive(Clone, Copy)]
pub struct VirtioDev {
    pub slot: u32,
    /// PA of the 4 KiB page holding the slot's registers.
    pub page_pa: u64,
    /// Offset of the slot's registers within that page.
    pub in_page_off: u64,
    pub intid: u32,
    /// PA of the first frame of the driver's DMA run.
    pub dma_pa: u64,
}

#[repr(transparent)]
struct Assignments(UnsafeCell<[Option<VirtioDev>; NR_VIRTIO_DRIVERS]>);
// SAFETY: written once during single-threaded boot by `probe`, read-only after.
unsafe impl Sync for Assignments {}

static ASSIGNMENTS: Assignments = Assignments(UnsafeCell::new([None; NR_VIRTIO_DRIVERS]));

const fn slot_pa(slot: usize) -> u64 {
    VIRTIO_MMIO_PHYS_BASE + slot as u64 * VIRTIO_MMIO_SLOT_STRIDE
}

/// Read one 32-bit register of `slot` through the HHDM.
///
/// SAFETY: the HHDM must be up; every virtio-mmio slot is inside the blanket
/// `[0, 4 GiB)` HHDM mapping (measured readable without a fault on QEMU `virt`).
unsafe fn read_reg(slot: usize, off: usize) -> u32 {
    let p = crate::mm::phys_to_hhdm(slot_pa(slot) + off as u64) as *const u32;
    // SAFETY: caller's contract; volatile because this is MMIO.
    unsafe { read_volatile(p) }
}

/// Probe the transports, print the markers, reserve the DMA runs and record the
/// assignments.
///
/// SAFETY: once, single-threaded boot, after the frame allocator and HHDM are up.
pub unsafe fn probe() {
    let mut slots = [SlotProbe {
        magic: 0,
        version: 0,
        device_id: 0,
    }; NR_VIRTIO_MMIO_SLOTS];
    for (i, s) in slots.iter_mut().enumerate() {
        // SAFETY: caller's contract; slot index is below NR_VIRTIO_MMIO_SLOTS.
        *s = unsafe {
            SlotProbe {
                magic: read_reg(i, VIRTIO_MMIO_MAGIC_OFF),
                version: read_reg(i, VIRTIO_MMIO_VERSION_OFF),
                device_id: read_reg(i, VIRTIO_MMIO_DEVICE_ID_OFF),
            }
        };
    }

    let image = crate::boot_image::BootImage::get();
    let mut has_module = [false; NR_VIRTIO_DRIVERS];
    for (d, flag) in has_module.iter_mut().enumerate() {
        *flag = image.module_by_proc_nr(VIRTIO_DRIVERS[d].nr).is_some();
    }

    let (verdicts, assigned) = assign(&slots, SLOTS_PER_PAGE, has_module);

    let mut uart = Pl011::new();
    for (i, v) in verdicts.iter().enumerate() {
        let s = &slots[i];
        let pa = slot_pa(i);
        match *v {
            SlotVerdict::Empty => {}
            SlotVerdict::SharedPage => {
                let _ = writeln!(
                    uart,
                    "[virtio] refuse slot={} id={} page={:#x} shared",
                    i,
                    s.device_id,
                    pa & !0xFFF,
                );
            }
            SlotVerdict::Assigned(d) => {
                let _ = writeln!(
                    uart,
                    "[virtio] slot={} pa={:#x} id={} ver={} drv={}",
                    i, pa, s.device_id, s.version, VIRTIO_DRIVERS[d].name,
                );
            }
            SlotVerdict::NoDriver => {
                let _ = writeln!(
                    uart,
                    "[virtio] slot={} pa={:#x} id={} ver={} skip=no-driver",
                    i, pa, s.device_id, s.version,
                );
            }
            SlotVerdict::NoModule(_) => {
                let _ = writeln!(
                    uart,
                    "[virtio] slot={} pa={:#x} id={} ver={} skip=no-module",
                    i, pa, s.device_id, s.version,
                );
            }
            SlotVerdict::Duplicate(_) => {
                let _ = writeln!(
                    uart,
                    "[virtio] slot={} pa={:#x} id={} ver={} skip=duplicate",
                    i, pa, s.device_id, s.version,
                );
            }
        }
    }

    // SAFETY: single-threaded boot; sole writer of the static.
    let table = unsafe { &mut *ASSIGNMENTS.0.get() };
    for (d, slot) in assigned.iter().enumerate() {
        let Some(slot) = *slot else { continue };
        let run = crate::mm::reserve_contiguous(DMA_RUN_PAGES as usize)
            .expect("no contiguous DMA run available for a virtio driver");
        let dma_pa = run.addr();
        // Stamp the run so the driver can prove its VA and the PA it was told
        // name the same memory (T7).
        // SAFETY: the run is exclusively ours, HHDM-mapped, 8-byte aligned.
        unsafe {
            write_volatile(
                crate::mm::phys_to_hhdm(dma_pa) as *mut u64,
                dma_pa ^ DMA_COOKIE,
            );
        }
        let pa = slot_pa(slot);
        table[d] = Some(VirtioDev {
            slot: slot as u32,
            page_pa: pa & !0xFFF,
            in_page_off: pa & 0xFFF,
            intid: VIRTIO_MMIO_SLOT0_INTID + slot as u32,
            dma_pa,
        });
    }
}

/// The device and DMA run the probe assigned to boot driver `nr`, if any.
pub fn assignment(nr: ProcNr) -> Option<VirtioDev> {
    let d = virtio_driver_index(nr)?;
    // SAFETY: read-only after boot; `probe` is the only writer and has finished.
    unsafe { (*ASSIGNMENTS.0.get())[d] }
}
