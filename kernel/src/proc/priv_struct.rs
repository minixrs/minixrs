// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
// IoRange/MemRange fields and several constants are forward declarations for
// slice 2.6 (kernel-call dispatch) — they are written by `proc::init` and read
// once slices 2.5/2.6 wire up IPC and `SYS_*` handlers.
#![allow(dead_code)]

//! Privilege-table entry.
//!
//! Mirrors MINIX 3 `kernel/priv.h`'s `struct priv` with the same simplifications
//! as [`Proc`]: no SMP, no live update, no profiling. The bitmaps are sized to
//! `NR_SYS_PROCS` (for IPC / notify / asyn target maps) and `NR_SYS_CALLS`
//! (for the kernel-call mask).
//!
//! [`Proc`]: super::Proc

use arrayvec::ArrayVec;

use minixrs_kernel_shared::callnr::NR_SYS_CALLS;
use minixrs_kernel_shared::com::NR_SYS_PROCS;
use minixrs_kernel_shared::endpoint::{Endpoint, NONE};
use minixrs_kernel_shared::sys_limits::{NR_IO_RANGE, NR_IRQ, NR_MEM_RANGE};
use minixrs_kernel_shared::{PrivId, ProcNr};

/// Number of `u32` chunks needed to cover `NR_SYS_PROCS` bits.
pub const IPC_MAP_CHUNKS: usize = NR_SYS_PROCS / 32;
/// Number of `u32` chunks needed to cover `NR_SYS_CALLS` bits.
pub const K_CALL_MASK_CHUNKS: usize = NR_SYS_CALLS / 32;

const _: () = assert!(NR_SYS_PROCS % 32 == 0);

/// I/O port range a privileged process may access.
#[derive(Copy, Clone, Debug, Default)]
pub struct IoRange {
    pub base: u64,
    pub count: u64,
}

/// Physical memory range a privileged process may map.
#[derive(Copy, Clone, Debug, Default)]
pub struct MemRange {
    pub base: u64,
    pub count: u64,
}

/// One slot in the kernel's privilege table.
///
/// Each system (privileged) process owns exactly one `Priv`; user processes
/// share a single user-class slot (`USER_PRIV_ID`).
#[repr(C)]
pub struct Priv {
    /// Slot index into the privilege table.
    pub id: PrivId,
    /// Process this slot belongs to, if any.
    pub proc_nr: Option<ProcNr>,
    /// Privilege flags (`PREEMPTIBLE`, `SYS_PROC`, …).
    pub flags: u16,
    /// Trap mask — bit `i` allows IPC primitive `i`.
    pub trap_mask: u16,

    // ----- IPC target / pending bitmaps ------------------------------------
    /// Bitmap of privileged slots this process may send to.
    pub ipc_to: [u32; IPC_MAP_CHUNKS],
    /// Bitmap of allowed kernel calls.
    pub k_call_mask: [u32; K_CALL_MASK_CHUNKS],
    /// Bitmap of pending notifications.
    pub notify_pending: [u32; IPC_MAP_CHUNKS],
    /// Bitmap of pending async messages.
    pub asyn_pending: [u32; IPC_MAP_CHUNKS],

    /// Signal manager endpoint (delivers signals raised against us).
    pub sig_mgr: Endpoint,

    // ----- Resource ownership ---------------------------------------------
    pub io_ranges: ArrayVec<IoRange, NR_IO_RANGE>,
    pub mem_ranges: ArrayVec<MemRange, NR_MEM_RANGE>,
    pub irqs: ArrayVec<u32, NR_IRQ>,

    /// Address of the grant table within the owner's address space (set by
    /// `SYS_SETGRANT`); slice 2.6 starts honoring this.
    pub grant_table: u64,
    /// Number of grant-table entries.
    pub grant_entries: u32,
}

impl Priv {
    /// Empty-slot initializer used to fill the static privilege table at boot.
    pub const EMPTY: Self = Self {
        id: PrivId::new(0),
        proc_nr: None,
        flags: 0,
        trap_mask: 0,
        ipc_to: [0; IPC_MAP_CHUNKS],
        k_call_mask: [0; K_CALL_MASK_CHUNKS],
        notify_pending: [0; IPC_MAP_CHUNKS],
        asyn_pending: [0; IPC_MAP_CHUNKS],
        sig_mgr: NONE,
        io_ranges: ArrayVec::new_const(),
        mem_ranges: ArrayVec::new_const(),
        irqs: ArrayVec::new_const(),
        grant_table: 0,
        grant_entries: 0,
    };
}
