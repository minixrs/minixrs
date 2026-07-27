// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_GETINFO` — kernel-state introspection.
//!
//! The request sub-type lives in the first 4 bytes of the message payload
//! (mirrors MINIX 3 `mess_lsys_krn_sys_getinfo.request`). Two sub-types are
//! implemented: `GET_WHOAMI` (Phase 2) and `GET_RAMDISK` (slice 5.7). Every other
//! sub-type returns `EINVAL` so a caller that sends an unsupported request gets a
//! recognizable error rather than a silently-zeroed reply.

use minixrs_kernel_shared::callnr::{
    GET_RAMDISK, GET_WHOAMI, GETINFO_RAMDISK_LEN_OFF, GETINFO_RAMDISK_VA_OFF, SYS_GETINFO_NAME_LEN,
};
use minixrs_kernel_shared::com::{MEM_PROC_NR, ROOTFS_MODULE_NAME};
use minixrs_kernel_shared::error::{EINVAL, ENOENT, EPERM, OK};
use minixrs_kernel_shared::message::Message;
use minixrs_kernel_shared::uspace::RAMDISK_VA;

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
