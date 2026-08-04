// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs PM (process manager) server — part A (slice 4.5).
//!
//! PM owns the POSIX-visible process identity the kernel deliberately knows
//! nothing about: pids, parentage, and signal disposition. Part A stands up
//! the [`mproc`] table, `getpid`/`getppid` (one `PM_GETPID` call — the reply's
//! `m_type` is the pid, payload `0..4` the parent's pid), and the minimal
//! kill path: PM drains kernel-queued signals and, with no handlers or masks
//! in Phase 4, applies the default disposition — terminate. Fork/exec/wait
//! are part B (slice 4.6).
//!
//! ## The kill chain (all observable kernel-side; EL0 cannot print)
//!
//! A system process raises a signal with `SYS_KILL` (in 4.5: VM, when a page
//! fault lands outside every region — stub D's deliberate SIGSEGV). The
//! kernel queues it (`cause_sig`) and wakes PM with a `NOTIFY` from `SYSTEM`;
//! [`drain_ksigs`] then loops `SYS_GETKSIG` → disposition → `SYS_ENDKSIG`
//! until the kernel reports no more pending procs. Boot servers
//! (`MF_PRIV_PROC`) are never terminated — signal-as-message delivery to
//! system processes (MINIX's sig2mess) waits for a consumer (RS restarts).
//! Every `SYS_GETKSIG`-returned target is `SYS_ENDKSIG`-acknowledged, even
//! skipped ones, so no proc is left stranded in signal-pending state; the
//! terminate (`SYS_EXIT`) happens *before* the acknowledge so the target
//! never becomes runnable in between.
//!
//! ## Live demo
//!
//! init (PID 1) is the live process-lifecycle exercise (slice 4.8): a real boot
//! process the kernel loader makes runnable, it forks a child, the child execs
//! the `worker` binary, and init `wait`s to reap it before looping — driving
//! `PM_FORK` / `PM_EXEC` / `PM_WAIT` over the PM ↔ shared-USER-priv edge opened
//! by `populate_user_priv`. (This replaced the slice-4.6 frozen stub E, which PM
//! used to release via `SYS_PRIVCTL(PRIVCTL_SET_USER)` at SEF init.)

// Freestanding for the bare-metal build, but a normal host binary under
// `cargo test` so `mproc`'s logic gets host-runnable unit tests. Same gating
// as the RS/SCHED servers.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

minixrs_abi_note::brand!();

mod mproc;

use minixrs_ipc::{ipc_send, ipc_sendrec};
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    EXEC_NAME_LEN, EXEC_SRC_NAME, EXEC_SRC_OFF, PM_EXEC, PM_EXEC_NAME_OFF, PM_EXIT, PM_FORK,
    PM_GETPID, PM_GRANT_TEST, PM_WAIT, PRIVCTL_SET_USER, SAFECOPY_FROM, SAFECOPY_TO,
    SCHEDULING_START, SCHEDULING_STOP, SYS_ENDKSIG, SYS_EXEC, SYS_EXIT, SYS_FORK,
    SYS_GETINFO_NAME_LEN, SYS_GETKSIG, SYS_PRIVCTL, VM_FORK,
};
use minixrs_kernel_shared::com::{
    INIT_PROC_NR, SCHED_PROC_NR, SYSTEM, VFS_PROC_NR, VM_PROC_NR, boot_endpoint,
};
use minixrs_kernel_shared::endpoint::{Endpoint, NONE, SELF, endpoint_proc};
use minixrs_kernel_shared::error::{EAGAIN, ECHILD, EFAULT, EINVAL, EPERM, ESRCH, OK};
use minixrs_kernel_shared::grant::{CPF_READ, grant_id};
use minixrs_kernel_shared::ipc_const::NOTIFY_MESSAGE;
use minixrs_server_rt::{
    GrantPool, SefConfig, buf_addr, diag_fmt, rd_i32, rd_name, rd_u64, sef_publish_to_ds,
    sef_startup, sys_copy, sys_safecopy, wr_i32,
};

/// Priority band and quantum PM assigns a forked child via `SCHEDULING_START`.
/// These match SCHED's own `USER_Q` / `QUANTUM` (`servers/sched/src/policy.rs`)
/// so a child round-robins in the managed band rather than sinking behind the
/// kernel-scheduled stubs.
const CHILD_PRIORITY: i32 = 8;
const CHILD_QUANTUM: i32 = 5;

/// Staging buffer for the slice-5.2 grant demo. Bounds every copy PM makes, so
/// a `PM_GRANT_TEST` naming an over-large length is clamped rather than
/// trusted.
const GRANT_TEST_MAX: usize = 64;

