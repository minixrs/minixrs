// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_GETINFO` — kernel-state introspection.
//!
//! The request sub-type lives in the first 4 bytes of the message payload
//! (mirrors MINIX 3 `mess_lsys_krn_sys_getinfo.request`). Four sub-types are
//! implemented: `GET_WHOAMI` (Phase 2), `GET_RAMDISK` (slice 5.7), and
//! `GET_VIRTIO_MMIO` / `GET_DMA` (slice 6.2). Every other
//! sub-type returns `EINVAL` so a caller that sends an unsupported request gets a
//! recognizable error rather than a silently-zeroed reply.

use minixrs_kernel_shared::callnr::{
    GET_DMA, GET_RAMDISK, GET_VIRTIO_MMIO, GET_WHOAMI, GETINFO_DMA_LEN_OFF, GETINFO_DMA_PA_OFF,
    GETINFO_DMA_VA_OFF, GETINFO_RAMDISK_LEN_OFF, GETINFO_RAMDISK_VA_OFF, GETINFO_VIRTIO_INTID_OFF,
    GETINFO_VIRTIO_SLOT_OFF, GETINFO_VIRTIO_VA_OFF, SYS_GETINFO_NAME_LEN,
};
use minixrs_kernel_shared::com::{MEM_PROC_NR, ROOTFS_MODULE_NAME};
use minixrs_kernel_shared::error::{EINVAL, ENOENT, EPERM, OK};
use minixrs_kernel_shared::message::Message;
use minixrs_kernel_shared::uspace::{DMA_RUN_PAGES, DMA_VA, RAMDISK_VA, VIRTIO_MMIO_PAGE_VA};
use minixrs_kernel_shared::virtio::virtio_driver_index;

use crate::arch::aarch64::virtio_mmio::assignment;
use crate::proc::proc_struct::PROC_NAME_LEN;
use crate::proc::{Priv, Proc};

// The GET_WHOAMI reply embeds `caller.name` verbatim, so the kernel's
// per-slot name field and the wire-format name field must stay the same
// size. If `PROC_NAME_LEN` ever changes, update `SYS_GETINFO_NAME_LEN`
// (and the layout-table comment below) to match.
const _: () = assert!(PROC_NAME_LEN == SYS_GETINFO_NAME_LEN);

/// `SYS_GETINFO` entry point. Dispatches by request sub-type.
pub(super) fn do_getinfo(caller: &mut Proc, caller_priv: &Priv, msg: &mut Message) -> i32 {
    let request = i32::from_ne_bytes(
        msg.payload[0..4]
            .try_into()
            .expect("payload is at least 4 bytes"),
    );
    match request {
        GET_WHOAMI => fill_whoami(caller, caller_priv, msg),
        GET_RAMDISK => fill_ramdisk(caller, msg),
        GET_VIRTIO_MMIO => fill_virtio_mmio(caller, msg),
        GET_DMA => fill_dma(caller, msg),
        _ => EINVAL,
    }
}

/// `GET_WHOAMI` reply — fills `msg.payload` in-place. Layout:
///
/// | offset  | type     | meaning                            |
/// |---------|----------|------------------------------------|
/// |   0..4  | i32      | caller endpoint                    |
/// |   4..8  | i32      | `Priv::flags`, zero-extended       |
/// |   8..12 | i32      | init flags (always 0 for Phase 2)  |
/// |  12..28 | [u8; 16] | `Proc::name`, NUL-padded           |
fn fill_whoami(caller: &Proc, caller_priv: &Priv, msg: &mut Message) -> i32 {
    msg.payload[0..4].copy_from_slice(&caller.endpoint.to_ne_bytes());
    msg.payload[4..8].copy_from_slice(&(caller_priv.flags as i32).to_ne_bytes());
    msg.payload[8..12].copy_from_slice(&0_i32.to_ne_bytes());
    msg.payload[12..12 + PROC_NAME_LEN].copy_from_slice(&caller.name);
    OK
}

