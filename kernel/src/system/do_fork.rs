// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_FORK` — clone a blocked parent into a free process slot.
//!
//! PM owns the process tree; the kernel's half of fork is mechanical
//! (MINIX 3 `kernel/system/do_fork.c`): validate, copy the parent's saved
//! state into the child slot PM picked, give the child its own address space
//! (an eager page-by-page copy — CoW is deferred), and hand back the child's
//! endpoint. PM then drives `VM_FORK`, `SCHEDULING_START`, and the
//! `SYS_PRIVCTL(PRIVCTL_SET_USER)` release before replying to both halves.
//!
//! The parent must be RECEIVE-blocked with no pending delivery ("fork is done
//! synchronously", MINIX parity): in the live flow it is mid-`SENDREC` to PM,
//! so its full register frame is parked in `Proc::regs` with `x0` already
//! holding the SENDREC's `OK`. The child copies that frame *verbatim* — both
//! processes resume after the same `svc` instruction and read their own reply
//! buffer; PM's reply `m_type` (child pid vs 0) is the only discriminator,
//! not a patched return register.
//!
//! The child is created **frozen**: `RTS_RECEIVING` (a faithful mid-SENDREC
//! receiver, so PM's reply delivers normally) plus `RTS_NO_PRIV` (the
//! stub-E freeze gate — `SYS_PRIVCTL` releases it once VM and SCHED know
//! about the child). It is not enqueued; `rts_unset` admits it when the last
//! block bit clears. `priv_id` is inherited from the parent — the shared
//! `USER_PRIV_ID` slot (MINIX's `static_priv` model), which is what lets PM's
//! reply pass `mini_send`'s `ipc_to` check before the release.
//!
//! The child consumes the slot's **stored** endpoint rather than minting one:
//! `SYS_EXIT` bumped the generation when it freed the slot, so a recycled
//! slot's new occupant is unreachable through any stale endpoint.
//!
//! Target-taking (routed beside `SYS_EXIT` in `kernel_call_dispatch`); trust
//! model identical to `do_vmctl` — the `k_call_mask` gate is the only check,
//! and PM is the intended sole holder.
//!
//! ## Message payload layout (offsets within `Message::payload`)
//!
//! | offset | field                  | direction |
//! |--------|------------------------|-----------|
//! |  0..4  | parent endpoint (i32)  | in        |
//! |  4..8  | child proc nr (i32)    | in        |
//! |  0..4  | child endpoint (i32)   | out       |

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::com::NR_PROCS;
use minixrs_kernel_shared::endpoint::NONE;
use minixrs_kernel_shared::error::{EINVAL, ENOMEM, OK};
use minixrs_kernel_shared::message::Message;

use crate::arch::aarch64::addrspace::{AddrSpace, MapError, walk_leaves};
use crate::arch::aarch64::asid;
use crate::arch::aarch64::mmu::flush_icache_range;
use crate::mm::{FRAME_SIZE, Frame, alloc_frame, free_frame, phys_to_hhdm};
use crate::proc::flags::{
    MF_DELIVERMSG, MF_REPLY_PEND, RTS_NO_PRIV, RTS_RECEIVING, RTS_SENDING, RTS_SLOT_FREE,
};
use crate::proc::table::{N_PROC_SLOTS, proc_index};
use crate::proc::{PageFaultState, Proc};
use crate::uart::Uart;

/// Leading `SYS_FORK` calls traced explicitly, plus an every-100th steady
/// sample — same cadence as `do_exit`'s trace, its lifecycle twin.
const FORK_TRACE_HEAD: u64 = 6;
const FORK_TRACE_EVERY: u64 = 100;
static FORK_COUNT: AtomicU64 = AtomicU64::new(0);

