// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_VMCTL` — kernel-mediated page-table control.
//!
//! minix.rs keeps every unsafe PTE write and the physical frame allocator in
//! the kernel; the user-space VM server (slice 3.4) drives paging *policy* by
//! issuing the subcalls implemented here. This mirrors MINIX 3's split of
//! mechanism (kernel) from policy (VM) while moving frame ownership kernel-side
//! (MINIX 3's VM owns physical memory directly).
//!
//! Each subcall names a *target* process by endpoint — slice 3.3 only ever
//! sees a process targeting itself (stub D pre-maps then frees its own heap),
//! but the cross-process shape is what 3.4's VM needs when it resolves another
//! process's fault. Resolution mirrors `ipc/send.rs`: `endpoint_proc` →
//! `proc_index`, with the `SELF` sentinel mapped to the caller.
//!
//! ## Trust model
//!
//! There is no *per-target* authorization here: any caller that reaches this
//! handler may name any process (a kernel task slot — negative `ProcNr` —
//! included) and mutate its page tables, fault state, and run-queue status.
//! The single gate is `Priv::k_call_mask` granting `SYS_VMCTL`, checked in
//! `kernel_call_dispatch` before we run. This is the deliberate MINIX 3
//! mechanism/policy split: `SYS_VMCTL` is privileged and the VM server (3.4)
//! is its sole intended holder — VM is trusted to target only processes it
//! legitimately manages, exactly as MINIX 3 trusts its VM. If a future slice
//! hands `SYS_VMCTL` to a less-trusted process, target authorization must be
//! added before that lands.
//!
//! ## Message payload layout (offsets within `Message::payload`)
//!
//! | offset  | field            | direction | subcalls                     |
//! |---------|------------------|-----------|------------------------------|
//! |  0..4   | subcall selector | in        | all                          |
//! |  4..8   | target endpoint  | in        | all (`SELF` = caller)        |
//! |  8..16  | `vaddr` (u64)    | in        | `PT_MAP`, `PT_UNMAP`         |
//! | 16..20  | prot bits (i32)  | in        | `PT_MAP`                     |
//! | 16..24  | `pa` (u64)       | out       | `PT_MAP` (allocated frame)   |
//! |  8..16  | fault addr (u64) | out       | `GET_PAGEFAULT`              |
//! | 16..20  | fault flags(u32) | out       | `GET_PAGEFAULT`              |
//! | 20..28  | fault ip (u64)   | out       | `GET_PAGEFAULT`              |
//!
//! Run-queue transitions (`CLEAR_PAGEFAULT`, `VMINHIBIT_*`) use the
//! `sched::rts_set`/`rts_unset` capture-then-borrow-end pattern that
//! `ipc::send::mini_send` established, so the `&mut Proc` borrow ends before
//! the internal `enqueue`/`dequeue` re-borrows the same slot.

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::callnr::{
    NR_VMCTL_SUBCALLS, VMCTL_CLEAR_PAGEFAULT, VMCTL_GET_PAGEFAULT, VMCTL_PROT_EXEC,
    VMCTL_PROT_WRITE, VMCTL_PT_MAP, VMCTL_PT_UNMAP, VMCTL_VMINHIBIT_CLEAR, VMCTL_VMINHIBIT_SET,
};
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{EINVAL, ENOMEM, OK};
use minixrs_kernel_shared::message::Message;

use crate::arch::aarch64::addrspace::{MapError, Prot, map_page_in, unmap_page_in};
use crate::arch::aarch64::mmu;
use crate::mm::{Frame, alloc_frame, free_frame};
use crate::proc::flags::{RTS_PAGEFAULT, RTS_VMINHIBIT};
use crate::proc::table::N_PROC_SLOTS;
use crate::proc::{PageFaultState, Proc, sched};
use crate::uart::Uart;

/// How many `PT_MAP`/`PT_UNMAP` calls get a detailed head trace before steady
/// state falls back to the `[ksys N]` sampling in `kernel_call_sendrec`.
/// Stub D loops at kernel-call speed, so an unsampled per-call trace would
/// drown everything else (same reasoning as slice 2.6's `TRACE_HEAD`).
const VMCTL_TRACE_HEAD: u64 = 6;
static MAP_COUNT: AtomicU64 = AtomicU64::new(0);
static UNMAP_COUNT: AtomicU64 = AtomicU64::new(0);

