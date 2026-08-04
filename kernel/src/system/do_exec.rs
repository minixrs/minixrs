// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_EXEC` — replace a target process's program image in place.
//!
//! PM owns exec (MINIX 3 `pm/exec.c` → `sys_exec`): a user proc `SENDREC`s
//! `PM_EXEC` to PM, PM works out where the image is, and issues `SYS_EXEC`
//! naming that proc as the target. The kernel's half is mechanical:
//!
//! - resolve the image ([`resolve_source`]) — either by name in the MXBI archive
//!   ([`BootImage::module_by_name`]) or through a grant PM names;
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
//! ## The two source forms (slice 5.9, decision D6)
//!
//! `EXEC_SRC_NAME` is slice 4.7's: the name field doubles as an MXBI module name.
//! `EXEC_SRC_GRANT` is exec-from-FS: VFS stages the file into its own memory and
//! direct-grants it to PM, PM names that grant here, and the kernel reads the ELF
//! **through the grant** with slice 5.1's page-walking copy engine. The kernel
//! keeps ELF authority and gains no filesystem, no heap, and no staging buffer —
//! `ElfSource::UserGrant` is read a header at a time onto this call's own stack.
//!
//! The grant is validated by [`do_safecopy::verify_grant`], not by anything
//! written here: `who_to` must be PM's own stored endpoint, the sequence must
//! match, `CPF_READ` must be granted, and the range must fit — the same eleven
//! checks `bdev.deny` and `fs.deny` already exercise on every boot. The read
//! happens **before the point of no return**, so a granted buffer that turns out
//! not to be an ELF leaves the target untouched on its old image and PM relays
//! `ENOEXEC` to it.
//!
//! ## Message payload layout (offsets within `Message::payload`)
//!
//! | offset             | field                                  | direction |
//! |--------------------|----------------------------------------|-----------|
//! |  0..4              | target endpoint (i32)                  | in        |
//! |  4..4+EXEC_NAME_LEN| `argv[0]` / proc name (NUL-padded)     | in        |
//! | 20..24             | source selector (`EXEC_SRC_*`)         | in        |
//! | 24..28             | granter endpoint (i32, grant form)     | in        |
//! | 28..32             | grant id (i32, grant form)             | in        |
//! | 32..40             | image length (u64, grant form)         | in        |

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::callnr::{
    EXEC_GRANT_OFF, EXEC_GRANTER_OFF, EXEC_LEN_OFF, EXEC_NAME_LEN, EXEC_SRC_GRANT, EXEC_SRC_NAME,
    EXEC_SRC_OFF,
};
use minixrs_kernel_shared::com::NR_SYS_PROCS;
use minixrs_kernel_shared::endpoint::{Endpoint, NONE, SELF};
use minixrs_kernel_shared::error::{E2BIG, EINVAL, ENOENT, OK};
use minixrs_kernel_shared::execimage::MAX_IMAGE_BYTES;
use minixrs_kernel_shared::execstack;
use minixrs_kernel_shared::grant::CPF_READ;
use minixrs_kernel_shared::message::{Message, USER_PAGE_SIZE};