/// `SYS_FORK` — clone `parent` into the free slot `child_nr`; reply the
/// child's endpoint.
pub(super) fn do_fork(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    caller_nr: ProcNr,
    msg: &mut Message,
) -> i32 {
    let parent_e = read_i32(msg, 0);
    let child_nr_raw = read_i32(msg, 4);

    let parent_idx = match super::resolve_target(proc_table, caller_nr, parent_e) {
        Ok(idx) => idx,
        Err(e) => return e,
    };

    // The child slot is named by proc nr, not endpoint — it is free, so it
    // has no live endpoint to validate. Task slots (negative) are never
    // forkable.
    if !(0..NR_PROCS as i32).contains(&child_nr_raw) {
        return EINVAL;
    }
    let child_nr = ProcNr::new(child_nr_raw);
    let child_idx = proc_index(child_nr).expect("user proc nr in range");
    if proc_table[child_idx].rts_flags.load(Ordering::Relaxed) & RTS_SLOT_FREE == 0 {
        return EINVAL;
    }

    // Parent gate: RECEIVE-blocked (mid-SENDREC to PM in the live flow) with
    // its frame fully parked and no delivery half-done. MINIX 3 do_fork.c:
    // "make sure the parent is blocked waiting for the reply".
    let parent = &proc_table[parent_idx];
    let parent_rts = parent.rts_flags.load(Ordering::Relaxed);
    if parent_rts & RTS_RECEIVING == 0
        || parent_rts & RTS_SENDING != 0
        || parent.misc_flags & MF_DELIVERMSG != 0
    {
        return EINVAL;
    }
    if parent.ttbr0_pa == 0 {
        // A proc with no address space (kernel task, never-loaded server)
        // has nothing to clone.
        return EINVAL;
    }

    // Build the child's address space *before* touching the child slot, so a
    // mid-copy allocation failure needs no proc-table rollback.
    let child_as = match copy_addrspace(parent.ttbr0_pa) {
        Ok((ttbr0_pa, pages)) => (ttbr0_pa, pages),
        Err(_) => return ENOMEM,
    };
    let (child_ttbr0, pages) = child_as;

    // SAFETY: single-threaded EL1 context; sole accessor of the ASID pool.
    let child_asid = unsafe { asid::alloc_asid() };

    // Snapshot the parent fields the child inherits (ends the shared borrow
    // before the child slot is borrowed mutably).
    let p = &proc_table[parent_idx];
    let parent_regs = p.regs;
    let parent_priority = p.priority;
    let parent_quantum_ms = p.quantum_ms;
    let parent_getfrom_e = p.getfrom_e;
    let parent_deliver_msg_vir = p.deliver_msg_vir;
    let parent_heap_window = p.heap_window;
    let parent_priv_id = p.priv_id;
    let parent_name = p.name;
    let parent_reply_pend = p.misc_flags & MF_REPLY_PEND;

    let child_e = {
        let c = &mut proc_table[child_idx];
        // Inherited: the parent's exact resume state and identity-adjacent
        // fields.
        c.regs = parent_regs;
        c.priv_id = parent_priv_id;
        c.misc_flags = parent_reply_pend;
        c.priority = parent_priority;
        c.quantum_ms = parent_quantum_ms;
        c.quantum_left = parent_quantum_ms as u64;
        c.getfrom_e = parent_getfrom_e;
        c.deliver_msg_vir = parent_deliver_msg_vir;
        c.heap_window = parent_heap_window;
        c.name = fork_name(parent_name);
        // Fresh: everything that must not leak across the fork (or across the
        // slot's previous occupant).
        c.user_time = 0;
        c.sys_time = 0;
        c.alarm_at = 0;
        c.sig_pending = 0;
        c.caller_q = None;
        c.q_link = None;
        c.sendto_e = NONE;
        c.send_msg = Message {
            m_source: 0,
            m_type: 0,
            payload: [0; 96],
        };
        c.deliver_msg = Message {
            m_source: 0,
            m_type: 0,
            payload: [0; 96],
        };
        c.ttbr0_pa = child_ttbr0;
        c.asid = child_asid;
        c.page_fault_state = PageFaultState::EMPTY;
        c.scheduler = NONE; // SCHED claims the child at SCHEDULING_START
        c.next_ready = None;
        // Blocked exactly like the parent (RECEIVE, waiting on PM's reply)
        // plus the freeze gate PM lifts via SYS_PRIVCTL once VM and SCHED
        // know about the child. Not enqueued — `rts_unset` admits it when
        // the last bit clears.
        c.rts_flags
            .store(RTS_RECEIVING | RTS_NO_PRIV, Ordering::Relaxed);
        c.endpoint // the slot's stored, generation-bumped endpoint
    };

    write_i32(msg, 0, child_e);

    let n = FORK_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= FORK_TRACE_HEAD || n.is_multiple_of(FORK_TRACE_EVERY) {
        let _ = writeln!(
            Uart::new(),
            "[ksys SYS_FORK] parent={} nr={} child_nr={child_nr_raw} child_e={child_e:#x} pages={pages}",
            parent_name[0] as char,
            proc_table[parent_idx].nr.get(),
        );
    }
    OK
}