/// Simultaneously outstanding grants PM can hold. PM's stack is one page and
/// the pool costs `N * 32` bytes; the demo issues one magic grant at a time.
const GRANT_SLOTS: usize = 8;

/// Bytes the magic-grant probe reads out of init's address space, and the VA it
/// reads them from — the load base every user binary shares (`user.ld`), so
/// init's first text page is mapped there for the whole run. The *values* are
/// deliberately never asserted: they are init's instruction encodings and change
/// whenever init is rebuilt.
const MAGIC_PROBE_VA: u64 = 0x0010_0000;
const MAGIC_PROBE_LEN: usize = 8;

/// A user VA nothing in PM's address space is mapped at — well clear of its
/// image (1 MiB) and its stack. Granting it produces a well-formed grant that
/// only the page-table walk can refuse, which is how the denial probe reaches
/// the copy engine's `EFAULT` path.
const UNMAPPED_PROBE_VA: u64 = 0x4000_0000;

/// ELF entry point. The kernel primes `SP_EL0` before `eret`, so `_start`
/// dives straight into Rust. Gated to `not(test)` (under `cargo test` the
/// crate links as a host binary with the C runtime's `_start`);
/// `.text._start` is gated to the minixrs target so a Mach-O host
/// `cargo check` still type-checks.
#[cfg(not(test))]
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "minixrs", unsafe(link_section = ".text._start"))]
pub extern "C" fn _start() -> ! {
    main()
}

#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    // `sef_startup` learns PM's endpoint/name via SYS_GETINFO(GET_WHOAMI) and
    // runs `pm_init` (DS publish, mproc seed). No signal
    // handler: PM *is* the signal manager, not a signal consumer. On failure
    // there is nothing to print from EL0 — park forever.
    let sef = sef_startup(SefConfig {
        init_fresh: Some(pm_init),
        signal_handler: None,
    })
    .unwrap_or_else(|_| {
        loop {
            core::hint::spin_loop()
        }
    });

    let system = boot_endpoint(SYSTEM);
    // VM and SCHED are boot servers at generation 0, so `boot_endpoint` is their
    // live endpoint (they never exit in Phase 4). PM drives the fork tree
    // through them: VM clones the child's regions, SCHED schedules it.
    let vm = boot_endpoint(VM_PROC_NR);
    let sched = boot_endpoint(SCHED_PROC_NR);

    // PM's own grant pool (slice 5.2), used for the magic-grant probe. A
    // `main`-frame value that outlives the receive loop — `server-rt` keeps the
    // table as a value rather than a static so it can be owned like this.
    let mut grants: GrantPool<GRANT_SLOTS> = GrantPool::new();
    let own_e = sef.endpoint();

    let mut msg = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };
    loop {
        if sef.receive(&mut msg) != OK {
            continue;
        }
        match msg.m_type {
            // The kernel's ksig wake-up (`deliver_ksig`): a NOTIFY stamped
            // from SYSTEM. RS heartbeat pings are also NOTIFYs but are
            // consumed inside `sef.receive` (classified `Ping`), so a NOTIFY
            // from any other source here is simply ignored.
            NOTIFY_MESSAGE if msg.m_source == system => drain_ksigs(system),
            PM_GETPID => handle_getpid(&mut msg),
            PM_FORK => handle_fork(system, vm, sched, &mut msg),
            PM_EXIT => handle_exit(system, sched, &mut msg),
            PM_WAIT => handle_wait(&mut msg),
            PM_EXEC => handle_exec(system, &mut msg),
            PM_GRANT_TEST => handle_grant_test(own_e, &mut grants, &msg),
            // Unknown request: drop it.
            _ => {}
        }
    }
}

/// SEF fresh-init callback: publish PM's endpoint to DS and seed the mproc
/// table. init (PID 1) — the live fork/exec/wait driver that replaced the
/// slice-4.6 stub E demo — is a real boot process made runnable by the kernel
/// boot loader, so PM no longer hand-releases a frozen stub here.
#[cfg_attr(test, allow(dead_code))]
fn pm_init(_endpoint: Endpoint, name: &[u8; SYS_GETINFO_NAME_LEN]) -> i32 {
    let rc = sef_publish_to_ds(name);
    if rc != OK {
        return rc;
    }
    mproc::seed();
    OK
}

