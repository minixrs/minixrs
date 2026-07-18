// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Process-table entry.
//!
//! Mirrors MINIX 3 `kernel/proc.h`'s `struct proc`, with simplifications:
//! no SMP fields (no `p_cpu_mask`, no `p_stale_tlb`), no profiling counters,
//! no syscall-trace bookkeeping, no live-update bookkeeping. The IPC
//! linked-list links use [`Option<ProcNr>`] indices rather than raw pointers
//! per the convention in `CLAUDE.md`.

use core::sync::atomic::AtomicU32;

use minixrs_kernel_shared::endpoint::{Endpoint, NONE};
use minixrs_kernel_shared::message::Message;
use minixrs_kernel_shared::{PrivId, ProcNr};

use crate::arch::ArchRegisterFrame;

use super::flags::RTS_SLOT_FREE;
use super::page_fault::{HeapWindow, PageFaultState};

/// Maximum length of a process name, including the trailing NUL byte if any.
pub const PROC_NAME_LEN: usize = 16;

/// One slot in the kernel's process table.
#[repr(C)]
pub struct Proc {
    /// Saved register state — populated on EL0 → EL1 trap (slice 2.3+).
    pub regs: ArchRegisterFrame,
    /// Index of this slot in the process table.
    pub nr: ProcNr,
    /// Endpoint (generation | proc-nr) — set when the slot is allocated.
    pub endpoint: Endpoint,
    /// Privilege-table slot, or `None` if the process has no privileges yet.
    pub priv_id: Option<PrivId>,
    /// Run-time state bits. Process is runnable iff zero.
    pub rts_flags: AtomicU32,
    /// Miscellaneous status bits (do not gate scheduling).
    pub misc_flags: u32,
    /// Current scheduling priority (lower = higher priority).
    pub priority: u8,
    /// Assigned quantum size (milliseconds).
    pub quantum_ms: u32,
    /// Quantum remaining (kernel ticks); slice 2.4 wires this up.
    pub quantum_left: u64,
    /// User-mode time consumed (ticks).
    pub user_time: u64,
    /// Kernel-mode time consumed (ticks).
    pub sys_time: u64,

    // ----- Timer / alarm state --------------------------------------------
    /// Absolute uptime tick at which this proc's one-shot `SYS_SETALARM` timer
    /// fires, or 0 if disarmed. On expiry the clock tick delivers a `NOTIFY`
    /// from `CLOCK` to this proc and clears the field (slice 4.4). Mirrors the
    /// per-process kernel-call alarm MINIX 3 hangs off `kernel/clock.c`'s
    /// `clock_timers`, simplified to a single absolute deadline per proc.
    pub alarm_at: u64,

    // ----- Signal state ----------------------------------------------------
    /// Pending kernel-signal bitmap (bit n = signal n, `1..NSIG`). Set by
    /// `system::do_sig::cause_sig`; handed off (cleared) by `SYS_GETKSIG`,
    /// with the matching `RTS_SIGNALED`/`RTS_SIG_PENDING` state cleared at
    /// `SYS_ENDKSIG` (slice 4.5). Zeroed on slot free (`SYS_EXIT`'s
    /// `free_slot`, alongside the generation bump) and again on `SYS_FORK`'s
    /// child populate, so recycled slots never inherit a predecessor's
    /// pending signals.
    pub sig_pending: u32,

    // ----- IPC state -------------------------------------------------------
    /// Head of the queue of processes wanting to send to us.
    pub caller_q: Option<ProcNr>,
    /// Next process in the receiver's caller queue.
    pub q_link: Option<ProcNr>,
    /// Endpoint we're trying to RECEIVE from (or `ANY`).
    pub getfrom_e: Endpoint,
    /// Endpoint we're trying to SEND to.
    pub sendto_e: Endpoint,
    /// Buffered outgoing message (used while blocked on SEND).
    pub send_msg: Message,
    /// Message to deliver when we unblock (used with `MF_DELIVERMSG`).
    pub deliver_msg: Message,
    /// User-space VA where the next delivered IPC message should land.
    /// Set when RECEIVE / SENDREC blocks (or its caller arrives mid-flight);
    /// consumed by `flush_deliver_msg` on every EL1 → EL0 transition.
    pub deliver_msg_vir: u64,

