// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_EXEC` — replace a target process's program image in place.
//!
//! PM owns exec (MINIX 3 `pm/exec.c` → `sys_exec`): a user proc `SENDREC`s
//! `PM_EXEC` to PM, PM selects a boot-embedded binary and issues `SYS_EXEC`
//! naming that proc as the target. The kernel's half is mechanical:
//!
//! - resolve the binary by name in the MXBI archive
//!   ([`BootImage::module_by_name`]);
//! - build a brand-new address space and load the ELF into it
//!   ([`userland::load_exec_image`] — the same helper that boots the servers);
//! - reset the target's register frame to the new entry point with a fresh
//!   stack (exec starts clean — no register carries over);
//! - swap in the new `(ttbr0_pa, asid)` and tear down the *old* address space
//!   (reusing [`do_exit`]'s teardown sequence);
//! - unblock the target so the scheduler resumes it at `_start`.
//!
//! The target is always a clean blocked receiver: in the live flow it is
//! mid-`SENDREC` to PM (`RTS_RECEIVING`), so its whole frame is parked. Exec
//! discards that continuation — the proc does not return from the `PM_EXEC`
//! call; it restarts at the new image's entry. PM therefore sends **no** reply
//! on success (this handler resumes the target); on failure the target is left
//! untouched on its old image and PM returns the errno to it.
//!
//! `SELF` and the caller's own endpoint are rejected — tearing down the active
//! TTBR0 mid-kernel-call would pull it out from under the running caller (the
//! `do_exit` stance). PM is the sole intended holder; it always names a third
//! party. exec preserves the proc's pid, privilege, and scheduler — only the
//! address space and register frame change.
//!
//! Target-taking (routed beside `SYS_FORK` in `kernel_call_dispatch`); trust
//! model identical to `do_fork` — the `k_call_mask` gate is the only check.
//!
//! ## Message payload layout (offsets within `Message::payload`)
//!
//! | offset             | field                            | direction |
//! |--------------------|----------------------------------|-----------|
//! |  0..4              | target endpoint (i32)            | in        |
//! |  4..4+EXEC_NAME_LEN| binary name (NUL-padded)         | in        |

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::callnr::EXEC_NAME_LEN;
use minixrs_kernel_shared::endpoint::{NONE, SELF};
use minixrs_kernel_shared::error::{EINVAL, ENOENT, ENOMEM, OK};
use minixrs_kernel_shared::message::Message;

use crate::arch::aarch64::context::ArchRegisterFrame;
use crate::arch::aarch64::userland;
use crate::proc::flags::{MF_DELIVERMSG, RTS_RECEIVING, RTS_SENDING};
use crate::proc::proc_struct::PROC_NAME_LEN;
use crate::proc::table::{N_PROC_SLOTS, proc_index};
use crate::proc::{Proc, sched};
use crate::uart::Uart;

// The exec name field fits in a proc-name slot, so a successful exec can rename
// the proc to its new program (MINIX parity; sharpens the traces).
const _: () = assert!(EXEC_NAME_LEN <= PROC_NAME_LEN);

/// Leading `SYS_EXEC` calls traced explicitly, plus an every-100th steady
/// sample — same cadence as `do_fork`/`do_exit`, its lifecycle siblings.
const EXEC_TRACE_HEAD: u64 = 6;
const EXEC_TRACE_EVERY: u64 = 100;
static EXEC_COUNT: AtomicU64 = AtomicU64::new(0);