/// Handle `PM_GETPID`: reply `m_type = pid` (MINIX result-is-pid convention;
/// errors negative) with the parent's pid in payload `0..4`, so `getppid`
/// needs no second call. The caller is identified by the kernel-stamped
/// `m_source` — there is no spoofable request payload.
#[cfg_attr(test, allow(dead_code))]
fn handle_getpid(msg: &mut Message) {
    let caller_e = msg.m_source;
    let result = match usize::try_from(endpoint_proc(caller_e).get())
        .ok()
        .and_then(mproc::getpid)
    {
        Some((pid, ppid)) => {
            wr_i32(msg, 0, ppid);
            pid
        }
        None => ESRCH,
    };
    reply(caller_e, msg, result);
}

/// Handle `PM_FORK`: create a child of the caller. PM allocates a child slot,
/// then drives the whole tree — `SYS_FORK` (kernel clones the frozen child +
/// its address space), `VM_FORK` (VM clones the region set), `SCHEDULING_START`
/// (SCHED schedules the still-frozen child), and `SYS_PRIVCTL(PRIVCTL_SET_USER)`
/// (release the freeze; the child stays a blocked receiver). Finally PM wakes
/// **both** halves of the shared blocked SENDREC: the child's reply carries
/// `m_type = 0`, the parent's carries the child pid. The ordering is deliberate
/// — release before the replies, and the replies last — so the child cannot run
/// until its identity, memory, and scheduling are fully set up.
#[cfg_attr(test, allow(dead_code))]
fn handle_fork(system: Endpoint, vm: Endpoint, sched: Endpoint, msg: &mut Message) {
    let parent_e = msg.m_source;
    let Some(parent_slot) = usize::try_from(endpoint_proc(parent_e).get()).ok() else {
        return reply(parent_e, msg, EINVAL);
    };
    if !mproc::in_use(parent_slot) {
        return reply(parent_e, msg, EINVAL);
    }

    // Allocate the child's slot (= its kernel proc number). Bail before any
    // kernel work if the table is full — recovery mid-fork is a nuisance.
    let Some(child_slot) = mproc::alloc_slot() else {
        return reply(parent_e, msg, EAGAIN);
    };

    // Kernel fork: clone the parent into the frozen child; get its endpoint.
    let (rc, child_e) = sys_fork(system, parent_e, child_slot as i32);
    if rc != OK {
        mproc::cleanup(child_slot);
        return reply(parent_e, msg, rc);
    }

    // VM: clone the parent's region set into the child. On failure, roll the
    // kernel fork back before returning the error to the parent.
    let vrc = vm_fork(vm, parent_e, child_e);
    if vrc != OK {
        let _ = sys_exit(system, child_e);
        mproc::cleanup(child_slot);
        return reply(parent_e, msg, vrc);
    }

    // SCHED schedules the (still-frozen) child. On failure, roll back like the
    // VM step above so we never record an unschedulable child.
    let src = sched_start(sched, child_e, CHILD_PRIORITY, CHILD_QUANTUM);
    if src != OK {
        let _ = sys_exit(system, child_e);
        mproc::cleanup(child_slot);
        return reply(parent_e, msg, src);
    }

    // Release the freeze; the child stays RTS_RECEIVING — a blocked receiver,
    // off the run queue — until its reply below clears the last block bit. On
    // failure roll back like `handle_exit` — stop SCHED first (endpoint still
    // valid), then tear the child down — so a child that never became runnable
    // is never recorded as "live" (which would hang the parent's `wait()`
    // forever). Both calls are boot-server-backed and don't fail with correct
    // wiring; this is defense-in-depth.
    let prc = privctl_set_user(system, child_e);
    if prc != OK {
        let _ = sched_stop(sched, child_e);
        let _ = sys_exit(system, child_e);
        mproc::cleanup(child_slot);
        return reply(parent_e, msg, prc);
    }

    let child_pid = mproc::set_child(child_slot, parent_slot, child_e);

    // Wake both halves. Child returns 0 (it is the child); parent returns the
    // child pid. Reuse `msg` for both — the kernel snapshots each send.
    reply(child_e, msg, 0);
    reply(parent_e, msg, child_pid);
}