    // ----- MMU state ------------------------------------------------------
    /// PA of this proc's L0 page-table root, or 0 if the proc never runs at
    /// EL0 (kernel tasks, unprivileged boot servers prior to VM setup).
    /// Set by `arch::aarch64::userland::userland_bootstrap` for the EL0
    /// stubs; `proc::sched::schedule_next` reads it on every context switch.
    pub ttbr0_pa: u64,
    /// 8-bit ARMv8 ASID for this address space. Goes into TTBR0_EL1[55:48]
    /// on switch (TCR_EL1.AS = 0, the Limine default). 0 = uninitialized
    /// (boot procs, kernel tasks); real values start at 1 and are handed
    /// out by `arch::aarch64::asid::alloc_asid` in slice-3.1b boot order
    /// (A=1, B=2, C=3).
    pub asid: u8,

    // ----- VM / page-fault state ------------------------------------------
    /// Details of the fault this proc is blocked on. Only meaningful while
    /// `RTS_PAGEFAULT` is set; reset to [`PageFaultState::EMPTY`] once the
    /// fault is resolved. Read back by slice 3.3's `VMCTL_GET_PAGEFAULT`.
    pub page_fault_state: PageFaultState,
    /// Virtual-address range the kernel resolves on-demand in slice 3.2
    /// (the kernel-as-VM stand-in). Empty for procs with no heap (kernel
    /// tasks, the slice 2.5/2.6 stubs A/B/C). Slice 3.4 moves this into
    /// the VM server's region table.
    pub heap_window: HeapWindow,

    // ----- Scheduling delegation ------------------------------------------
    /// Endpoint of this proc's user-space scheduler, or [`NONE`] for the
    /// kernel-scheduled default (slice 4.3). When `NONE`, `sched::reschedule`
    /// refills the quantum and rotates the proc as it always has; otherwise the
    /// kernel sends `SCHEDULING_NO_QUANTUM` to this endpoint on quantum
    /// exhaustion and leaves the proc off the run queue until the scheduler
    /// re-admits it via `SYS_SCHEDULE`. Mirrors MINIX 3's `p_scheduler`.
    /// Kernel tasks and SCHED itself stay `NONE` (a scheduler must not schedule
    /// itself).
    pub scheduler: Endpoint,

    // ----- Run-queue state -------------------------------------------------
    /// Next process in the same priority-band run queue, or `None` if last.
    /// Mirrors MINIX 3's `p_nextready` but as a [`ProcNr`] index per the
    /// no-raw-pointers convention in `CLAUDE.md`.
    pub next_ready: Option<ProcNr>,

    /// ASCII process name, NUL-padded; first 0 byte terminates.
    pub name: [u8; PROC_NAME_LEN],
}

impl Proc {
    /// Empty-slot initializer used to fill the static process table at boot.
    ///
    /// The slot is marked `RTS_SLOT_FREE` so the scheduler can recognize it
    /// as unallocated until [`super::init`] populates the boot-image slots.
    pub const EMPTY: Self = Self {
        regs: ArchRegisterFrame::EMPTY,
        nr: ProcNr::new(0),
        endpoint: NONE,
        priv_id: None,
        rts_flags: AtomicU32::new(RTS_SLOT_FREE),
        misc_flags: 0,
        priority: 0,
        quantum_ms: 0,
        quantum_left: 0,
        user_time: 0,
        sys_time: 0,
        alarm_at: 0,
        sig_pending: 0,
        caller_q: None,
        q_link: None,
        getfrom_e: NONE,
        sendto_e: NONE,
        send_msg: Message {
            m_source: 0,
            m_type: 0,
            payload: [0; 96],
        },
        deliver_msg: Message {
            m_source: 0,
            m_type: 0,
            payload: [0; 96],
        },
        deliver_msg_vir: 0,
        ttbr0_pa: 0,
        asid: 0,
        page_fault_state: PageFaultState::EMPTY,
        heap_window: HeapWindow::EMPTY,
        scheduler: NONE,
        next_ready: None,
        name: [0; PROC_NAME_LEN],
    };
}