/// Eagerly clone every mapped page of the tree rooted at `parent_ttbr0` into
/// a fresh address space: allocate a frame per leaf, copy the 4 KiB through
/// HHDM, and map it at the same VA with the parent's permissions (I-cache
/// synced for executable pages — TCG hides a missing flush, real hardware
/// does not). Returns `(child_ttbr0_pa, pages_copied)`.
///
/// On failure the partial child is unwound — its leaves freed, its tree
/// destroyed — so an out-of-memory fork leaks nothing.
fn copy_addrspace(parent_ttbr0: u64) -> Result<(u64, u64), MapError> {
    let mut child = AddrSpace::new()?;
    let mut pages: u64 = 0;

    let result = walk_leaves(parent_ttbr0, &mut |va, pa, prot| {
        let frame = alloc_frame().ok_or(MapError::OutOfMemory)?;
        let new_pa = frame.addr();
        // SAFETY: both PAs came from the frame allocator, so both are
        // HHDM-mapped, 4 KiB-aligned, and distinct (copy_nonoverlapping's
        // contract). Single-threaded EL1 context.
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_hhdm(pa) as *const u8,
                phys_to_hhdm(new_pa),
                FRAME_SIZE,
            );
        }
        if prot.executable {
            // SAFETY: the HHDM alias of `new_pa` is a valid EL1-readable
            // mapping of the just-written range (same pattern as
            // `userland::build_stub`'s blob install).
            unsafe { flush_icache_range(phys_to_hhdm(new_pa) as u64, FRAME_SIZE) };
        }
        if let Err(e) = child.map_page(va, new_pa, prot) {
            // `map_page` can OOM allocating an intermediate table *after* the
            // leaf frame is already live but before it is linked into the
            // tree. The `Err` unwind below sweeps only *mapped* leaves, so
            // free this orphan here — otherwise a fork that runs out of
            // memory mid-copy would leak exactly this frame.
            free_frame(frame);
            return Err(e);
        }
        pages += 1;
        Ok(())
    });

    match result {
        Ok(()) => Ok((child.ttbr0_pa, pages)),
        Err(e) => {
            // Unwind: free the pages the partial copy installed, then the
            // tree. The child never had an ASID, so there is nothing to
            // flush or recycle.
            let _ = walk_leaves(child.ttbr0_pa, &mut |_va, pa, _prot| {
                free_frame(Frame::from_addr(pa));
                Ok(())
            });
            child.destroy();
            Err(e)
        }
    }
}

/// The child's name: the parent's with `*F` appended when there is room
/// (MINIX 3 do_fork.c parity — `name[0]` keeps the parent's letter so traces
/// stay readable).
fn fork_name(
    mut name: [u8; crate::proc::proc_struct::PROC_NAME_LEN],
) -> [u8; crate::proc::proc_struct::PROC_NAME_LEN] {
    let len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    if len + 2 < name.len() {
        name[len] = b'*';
        name[len + 1] = b'F';
    }
    name
}

#[inline]
fn read_i32(msg: &Message, off: usize) -> i32 {
    i32::from_ne_bytes(
        msg.payload[off..off + 4]
            .try_into()
            .expect("payload in range"),
    )
}

#[inline]
fn write_i32(msg: &mut Message, off: usize, v: i32) {
    msg.payload[off..off + 4].copy_from_slice(&v.to_ne_bytes());
}