/// Handle `PM_EXIT`: terminate the caller with the status in payload `0..4`.
/// PM hands the child back to the kernel scheduler (`SCHEDULING_STOP`, while its
/// endpoint is still valid) then tears it down (`SYS_EXIT`, a full teardown). It
/// keeps the `mproc` slot as a zombie holding the encoded status. If the parent
/// is already blocked in `wait()`, PM wakes it immediately and reaps the zombie;
/// otherwise the zombie waits for the parent's next `wait()`. PM sends the
/// exiting child no reply — it is gone.
#[cfg_attr(test, allow(dead_code))]
fn handle_exit(system: Endpoint, sched: Endpoint, msg: &mut Message) {
    let child_e = msg.m_source;
    let status = rd_i32(msg, 0);
    let Some(child_slot) = usize::try_from(endpoint_proc(child_e).get()).ok() else {
        return;
    };
    // Capture the pid while the proc is still live (getpid ignores zombies).
    let Some((child_pid, _)) = mproc::getpid(child_slot) else {
        return;
    };
    // Scope limitation: there is no reparenting yet. If the parent already
    // exited, `parent_of` falls back to the child itself, so the zombie below is
    // never reaped (no live parent will `wait()` for it). Harmless while every
    // parent is a long-lived boot stub; reparenting orphans to INIT waits for a
    // running INIT (4.8+).
    let parent_slot = mproc::parent_of(child_slot).unwrap_or(child_slot);

    let _ = sched_stop(sched, child_e);
    let _ = sys_exit(system, child_e);
    mproc::set_zombie(child_slot, encode_status(status));

    // Notify a waiting parent directly — no async SIGCHLD in Phase 4 (the kernel
    // signal path default-terminates, which would kill a handler-less parent).
    if mproc::is_waiting(parent_slot)
        && let Some(parent_e) = mproc::endpoint_of(parent_slot)
    {
        wr_i32(msg, 0, encode_status(status));
        reply(parent_e, msg, child_pid);
        mproc::set_waiting(parent_slot, false);
        mproc::cleanup(child_slot);
    }
}

/// Handle `PM_WAIT`: reap a child of the caller. If a zombie child exists, reply
/// its pid (with the encoded status in payload `0..4`) and free the slot. If a
/// live child exists but none has exited, suspend the caller (no reply) — the
/// exiting child's `handle_exit` will wake it. With no children, reply `ECHILD`.
#[cfg_attr(test, allow(dead_code))]
fn handle_wait(msg: &mut Message) {
    let parent_e = msg.m_source;
    let Some(parent_slot) = usize::try_from(endpoint_proc(parent_e).get()).ok() else {
        return reply(parent_e, msg, EINVAL);
    };

    if let Some((slot, pid, status)) = mproc::find_zombie_child(parent_slot) {
        wr_i32(msg, 0, status);
        reply(parent_e, msg, pid);
        mproc::cleanup(slot);
    } else if mproc::has_live_child(parent_slot) {
        // SUSPEND: no reply. The caller stays blocked in its SENDREC until a
        // child exits and `handle_exit` replies on its behalf.
        mproc::set_waiting(parent_slot, true);
    } else {
        reply(parent_e, msg, ECHILD);
    }
}

/// Handle `PM_EXEC`: replace the caller's program image with the boot-embedded
/// module it names. The target comes from the payload
/// ([`PM_EXEC_NAME_OFF`]`..+`[`EXEC_NAME_LEN`], NUL-padded) as of slice 5.6 —
/// before that PM hardcoded `"worker"`. PM issues `SYS_EXEC` naming the caller
/// (kernel-stamped `m_source`); the kernel resolves the name against the boot
/// image, builds the new address space, and resumes the caller at the new entry
/// point.
///
/// On success PM sends **no** reply — the caller does not return from this call,
/// it restarts at the new image's `_start`. On failure the caller is untouched on
/// its old image, so PM replies the errno: `EINVAL` for a malformed name, else
/// whatever the kernel answered (`ENOENT` for an unknown module).
#[cfg_attr(test, allow(dead_code))]
fn handle_exec(system: Endpoint, msg: &mut Message) {
    let caller_e = msg.m_source;
    // `name` borrows `msg`; `sys_exec` marshals into its own buffer and never
    // touches it, so the borrow ends before `reply` needs `&mut msg`.
    let rc = match rd_name(msg, PM_EXEC_NAME_OFF, EXEC_NAME_LEN) {
        Some(name) => sys_exec_name(system, caller_e, name),
        None => EINVAL,
    };
    if rc != OK {
        reply(caller_e, msg, rc);
    }
    // Success: the kernel already resumed the caller at the new image — no reply.
}