use crate::arch::aarch64::context::ArchRegisterFrame;
use crate::arch::aarch64::userland;
use crate::boot_image::elf::ElfSource;
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
    // The source selector is a pure payload check, so it sits with the other
    // payload checks rather than beside the resolution it governs. Selector 0 —
    // a zeroed payload — is invalid, never a default form.
    let src = read_i32(msg, EXEC_SRC_OFF);
    if src != EXEC_SRC_NAME && src != EXEC_SRC_GRANT {
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

    // `argv[0]` / the new proc name: payload `4..4+EXEC_NAME_LEN`, NUL-padded.
    // Copied into a proc-name-sized buffer so a successful exec adopts it as the
    // proc name. In the grant form PM sends the path's *basename* here, which is
    // what keeps this field — and `execstack`'s geometry — unchanged by 5.9.
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

    // Resolve where the image's bytes are. Both arms fail before anything is
    // allocated, so the target stays on its old image.
    let (source, granter_e, image_len) =
        match resolve_source(proc_table, priv_table, caller_idx, src, name, msg) {
            Ok(v) => v,
            Err(e) => return e,
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
    let img = match unsafe { userland::load_exec_image(&source) } {
        Ok(img) => img,
        // Relayed verbatim rather than folded into `ENOMEM`: since slice 5.9 the
        // bytes can be a file, so `ENOEXEC` ("that is not an executable") is an
        // ordinary answer and is a different thing to tell PM than `ENOMEM` or
        // `EFAULT`.
        Err(e) => return e,
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
        // `name=… src=… entry=` keeps the two asserted substrings adjacent, so
        // one marker proves *program and form together* — which is what makes
        // "the C program came out of the filesystem" assertable at all, since
        // `hello`'s own console output is byte-identical either way. The
        // run-variable fields go last, as they have since slice 4.7.
        let src_name = if src == EXEC_SRC_GRANT {
            "grant"
        } else {
            "name"
        };
        let _ = writeln!(
            Uart::new(),
            "[ksys SYS_EXEC] target={target_nr} name={name} src={src_name} entry={:#x} old_asid={old_asid} new_asid={} freed={freed} granter={granter_e} len={image_len}",
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

/// Work out where the image's bytes are, and hand back what the trace needs to
/// say about it.
///
/// Returns `(source, granter_endpoint, length)`. For the name form the granter is
/// [`NONE`] and the length is the module's, so the trace's two trailing fields
/// stay meaningful in both — a `granter=0 len=0` would read as a broken grant
/// rather than as "there was no grant".
///
/// Both arms are total and allocate nothing: a failure here is a failure with the
/// target still cleanly on its old image, which is the invariant every check
/// before the point of no return exists to preserve.
fn resolve_source<'a>(
    proc_table: &[Proc; N_PROC_SLOTS],
    priv_table: &[Priv; NR_SYS_PROCS],
    caller_idx: usize,
    src: i32,
    name: &str,
    msg: &Message,
) -> Result<(ElfSource<'a>, Endpoint, usize), i32> {
    if src == EXEC_SRC_NAME {
        let elf = crate::boot_image::BootImage::get()
            .module_by_name(name)
            .ok_or(ENOENT)?;
        return Ok((ElfSource::Bytes(elf), NONE, elf.len()));
    }

    // The grant form. `MAX_IMAGE_BYTES` bounds the length *before* the grant is
    // even looked at, so a caller cannot ask the validator to reason about a
    // ~16 EiB range; zero is refused outright, since an empty image can only fail
    // the header read a few lines later with a less useful errno.
    let granter_e: Endpoint = read_i32(msg, EXEC_GRANTER_OFF);
    let gid = read_i32(msg, EXEC_GRANT_OFF);
    let len = read_u64(msg, EXEC_LEN_OFF);
    if len == 0 || len > MAX_IMAGE_BYTES as u64 {
        return Err(EINVAL);
    }

    // The shared validator, with nothing added: offset 0 (the granted buffer
    // holds the image from its start — the BDEV/FS bands' no-grant-offset rule)
    // and `CPF_READ`, because the kernel only ever reads an image. A magic grant
    // resolves to `who_from` here exactly as it would for `SYS_SAFECOPY`, which
    // is why nothing about "the granter is not the owner" needs saying twice.
    let (owner_ttbr0, va) = super::do_safecopy::verify_grant(
        proc_table, priv_table, caller_idx, granter_e, gid, 0, len, CPF_READ,
    )?;

    Ok((
        ElfSource::UserGrant {
            ttbr0_pa: owner_ttbr0,
            va,
            len: len as usize,
        },
        granter_e,
        len as usize,
    ))
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
fn read_u64(msg: &Message, off: usize) -> u64 {
    u64::from_ne_bytes(
        msg.payload[off..off + 8]
            .try_into()
            .expect("payload in range"),
    )
}