/// `SYS_VMCTL` entry point. Dispatches by subcall selector after resolving the
/// target process named in the payload.
pub(super) fn do_vmctl(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    caller_nr: ProcNr,
    msg: &mut Message,
) -> i32 {
    let subcall = read_i32(msg, 0);
    let target_e: Endpoint = read_i32(msg, 4);
    // No per-target authorization — the caller is trusted via `k_call_mask`
    // (see the "Trust model" note above). `target_nr` may be any slot,
    // including a kernel task; `proc_index` only range-checks it.
    let target_idx = match super::resolve_target(proc_table, caller_nr, target_e) {
        Ok(idx) => idx,
        Err(e) => return e,
    };

    // Lock subcall coverage: adding a `VMCTL_*` without a match arm is a
    // compile error.
    const _: () = assert!(
        NR_VMCTL_SUBCALLS == 6,
        "expand do_vmctl when a new VMCTL_* subcall lands",
    );
    match subcall {
        VMCTL_PT_MAP => pt_map(proc_table, target_idx, msg),
        VMCTL_PT_UNMAP => pt_unmap(proc_table, target_idx, msg),
        VMCTL_CLEAR_PAGEFAULT => clear_pagefault(proc_table, target_idx),
        VMCTL_GET_PAGEFAULT => get_pagefault(proc_table, target_idx, msg),
        VMCTL_VMINHIBIT_SET => set_vminhibit(proc_table, target_idx, true),
        VMCTL_VMINHIBIT_CLEAR => set_vminhibit(proc_table, target_idx, false),
        _ => EINVAL,
    }
}

/// `VMCTL_PT_MAP` — allocate a fresh frame and map it into the target's AS.
///
/// The kernel allocates because it owns the frame allocator; the reply carries
/// the chosen PA so VM (3.4) can track the frame for later `PT_UNMAP`.
fn pt_map(proc_table: &mut [Proc; N_PROC_SLOTS], target_idx: usize, msg: &mut Message) -> i32 {
    let vaddr = read_u64(msg, 8);
    let prot_bits = read_i32(msg, 16);
    let prot = Prot {
        writable: prot_bits & VMCTL_PROT_WRITE != 0,
        executable: prot_bits & VMCTL_PROT_EXEC != 0,
    };

    // Snapshot the target's AS coordinates, then drop the borrow before
    // touching the frame allocator / page tables (neither aliases the proc
    // table).
    let (ttbr0_pa, asid, name) = {
        let p = &proc_table[target_idx];
        (p.ttbr0_pa, p.asid, p.name[0])
    };
    if ttbr0_pa == 0 {
        return EINVAL; // target never runs at EL0 (kernel task / unset slot)
    }

    let frame = match alloc_frame() {
        Some(f) => f,
        None => return ENOMEM,
    };
    match map_page_in(ttbr0_pa, vaddr, frame.addr(), prot) {
        Ok(()) => {}
        // Already-mapped or misaligned/out-of-range: hand the frame back and
        // report the error rather than leaking it.
        Err(MapError::AlreadyMapped) | Err(_) => {
            free_frame(frame);
            return EINVAL;
        }
    }
    // SAFETY: ASID-tagged TLBI. The target's TTBR0 may or may not be the live
    // one; ASID-tagged invalidation covers that ASID's entries either way.
    unsafe { mmu::flush_tlb_asid(asid) };

    // Reply: the allocated PA (overwrites the prot-bits input word).
    write_u64(msg, 16, frame.addr());

    let n = MAP_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= VMCTL_TRACE_HEAD {
        let mut uart = Uart::new();
        let _ = writeln!(
            uart,
            "[ksys VMCTL_PT_MAP] proc={} va={vaddr:#x} pa={:#x} result=0",
            name as char,
            frame.addr(),
        );
    }
    OK
}

