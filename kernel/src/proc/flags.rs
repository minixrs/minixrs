// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
// Many of these constants are defined now so that the slice-2.5 IPC code and
// slice-2.6 kernel-call dispatch can land without churning this module
// again. They become live as later slices consume them.
#![allow(dead_code)]

//! Kernel-internal flag bits for [`Proc`] and [`Priv`].
//!
//! [`Proc`]: super::Proc
//! [`Priv`]: super::Priv
//!
//! Values mirror MINIX 3 `kernel/proc.h` (RTS / MF) and `include/minix/const.h`
//! (PREEMPTIBLE / BILLABLE / …) so the bit semantics are recognizable. None
//! of these constants belong on the IPC wire — userspace never sees them —
//! so they live in the kernel crate rather than in `kernel-shared`.
//!
//! minix.rs drops a few MINIX 3 flags that are not relevant to the port:
//! `LU_SYS_PROC` (no live update), `MF_FLUSH_TLB` / `MF_SPROF_SEEN` (no SMP,
//! no profiling), `MF_SC_*` (no syscall tracing).

use minixrs_kernel_shared::ipc_const::{RECEIVE, SENDREC};

// ---------------------------------------------------------------------------
// Run-time state flags (`Proc::rts_flags`).
//
// A process is runnable iff `rts_flags == 0`. Each bit identifies a distinct
// reason the kernel is keeping the process off the run queue; clearing all
// reasons makes the process runnable again.
// ---------------------------------------------------------------------------

/// Process-table slot is free.
pub const RTS_SLOT_FREE: u32 = 0x0_0001;
/// Process has been explicitly stopped.
pub const RTS_PROC_STOP: u32 = 0x0_0002;
/// Blocked trying to `SEND`.
pub const RTS_SENDING: u32 = 0x0_0004;
/// Blocked trying to `RECEIVE`.
pub const RTS_RECEIVING: u32 = 0x0_0008;
/// A kernel signal has arrived for this process.
pub const RTS_SIGNALED: u32 = 0x0_0010;
/// Signal handling is in progress.
pub const RTS_SIG_PENDING: u32 = 0x0_0020;
/// Process is being traced (debugger / ptrace).
pub const RTS_P_STOP: u32 = 0x0_0040;
/// Forked system process awaiting privilege assignment.
pub const RTS_NO_PRIV: u32 = 0x0_0080;
/// Process cannot send or receive — no valid endpoint.
pub const RTS_NO_ENDPOINT: u32 = 0x0_0100;
/// Awaiting VM to set up page tables.
pub const RTS_VMINHIBIT: u32 = 0x0_0200;
/// Unhandled page fault pending.
pub const RTS_PAGEFAULT: u32 = 0x0_0400;
/// Originator of a VM memory request, waiting on resolution.
pub const RTS_VMREQUEST: u32 = 0x0_0800;
/// Target of a VM memory request, helping the originator.
pub const RTS_VMREQTARGET: u32 = 0x0_1000;
/// Preempted by a higher-priority process — re-enqueue at the front.
pub const RTS_PREEMPTED: u32 = 0x0_4000;
/// Quantum exhausted — re-enqueue at the back of the run queue.
pub const RTS_NO_QUANTUM: u32 = 0x0_8000;
/// Awaiting VM to finish boot-time setup.
pub const RTS_BOOTINHIBIT: u32 = 0x1_0000;

// ---------------------------------------------------------------------------
// Miscellaneous flags (`Proc::misc_flags`).
//
// These do NOT block scheduling; they are status bits the kernel consults
// during IPC, signal delivery, and FPU handling.
// ---------------------------------------------------------------------------

pub const MF_REPLY_PEND: u32 = 0x0_0001;
pub const MF_VIRT_TIMER: u32 = 0x0_0002;
pub const MF_PROF_TIMER: u32 = 0x0_0004;
pub const MF_KCALL_RESUME: u32 = 0x0_0008;
pub const MF_DELIVERMSG: u32 = 0x0_0040;
pub const MF_SIG_DELAY: u32 = 0x0_0080;
pub const MF_FPU_INITIALIZED: u32 = 0x0_1000;
pub const MF_SENDING_FROM_KERNEL: u32 = 0x0_2000;
pub const MF_CONTEXT_SET: u32 = 0x0_4000;
pub const MF_SENDA_VM_MISS: u32 = 0x2_0000;
pub const MF_STEP: u32 = 0x4_0000;
pub const MF_MSGFAILED: u32 = 0x8_0000;
pub const MF_NICED: u32 = 0x10_0000;

// ---------------------------------------------------------------------------
// Privilege flags (`Priv::flags`). 16-bit to match `Priv`'s field width.
// ---------------------------------------------------------------------------

/// Process is preemptible (kernel tasks are not).
pub const PREEMPTIBLE: u16 = 0x002;
/// CPU time is charged to this process.
pub const BILLABLE: u16 = 0x004;
/// Privilege ID was assigned dynamically (not from the boot image).
pub const DYN_PRIV_ID: u16 = 0x008;
/// System process — owns its own [`Priv`] slot.
///
/// [`Priv`]: super::Priv
pub const SYS_PROC: u16 = 0x010;
/// Privilege subsystem checks I/O port access requests against `Priv::io_ranges`.
pub const CHECK_IO_PORT: u16 = 0x020;
/// Privilege subsystem checks IRQ assignments against `Priv::irqs`.
pub const CHECK_IRQ: u16 = 0x040;
/// Privilege subsystem checks VM memory-range requests against `Priv::mem_ranges`.
pub const CHECK_MEM: u16 = 0x080;
/// Root system process (the reincarnation server, RS).
pub const ROOT_SYS_PROC: u16 = 0x100;
/// VM system process — gets the dedicated VM privilege role.
pub const VM_SYS_PROC: u16 = 0x200;
/// Restarted system process (set on respawn).
pub const RST_SYS_PROC: u16 = 0x800;

// ---------------------------------------------------------------------------
// Trap masks (`Priv::trap_mask`). Bit `i` allows IPC primitive `i`.
// ---------------------------------------------------------------------------

/// Kernel-task trap mask — no IPC traps allowed (HARDWARE, IDLE, ASYNCM).
pub const TSK_T: u16 = 0;
/// "Constrained" kernel-task trap mask — only `RECEIVE` allowed (CLOCK, SYSTEM).
pub const CSK_T: u16 = 1 << RECEIVE;
/// User-process trap mask — only `SENDREC` allowed.
pub const USR_T: u16 = 1 << SENDREC;
/// System-server trap mask — all IPC primitives allowed.
pub const SRV_T: u16 = !0;