/// `GET_RAMDISK` reply (slice 5.7) — where the boot ramdisk was mapped, and how
/// long it is:
///
/// | offset  | type | meaning                     |
/// |---------|------|-----------------------------|
/// |   0..8  | u64  | `uspace::RAMDISK_VA`        |
/// |   8..16 | u64  | image length in bytes       |
///
/// **Gated on the caller being the `memory` driver.** The ramdisk is pre-mapped
/// into exactly one address space — `arch::aarch64::userland::load_boot_server`
/// installs it under a `nr == MEM_PROC_NR` arm — so the VA is meaningless in any
/// other, and handing it out would be handing out an address that faults. The two
/// `MEM_PROC_NR` tests must agree: if the loader's arm and this gate ever named
/// different procs, the outcome is either a driver whose every access is `EFAULT`
/// (gate too narrow) or a driver aimed at an unmapped VA (gate too wide).
///
/// No new kernel state backs this. The VA is a constant and the length is the
/// `rootfs` MXBI module's, read straight out of the archive — so there is nothing
/// here that a `SYS_EXEC` or a reboot could leave stale. `ENOENT` when the module
/// is absent, which cannot happen with a `kernel/build.rs` that packed it, but is
/// the honest answer rather than a zero length the driver would have to
/// special-case.
fn fill_ramdisk(caller: &Proc, msg: &mut Message) -> i32 {
    if caller.nr != MEM_PROC_NR {
        return EPERM;
    }
    let Some(blob) = crate::boot_image::BootImage::get().module_by_name(ROOTFS_MODULE_NAME) else {
        return ENOENT;
    };
    msg.payload[GETINFO_RAMDISK_VA_OFF..GETINFO_RAMDISK_VA_OFF + 8]
        .copy_from_slice(&RAMDISK_VA.to_ne_bytes());
    msg.payload[GETINFO_RAMDISK_LEN_OFF..GETINFO_RAMDISK_LEN_OFF + 8]
        .copy_from_slice(&(blob.len() as u64).to_ne_bytes());
    OK
}

/// `GET_VIRTIO_MMIO` reply (slice 6.2, spec T8) -- where the driver's register
/// page is mapped, its interrupt line, and its transport slot:
///
/// | offset  | type | meaning                                              |
/// |---------|------|------------------------------------------------------|
/// |   0..8  | u64  | `VIRTIO_MMIO_PAGE_VA` + the slot's offset in its page |
/// |   8..12 | u32  | INTID                                                |
/// |  12..16 | u32  | transport slot                                       |
///
/// **Gated on the caller being one of the four virtio driver procs**: the VA means
/// nothing in any other address space, so `EPERM`. A driver with no assignment
/// gets `ENOENT` -- how it is told there is no device, not an error path.
fn fill_virtio_mmio(caller: &Proc, msg: &mut Message) -> i32 {
    if virtio_driver_index(caller.nr).is_none() {
        return EPERM;
    }
    let Some(dev) = assignment(caller.nr) else {
        return ENOENT;
    };
    let Some(va) = VIRTIO_MMIO_PAGE_VA.checked_add(dev.in_page_off) else {
        return EINVAL;
    };
    msg.payload[GETINFO_VIRTIO_VA_OFF..GETINFO_VIRTIO_VA_OFF + 8]
        .copy_from_slice(&va.to_ne_bytes());
    msg.payload[GETINFO_VIRTIO_INTID_OFF..GETINFO_VIRTIO_INTID_OFF + 4]
        .copy_from_slice(&dev.intid.to_ne_bytes());
    msg.payload[GETINFO_VIRTIO_SLOT_OFF..GETINFO_VIRTIO_SLOT_OFF + 4]
        .copy_from_slice(&dev.slot.to_ne_bytes());
    OK
}

/// `GET_DMA` reply (slice 6.2, spec T8) -- the driver's pre-mapped DMA run:
///
/// | offset  | type | meaning                     |
/// |---------|------|-----------------------------|
/// |   0..8  | u64  | `uspace::DMA_VA`            |
/// |   8..16 | u64  | physical address of the run |
/// |  16..24 | u64  | length in bytes             |
///
/// Same gate and absent answer as [`fill_virtio_mmio`].
fn fill_dma(caller: &Proc, msg: &mut Message) -> i32 {
    if virtio_driver_index(caller.nr).is_none() {
        return EPERM;
    }
    let Some(dev) = assignment(caller.nr) else {
        return ENOENT;
    };
    let Some(len) = DMA_RUN_PAGES.checked_mul(4096) else {
        return EINVAL;
    };
    msg.payload[GETINFO_DMA_VA_OFF..GETINFO_DMA_VA_OFF + 8].copy_from_slice(&DMA_VA.to_ne_bytes());
    msg.payload[GETINFO_DMA_PA_OFF..GETINFO_DMA_PA_OFF + 8]
        .copy_from_slice(&dev.dma_pa.to_ne_bytes());
    msg.payload[GETINFO_DMA_LEN_OFF..GETINFO_DMA_LEN_OFF + 8].copy_from_slice(&len.to_ne_bytes());
    OK
}