/// `VMCTL_PT_UNMAP` — clear the PTE at `vaddr` and free its backing frame.
///
/// Assumes the leaf is an anonymous, kernel-allocated frame (everything
/// `PT_MAP` installs is). Mapping a caller-supplied device/shared PA — which
/// must *not* be freed here — is a future subcall.
fn pt_unmap(proc_table: &mut [Proc; N_PROC_SLOTS], target_idx: usize, msg: &mut Message) -> i32 {
    let vaddr = read_u64(msg, 8);
    let (ttbr0_pa, asid, name) = {
        let p = &proc_table[target_idx];
        (p.ttbr0_pa, p.asid, p.name[0])
    };
    if ttbr0_pa == 0 {
        return EINVAL;
    }

    let Some(pa) = unmap_page_in(ttbr0_pa, vaddr) else {
        return EINVAL; // nothing mapped at vaddr
    };
    free_frame(Frame::from_addr(pa));
    // SAFETY: ASID-tagged TLBI; same rationale as `pt_map`.
    unsafe { mmu::flush_tlb_asid(asid) };

    let n = UNMAP_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= VMCTL_TRACE_HEAD {
        let mut uart = Uart::new();
        let _ = writeln!(
            uart,
            "[ksys VMCTL_PT_UNMAP] proc={} va={vaddr:#x} pa={pa:#x} result=0",
            name as char,
        );
    }
    OK
}

/// `VMCTL_CLEAR_PAGEFAULT` — clear the target's recorded fault and unblock it.
fn clear_pagefault(proc_table: &mut [Proc; N_PROC_SLOTS], target_idx: usize) -> i32 {
    let p = &mut proc_table[target_idx];
    p.page_fault_state = PageFaultState::EMPTY;
    // SAFETY: rts_unset captures `nr` then ends the `p` borrow before enqueue;
    // single-threaded EL1 context (same invariant as `do_page_fault`).
    unsafe { sched::rts_unset(p, RTS_PAGEFAULT) };
    OK
}

/// `VMCTL_GET_PAGEFAULT` — return the target's recorded fault state.
///
/// Only meaningful while the target is blocked on a page fault; reading it
/// otherwise would hand VM stale coordinates, so reject that case.
fn get_pagefault(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    target_idx: usize,
    msg: &mut Message,
) -> i32 {
    let p = &proc_table[target_idx];
    if p.rts_flags.load(Ordering::Relaxed) & RTS_PAGEFAULT == 0 {
        return EINVAL;
    }
    let pf = p.page_fault_state;
    write_u64(msg, 8, pf.addr);
    write_i32(msg, 16, pf.flags as i32);
    write_u64(msg, 20, pf.ip);
    OK
}

/// `VMCTL_VMINHIBIT_SET`/`_CLEAR` — gate scheduling of the target while VM
/// mutates its address space.
fn set_vminhibit(proc_table: &mut [Proc; N_PROC_SLOTS], target_idx: usize, set: bool) -> i32 {
    let p = &mut proc_table[target_idx];
    // SAFETY: rts_set/rts_unset capture `nr` then end the `p` borrow before
    // dequeue/enqueue; single-threaded EL1 context.
    unsafe {
        if set {
            sched::rts_set(p, RTS_VMINHIBIT);
        } else {
            sched::rts_unset(p, RTS_VMINHIBIT);
        }
    }
    OK
}

// ----- payload accessors ----------------------------------------------------

#[inline]
fn read_i32(msg: &Message, off: usize) -> i32 {
    i32::from_ne_bytes(
        msg.payload[off..off + 4]
            .try_into()
            .expect("payload in range"),
    )
}

#[inline]
fn read_u64(msg: &Message, off: usize) -> u64 {
    u64::from_ne_bytes(
        msg.payload[off..off + 8]
            .try_into()
            .expect("payload in range"),
    )
}

#[inline]
fn write_i32(msg: &mut Message, off: usize, v: i32) {
    msg.payload[off..off + 4].copy_from_slice(&v.to_ne_bytes());
}

#[inline]
fn write_u64(msg: &mut Message, off: usize, v: u64) {
    msg.payload[off..off + 8].copy_from_slice(&v.to_ne_bytes());
}
