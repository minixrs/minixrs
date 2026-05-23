//! Process-table entry.
//!
//! Mirrors MINIX 3 `kernel/proc.h`'s `struct proc`, with simplifications:
//! no SMP fields (no `p_cpu_mask`, no `p_stale_tlb`), no profiling counters,
//! no syscall-trace bookkeeping, no live-update bookkeeping. The IPC
//! linked-list links use [`Option<ProcNr>`] indices rather than raw pointers
//! per the convention in `CLAUDE.md`.

use core::sync::atomic::AtomicU32;

use minix4_kernel_shared::endpoint::{Endpoint, NONE};
use minix4_kernel_shared::message::Message;
use minix4_kernel_shared::{PrivId, ProcNr};

use crate::arch::ArchRegisterFrame;

use super::flags::RTS_SLOT_FREE;

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
        next_ready: None,
        name: [0; PROC_NAME_LEN],
    };
}
