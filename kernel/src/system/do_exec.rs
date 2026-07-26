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
//! - write the SysV/Linux initial stack frame into it
//!   ([`execstack::build_initial_stack`]) so a C runtime finds `argc`/`argv`/
//!   `envp`/auxv where it expects them;
//! - reset the target's register frame to the new entry point with `sp` pointing
//!   at that frame (exec starts clean — no register carries over);
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
//! Takes `priv_table` to drop the old image's grant-table registration.
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
use minixrs_kernel_shared::com::NR_SYS_PROCS;
use minixrs_kernel_shared::endpoint::{NONE, SELF};
use minixrs_kernel_shared::error::{E2BIG, EINVAL, ENOENT, ENOMEM, OK};
use minixrs_kernel_shared::execstack;
use minixrs_kernel_shared::message::{Message, USER_PAGE_SIZE};

use crate::arch::aarch64::context::ArchRegisterFrame;
use crate::arch::aarch64::userland;
use crate::proc::flags::{MF_DELIVERMSG, RTS_RECEIVING, RTS_SENDING};
use crate::proc::proc_struct::PROC_NAME_LEN;
use crate::proc::table::{N_PROC_SLOTS, proc_index};
use crate::proc::{Priv, Proc, sched};
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
    priv_table: &mut [Priv; NR_SYS_PROCS],
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

    // Hand the new image a real SysV/Linux initial stack (slice 5.5, D13):
    // `[argc][argv…][NULL][envp…][NULL][auxv][AT_NULL]` with the name string
    // above it, so musl's crt runs unpatched. Still before the point of no
    // return — either failure below tears the *fresh* image down and leaves the
    // target on its old one, exactly like the `load_exec_image` failure above.
    let mut auxv = [(0u64, 0u64); execstack::MAX_AUXV];
    let mut n_auxv = 0;
    // `AT_PHDR` only when a PT_LOAD actually maps the header table — an image
    // linked without the `FILEHDR PHDRS` idiom leaves the ELF header in an
    // unmapped file prefix, and a VA pointing there would fault `__init_tls`.
    if let Some(phdr_va) = img.phdr_va {
        auxv[n_auxv] = (execstack::AT_PHDR, phdr_va);
        auxv[n_auxv + 1] = (execstack::AT_PHNUM, img.phnum as u64);
        auxv[n_auxv + 2] = (execstack::AT_PHENT, img.phentsize as u64);
        n_auxv += 3;
    }
    auxv[n_auxv] = (execstack::AT_PAGESZ, USER_PAGE_SIZE);
    n_auxv += 1;

    let mut frame_buf = [0u8; execstack::INITIAL_STACK_MAX];
    let Some(frame) =
        execstack::build_initial_stack(&mut frame_buf, img.sp_top, name, &auxv[..n_auxv])
    else {
        super::do_exit::teardown_addrspace(img.ttbr0_pa, img.asid);
        return E2BIG;
    };
    // `copy_to_user_as` is address-space-independent by design (slice 5.1): it
    // walks `ttbr0_pa` and copies through each frame's HHDM alias, so it works
    // on an address space that is not installed — no new copy machinery, and the
    // stack page's `Prot::writable` is checked like any other user write.
    //
    // Its errno is relayed **verbatim** rather than folded into `ENOMEM`, the
    // rule TTY already follows for `SYS_SAFECOPY`: every failure here is a
    // kernel bug (the destination is the stack page this call just mapped), and
    // `EFAULT` — the walk found nothing, or found something read-only — points
    // at a different bug from a genuine allocation failure. Flattening them
    // costs the one distinction worth having.
    if let Err(e) =
        crate::mm::uaccess::copy_to_user_as(img.ttbr0_pa, frame.sp, &frame_buf[..frame.len])
    {
        super::do_exit::teardown_addrspace(img.ttbr0_pa, img.asid);
        return e;
    }

    // Point of no return. Drop any grant-table registration the *old* image
    // made (slice 5.2): `grant_table` is a VA in the address space about to be
    // discarded, and exec preserves the privilege slot, so leaving it would aim
    // `verify_grant` at whatever the new image happens to map there. Only a
    // dedicated slot is cleared — a shared one (`USER_PRIV_ID`) belongs to every
    // user process collectively and can hold no table anyway (`do_setgrant`
    // rejects it). Same reasoning, and same guard, as `do_exit`'s slot teardown.
    if let Some(pid) = proc_table[target_idx].priv_id {
        let pidx = pid.as_usize();
        if pidx < NR_SYS_PROCS && priv_table[pidx].proc_nr == Some(proc_table[target_idx].nr) {
            priv_table[pidx].grant_table = 0;
            priv_table[pidx].grant_entries = 0;
        }
    }

    // Swap the new image onto the target and reset its frame to a clean EL0
    // start. Capture the old AS + trace fields, then end the borrow.
    let (old_ttbr0, old_asid, target_nr) = {
        let t = &mut proc_table[target_idx];
        let old = (t.ttbr0_pa, t.asid);
        t.regs = ArchRegisterFrame::EMPTY;
        t.regs.elr_el1 = img.entry;
        t.regs.sp_el0 = frame.sp;
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
    //
    // The device-leaf count is dropped rather than traced: the new image built by
    // `load_exec_image` carries no device mapping (the TTY UART pre-map lives in
    // `load_boot_server`, deliberately outside the image-generic helper), so a
    // proc that exec'd would *lose* its device window — which is why nothing that
    // owns one execs. Only `SYS_EXIT` reports the count, where it is the boot
    // selftest's proof.
    let (freed, _dev_pages) = super::do_exit::teardown_addrspace(old_ttbr0, old_asid);

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
        // The initial stack the new image will read. `auxv` counts real pairs,
        // excluding the `AT_NULL` terminator — which is what makes the
        // conditional `AT_PHDR` arm observable rather than quietly dead. `sp`
        // goes last: it is derived from the name length and auxv count, so the
        // asserted marker substring stops before it.
        let _ = writeln!(
            Uart::new(),
            "[exec] argc=1 envc=0 auxv={n_auxv} sp={:#x}",
            frame.sp,
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