/// Handle `PM_GRANT_TEST` (slice 5.2): the live end-to-end exercise of the
/// grant engine. VFS has direct-granted PM a 64-byte read-only buffer and sent
/// the id in-band; PM now reads it three different ways and reports each on the
/// kernel debug channel.
///
/// 1. **Grant** — `SYS_SAFECOPY(SAFECOPY_FROM, …)` pulls the buffer out of VFS's
///    address space through the grant. Reports the checksum.
/// 2. **Raw** — `SYS_COPY` reads the *same* bytes from VFS's raw address with no
///    grant at all (D4's small control-plane read). Reports whether the two
///    checksums agree, which is what proves the grant path copied the right
///    bytes and not merely *some* bytes.
/// 3. **Magic** — PM grants *itself* access to init's text page and safecopies
///    from it: a genuine third-party read out of a process that granted nothing
///    and is not party to the exchange. This is the arm slices 5.5+ depend on,
///    and without a live probe it would sit untested until then.
/// 4. **Denials** — [`grant_denials`], the copies that must *not* happen.
///
/// Each line's prefix is stable and assertable (`tests/qemu-boot.expected`); the
/// run-variable values follow it. The magic line deliberately reports only the
/// length: init's `_start` bytes change whenever init is rebuilt.
///
/// PM sends no reply — VFS used `ipc_send`, not a SENDREC.
///
/// ## Why the granter is not read from the payload
///
/// PM holds `SYS_COPY` and `SYS_SAFECOPY`; its clients — init, and every forked
/// child on the shared USER privilege — hold neither. A caller-supplied granter
/// endpoint would therefore let any of them aim a privileged cross-address-space
/// copy at a third party *through PM*, and read back a checksum of the result: a
/// textbook confused deputy. The granter is the kernel-stamped `m_source`
/// instead (the anti-spoof property DS relies on for `DS_PUBLISH`), so a client
/// can only ever name its own address space. The request is additionally served
/// only when that source is VFS, since nothing else has business driving a
/// demo-only request number.
#[cfg_attr(test, allow(dead_code))]
fn handle_grant_test(own_e: Endpoint, grants: &mut GrantPool<GRANT_SLOTS>, msg: &Message) {
    // The granter is whoever the kernel says sent this, never a payload field.
    let granter = msg.m_source;
    if granter != boot_endpoint(VFS_PROC_NR) {
        return;
    }
    let gid = rd_i32(msg, 0);
    let rw_gid = rd_i32(msg, 8);
    let raw_addr = rd_u64(msg, 16);
    // Clamp to PM's staging buffer rather than trusting the sender's length.
    let len = match usize::try_from(rd_i32(msg, 4)) {
        Ok(n) if n <= GRANT_TEST_MAX => n,
        _ => GRANT_TEST_MAX,
    };

    // 1. Through the grant.
    let mut buf = [0u8; GRANT_TEST_MAX];
    let rc = sys_safecopy(
        SAFECOPY_FROM,
        granter,
        gid,
        0,
        buf_addr(&mut buf),
        len as u64,
    );
    let grant_sum = checksum(&buf[..len]);
    if rc == OK {
        diag_fmt(format_args!("grant.test ok sum={grant_sum} len={len}"));
    } else {
        diag_fmt(format_args!("grant.test FAIL rc={rc}"));
        return;
    }

    // 2. The same bytes with no grant, straight out of VFS's address space.
    let mut raw = [0u8; GRANT_TEST_MAX];
    let rc = sys_copy(granter, raw_addr, SELF, buf_addr(&mut raw), len as u64);
    if rc == OK {
        let same = u8::from(checksum(&raw[..len]) == grant_sum);
        diag_fmt(format_args!("copy ok match={same}"));
    } else {
        diag_fmt(format_args!("copy FAIL rc={rc}"));
    }

    // 3. Magic: PM grants itself access to init's memory, then reads it.
    let mut probe = [0u8; MAGIC_PROBE_LEN];
    match grants.grant_magic(
        own_e,
        boot_endpoint(INIT_PROC_NR),
        MAGIC_PROBE_VA,
        MAGIC_PROBE_LEN as u64,
        CPF_READ,
    ) {
        Ok(mgid) => {
            let rc = sys_safecopy(
                SAFECOPY_FROM,
                own_e,
                mgid,
                0,
                buf_addr(&mut probe),
                MAGIC_PROBE_LEN as u64,
            );
            if rc == OK {
                diag_fmt(format_args!("magic ok len={MAGIC_PROBE_LEN}"));
            } else {
                diag_fmt(format_args!("magic FAIL rc={rc}"));
            }
            let _ = grants.revoke(mgid);
        }
        Err(e) => diag_fmt(format_args!("magic FAIL grant={e}")),
    }

    // 4. The copies that must be refused.
    grant_denials(own_e, grants, granter, gid, rw_gid, len as u64);
}