/// `SYS_EXEC` — load the named boot-embedded binary into a fresh address space
/// for the target, discard its old image, and resume it at the new entry point.
pub(super) fn do_exec(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    caller_nr: ProcNr,
    msg: &mut Message,
) -> i32 {
    let target_e = read_i32(msg, 0);

    // Reject exec of the running caller's own address space (the active TTBR0
    // hazard) — both spellings, like `do_exit`.
    if target_e == SELF {
        return EINVAL;
    }
    let target_idx = match super::resolve_target(proc_table, caller_nr, target_e) {
        Ok(idx) => idx,
        Err(e) => return e,
    };
    let caller_idx = proc_index(caller_nr).expect("caller in proc table");
    if target_idx == caller_idx {
        return EINVAL;
    }

    // Binary name: payload `4..4+EXEC_NAME_LEN`, NUL-padded. Copied into a
    // proc-name-sized buffer so a successful exec can adopt it as the proc name.
    let mut name_buf = [0u8; PROC_NAME_LEN];
    name_buf[..EXEC_NAME_LEN].copy_from_slice(&msg.payload[4..4 + EXEC_NAME_LEN]);
    let nul = name_buf
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(PROC_NAME_LEN);
    let name = match core::str::from_utf8(&name_buf[..nul]) {
        Ok(s) if !s.is_empty() => s,
        _ => return EINVAL,
    };

    // Resolve the boot-embedded module by name.
    let elf = match crate::boot_image::BootImage::get().module_by_name(name) {
        Some(elf) => elf,
        None => return ENOENT,
    };

    // Gate the target: a clean blocked receiver with its whole frame parked and
    // no half-done delivery (the `do_fork` parent gate). This is the last point
    // a failure leaves the target cleanly on its old image.
    let target = &proc_table[target_idx];
    let rts = target.rts_flags.load(Ordering::Relaxed);
    if rts & RTS_RECEIVING == 0 || rts & RTS_SENDING != 0 || target.misc_flags & MF_DELIVERMSG != 0
    {
        return EINVAL;
    }

    // Build the new address space *before* touching the target — a failure here
    // (OOM, malformed ELF) leaves it untouched on its old image.
    // SAFETY: single-threaded EL1; the sole caller of the frame allocator + ASID
    // pool here.
    let img = match unsafe { userland::load_exec_image(elf) } {
        Some(img) => img,
        None => return ENOMEM,
    };

    // Swap the new image onto the target and reset its frame to a clean EL0
    // start. Capture the old AS + trace fields, then end the borrow.
    let (old_ttbr0, old_asid, target_nr) = {
        let t = &mut proc_table[target_idx];
        let old = (t.ttbr0_pa, t.asid);
        t.regs = ArchRegisterFrame::EMPTY;
        t.regs.elr_el1 = img.entry;
        t.regs.sp_el0 = img.sp_top;
        t.regs.spsr_el1 = userland::STUB_SPSR_EL0;
        t.ttbr0_pa = img.ttbr0_pa;
        t.asid = img.asid;
        t.name = name_buf;
        // Drop the stale IPC continuation — the proc restarts at `_start`, not
        // in the middle of the SENDREC it used to reach PM. It is a receiver, so
        // it is on no `caller_q`, but clear defensively.
        t.getfrom_e = NONE;
        t.sendto_e = NONE;
        t.misc_flags &= !MF_DELIVERMSG;
        t.caller_q = None;
        t.deliver_msg = Message {
            m_source: 0,
            m_type: 0,
            payload: [0; 96],
        };
        (old.0, old.1, t.nr.get())
    };

    // Reclaim the old image. Safe: the target is never the running caller (PM),
    // so its old AS is not the active TTBR0.
    let freed = super::do_exit::teardown_addrspace(old_ttbr0, old_asid);

    // Unblock: clear RTS_RECEIVING so the scheduler resumes the target at the
    // new entry. `rts_unset` enqueues when the last block bit clears.
    // SAFETY: single-threaded EL1; `rts_unset` captures `nr` and ends the borrow
    // before touching the run queue.
    unsafe { sched::rts_unset(&mut proc_table[target_idx], RTS_RECEIVING) };

    let n = EXEC_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= EXEC_TRACE_HEAD || n.is_multiple_of(EXEC_TRACE_EVERY) {
        let _ = writeln!(
            Uart::new(),
            "[ksys SYS_EXEC] target={target_nr} name={name} entry={:#x} old_asid={old_asid} new_asid={} freed={freed}",
            img.entry,
            img.asid,
        );
    }
    OK
}

#[inline]
fn read_i32(msg: &Message, off: usize) -> i32 {
    i32::from_ne_bytes(
        msg.payload[off..off + 4]
            .try_into()
            .expect("payload in range"),
    )
}