/// Probe the checks that no *successful* copy exercises.
///
/// Slice 5.1's lesson, applied to grants: a validator no marker exercises is a
/// validator that can regress silently. Each case is constructed so that every
/// check but the one it targets passes — remove that check from the kernel and
/// the copy succeeds, flipping this line from `ok` to `FAIL <name>`.
///
/// The `rodata` case is the important one and the reason VFS issues a second,
/// deliberately-lying grant: a granter may claim `CPF_WRITE` over memory that is
/// not writable, and the kernel copies through its own HHDM alias where the
/// MMU's EL0 permission bits do not apply. `EFAULT` (not `EPERM`) is the
/// expected answer there — the *grant* is in order; the destination page is not.
/// `unmapped` is the same distinction on the read side: a well-formed grant over
/// a page nothing backs, refused by the walk rather than by any grant check.
///
/// Two arms are not probed. `CPF_INDIRECT` cannot be, because [`GrantPool`] has
/// no API to mint an indirect grant (D4 defers them). The magic path's
/// `SYS_PROC` gate cannot be, because every process able to issue `SYS_SETGRANT`
/// at all is server-grade — the shared USER privilege has an empty kernel-call
/// mask — so no granter exists that the gate would reject. A non-server granter
/// arrives with the musl slices; that probe belongs there.
///
/// `idx` is an end-to-end case rather than an isolated one: an index past the
/// granter's table is refused, but the read of that non-existent entry fails
/// first (unmapped page, or a slot without `CPF_USED`), so the index check
/// itself is defence in depth rather than the observed cause.
#[allow(clippy::too_many_arguments)] // the demo's inputs, threaded straight
// through from the PM_GRANT_TEST payload
#[cfg_attr(test, allow(dead_code))]
fn grant_denials(
    own_e: Endpoint,
    grants: &mut GrantPool<GRANT_SLOTS>,
    vfs_e: Endpoint,
    vfs_gid: i32,
    vfs_rw_gid: i32,
    vfs_len: u64,
) {
    let mut scratch = [0u8; MAGIC_PROBE_LEN];
    let addr = buf_addr(&mut scratch);
    let n = MAGIC_PROBE_LEN as u64;

    // Three grants of PM's own scratch buffer, each valid in every respect but
    // the one its probe violates.
    //   `not_mine`  — names VFS as the only permitted user.
    //   `read_only` — carries CPF_READ but not CPF_WRITE.
    //   `stale`     — revoked, and its slot immediately reissued, so the id's
    //                 index is live but its sequence is a generation behind.
    let (Ok(not_mine), Ok(read_only), Ok(stale)) = (
        grants.grant_direct(vfs_e, addr, n, CPF_READ),
        grants.grant_direct(own_e, addr, n, CPF_READ),
        grants.grant_direct(own_e, addr, n, CPF_READ),
    ) else {
        return diag_fmt(format_args!("grant.deny FAIL setup"));
    };
    if grants.revoke(stale).is_err() || grants.grant_direct(own_e, addr, n, CPF_READ).is_err() {
        return diag_fmt(format_args!("grant.deny FAIL reissue"));
    }
    // A grant over an address PM has nothing mapped at. Perfectly well-formed —
    // only the page-table walk can catch it, which is the point.
    let Ok(unmapped) = grants.grant_direct(own_e, UNMAPPED_PROBE_VA, n, CPF_READ) else {
        return diag_fmt(format_args!("grant.deny FAIL setup"));
    };

    let probes: [(&str, i32, Endpoint, i32, u64, i32); 7] = [
        // The grant permits VFS, and PM is not VFS.
        ("grantee", SAFECOPY_FROM, own_e, not_mine, 0, EPERM),
        // A read-only grant cannot be written through.
        ("access", SAFECOPY_TO, own_e, read_only, 0, EPERM),
        // The slot is live, but under a newer sequence than this id names.
        ("seq", SAFECOPY_FROM, own_e, stale, 0, EPERM),
        // No such slot in VFS's table.
        ("idx", SAFECOPY_FROM, vfs_e, grant_id(9999, 0), 0, EPERM),
        // A live grant, read past the end of the range it covers.
        ("range", SAFECOPY_FROM, vfs_e, vfs_gid, vfs_len, EPERM),
        // A grant that claims CPF_WRITE over VFS's read-only `.rodata`.
        ("rodata", SAFECOPY_TO, vfs_e, vfs_rw_gid, 0, EFAULT),
        // A valid grant over a page nobody has mapped.
        ("unmapped", SAFECOPY_FROM, own_e, unmapped, 0, EFAULT),
    ];

    let mut denied = 0usize;
    for (name, dir, granter, gid, offset, want) in probes {
        let rc = sys_safecopy(dir, granter, gid, offset, addr, n);
        if rc == want {
            denied += 1;
        } else {
            diag_fmt(format_args!("grant.deny FAIL {name} rc={rc}"));
        }
    }
    if denied == probes.len() {
        diag_fmt(format_args!("grant.deny ok n={denied}"));
    }
}

/// Wrapping byte sum. Not a strong checksum — it only has to change when the
/// bytes do, which is all the demo needs to distinguish a real copy from a
/// zeroed buffer or a short one.
#[cfg_attr(test, allow(dead_code))]
fn checksum(b: &[u8]) -> u32 {
    b.iter()
        .fold(0u32, |acc, &x| acc.wrapping_mul(31).wrapping_add(x as u32))
}

/// MINIX `W_EXITCODE` for a normal exit: the low byte of `status` in bits 8..16,
/// the terminating-signal byte (0 for a normal exit) in bits 0..8.
fn encode_status(status: i32) -> i32 {
    (status & 0xff) << 8
}

/// Drain the kernel's pending-signal queue: `SYS_GETKSIG` until it reports
/// `NONE`, disposing of each signalled proc. Default disposition is
/// terminate (`SYS_EXIT`) for user processes; boot servers are skipped.
///
/// Contract with the kernel (see `system::do_sig`): every endpoint GETKSIG
/// returns must be either terminated or ENDKSIG-acknowledged, or it stays
/// signal-pending forever. A *successful* terminate gets **no** `SYS_ENDKSIG`:
/// as of slice 4.6 `SYS_EXIT` is a full teardown — signal state zeroed, slot
/// freed, endpoint generation bumped — so a post-exit acknowledge would only
/// bounce off `okendpt` with `EDEADSRCDST`. (MINIX can acknowledge after
/// terminate because its `sys_clear` is deferred behind VFS; minix.rs tears
/// down immediately.) GETKSIG's scan gates on `sig_pending != 0`, which the
/// exit zeroed, so the dead proc is never re-returned. A *rejected* exit,
/// however, leaves the slot live with `sig_pending` already handed off — so
/// it falls back to `SYS_ENDKSIG` to clear the RTS signal state, keeping the
/// dispose-exactly-once invariant even when teardown can't proceed.
#[cfg_attr(test, allow(dead_code))]
fn drain_ksigs(system: Endpoint) {
    loop {
        let mut m = Message {
            m_source: 0,
            m_type: SYS_GETKSIG,
            payload: [0u8; 96],
        };
        if ipc_sendrec(system, &mut m) != OK || m.m_type != OK {
            return;
        }
        let target_e: Endpoint = rd_i32(&m, 0);
        if target_e == NONE {
            return;
        }

        // Disposition. The signal bitmap (payload 4..8) is not consulted
        // beyond the kernel's guarantee that it is non-empty: with no
        // handlers, masks, or an ignore set in Phase 4, every signal shares
        // the terminate default.
        let action = usize::try_from(endpoint_proc(target_e).get())
            .ok()
            .map(mproc::handle_kill)
            .unwrap_or(mproc::KillAction::NotFound);
        if matches!(action, mproc::KillAction::Terminate) {
            // Happy path: SYS_EXIT tears the target down (signal state zeroed,
            // slot freed) so no acknowledge follows — a post-exit ENDKSIG
            // would just bounce off `okendpt`. But if the exit is *rejected*
            // the slot stays live, and since GETKSIG already handed off
            // `sig_pending` the scan will never re-return it — so fall back to
            // ENDKSIG to clear the RTS signal state. Every drained endpoint is
            // thus disposed of exactly once.
            if sys_exit(system, target_e) != OK {
                sys_endksig(system, target_e);
            }
        } else {
            sys_endksig(system, target_e);
        }
    }
}

/// `SYS_EXIT` — terminate `target_e` (SENDREC to SYSTEM). Returns the
/// kernel-call result so `drain_ksigs` can tell a real teardown from a
/// rejected one.
#[cfg_attr(test, allow(dead_code))]
fn sys_exit(system: Endpoint, target_e: Endpoint) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type: SYS_EXIT,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, 0, target_e);
    let rc = ipc_sendrec(system, &mut m);
    if rc != OK {
        return rc;
    }
    m.m_type
}

/// `SYS_ENDKSIG` — acknowledge `target_e`'s signals as handled.
#[cfg_attr(test, allow(dead_code))]
fn sys_endksig(system: Endpoint, target_e: Endpoint) {
    let mut m = Message {
        m_source: 0,
        m_type: SYS_ENDKSIG,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, 0, target_e);
    let _ = ipc_sendrec(system, &mut m);
}

/// `SYS_PRIVCTL(PRIVCTL_SET_USER)` — point the frozen `target_e` at the
/// shared USER priv slot and release it. Returns the kernel-call result.
#[cfg_attr(test, allow(dead_code))]
fn privctl_set_user(system: Endpoint, target_e: Endpoint) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type: SYS_PRIVCTL,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, 0, target_e);
    wr_i32(&mut m, 4, PRIVCTL_SET_USER);
    let rc = ipc_sendrec(system, &mut m);
    if rc != OK {
        return rc;
    }
    m.m_type
}

/// `SYS_FORK` — kernel-clone the blocked `parent_e` into free slot `child_nr`.
/// Returns `(kernel_result, child_endpoint)`; the endpoint is meaningful only
/// when the result is `OK` (the kernel writes the child's generation-aware
/// endpoint into payload `0..4`).
#[cfg_attr(test, allow(dead_code))]
fn sys_fork(system: Endpoint, parent_e: Endpoint, child_nr: i32) -> (i32, Endpoint) {
    let mut m = Message {
        m_source: 0,
        m_type: SYS_FORK,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, 0, parent_e);
    wr_i32(&mut m, 4, child_nr);
    let rc = ipc_sendrec(system, &mut m);
    if rc != OK {
        return (rc, NONE);
    }
    (m.m_type, rd_i32(&m, 0))
}

/// `SYS_EXEC` — ask the kernel to replace `target_e`'s image with the
/// boot-embedded module `name` (SENDREC to SYSTEM). Payload: target endpoint in
/// `0..4`, the NUL-padded name in `4..4+EXEC_NAME_LEN`, and
/// [`EXEC_SRC_NAME`] in [`EXEC_SRC_OFF`]. Returns the kernel-call result; on `OK`
/// the kernel has already resumed the target at the new entry.
///
/// The selector is written explicitly rather than left zero: a zeroed payload is
/// deliberately *not* a valid form (slice 5.9), so a caller that forgot it gets
/// `EINVAL` instead of whichever source happened to be the default.
#[cfg_attr(test, allow(dead_code))]
fn sys_exec_name(system: Endpoint, target_e: Endpoint, name: &str) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type: SYS_EXEC,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, 0, target_e);
    // The name came out of a `PM_EXEC` payload field, so it may be longer than
    // this one — a path's basename is capped at `EXEC_NAME_LEN` by its caller,
    // but a *module* name is written straight through. Clamped either way:
    // `sys_exec_name` is also called with literals.
    let bytes = name.as_bytes();
    let n = bytes.len().min(EXEC_NAME_LEN);
    m.payload[4..4 + n].copy_from_slice(&bytes[..n]);
    wr_i32(&mut m, EXEC_SRC_OFF, EXEC_SRC_NAME);
    let rc = ipc_sendrec(system, &mut m);
    if rc != OK {
        return rc;
    }
    m.m_type
}

/// `VM_FORK` — ask VM to clone `parent_e`'s region set into `child_e` (SENDREC
/// to VM). Returns VM's reply `m_type`.
#[cfg_attr(test, allow(dead_code))]
fn vm_fork(vm: Endpoint, parent_e: Endpoint, child_e: Endpoint) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type: VM_FORK,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, 0, parent_e);
    wr_i32(&mut m, 4, child_e);
    let rc = ipc_sendrec(vm, &mut m);
    if rc != OK {
        return rc;
    }
    m.m_type
}

/// `SCHEDULING_START` — ask SCHED to begin scheduling `target_e` at the given
/// priority/quantum (SENDREC to SCHED). Returns SCHED's reply `m_type`.
#[cfg_attr(test, allow(dead_code))]
fn sched_start(sched: Endpoint, target_e: Endpoint, priority: i32, quantum: i32) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type: SCHEDULING_START,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, 0, target_e);
    wr_i32(&mut m, 4, priority);
    wr_i32(&mut m, 8, quantum);
    let rc = ipc_sendrec(sched, &mut m);
    if rc != OK {
        return rc;
    }
    m.m_type
}

/// `SCHEDULING_STOP` — hand `target_e` back to the kernel scheduler (SENDREC to
/// SCHED). Called on exit *before* `SYS_EXIT`, while the endpoint is still valid.
#[cfg_attr(test, allow(dead_code))]
fn sched_stop(sched: Endpoint, target_e: Endpoint) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type: SCHEDULING_STOP,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, 0, target_e);
    let rc = ipc_sendrec(sched, &mut m);
    if rc != OK {
        return rc;
    }
    m.m_type
}

/// Reply to a SENDREC caller: stamp `m_type`, zero `m_source` (the kernel
/// overwrites it on delivery), and SEND the message back. Any payload the
/// caller wants returned must be written before this call.
#[cfg_attr(test, allow(dead_code))]
fn reply(target_e: Endpoint, msg: &mut Message, m_type: i32) {
    msg.m_type = m_type;
    msg.m_source = 0;
    let _ = ipc_send(target_e, msg);
}

// The freestanding panic handler; under `cargo test` std supplies its own.
#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop()
    }
}
