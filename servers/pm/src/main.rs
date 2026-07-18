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
//! PM's SEF init also releases stub E — built frozen (`RTS_NO_PRIV`, no priv
//! slot) at boot — via `SYS_PRIVCTL(PRIVCTL_SET_USER)`, standing in for the
//! fork path that will create frozen children in 4.6. E then exercises
//! `PM_GETPID` in a SENDREC loop over the PM ↔ shared-USER-priv edge opened
//! by `populate_user_priv`.

// Freestanding for the bare-metal build, but a normal host binary under
// `cargo test` so `mproc`'s logic gets host-runnable unit tests. Same gating
// as the RS/SCHED servers.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

mod mproc;

use minixrs_ipc::{ipc_send, ipc_sendrec};
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    PM_EXIT, PM_FORK, PM_GETPID, PM_WAIT, PRIVCTL_SET_USER, SCHEDULING_START, SCHEDULING_STOP,
    SYS_ENDKSIG, SYS_EXIT, SYS_FORK, SYS_GETINFO_NAME_LEN, SYS_GETKSIG, SYS_PRIVCTL, VM_FORK,
};
use minixrs_kernel_shared::com::{
    SCHED_PROC_NR, STUB_E_PROC_NR, SYSTEM, VM_PROC_NR, boot_endpoint,
};
use minixrs_kernel_shared::endpoint::{Endpoint, NONE, endpoint_proc};
use minixrs_kernel_shared::error::{EAGAIN, ECHILD, EINVAL, ESRCH, OK};
use minixrs_kernel_shared::ipc_const::NOTIFY_MESSAGE;
use minixrs_server_rt::{SefConfig, sef_publish_to_ds, sef_startup};

/// Priority band and quantum PM assigns a forked child via `SCHEDULING_START`.
/// These match SCHED's own `USER_Q` / `QUANTUM` (`servers/sched/src/policy.rs`)
/// so a child round-robins in the managed band rather than sinking behind the
/// kernel-scheduled stubs.
const CHILD_PRIORITY: i32 = 8;
const CHILD_QUANTUM: i32 = 5;

/// ELF entry point. The kernel primes `SP_EL0` before `eret`, so `_start`
/// dives straight into Rust. Gated to `not(test)` (under `cargo test` the
/// crate links as a host binary with the C runtime's `_start`);
/// `.text._start` is gated to the bare-metal target so a Mach-O host
/// `cargo check` still type-checks.
#[cfg(not(test))]
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "none", unsafe(link_section = ".text._start"))]
pub extern "C" fn _start() -> ! {
    main()
}

#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    // `sef_startup` learns PM's endpoint/name via SYS_GETINFO(GET_WHOAMI) and
    // runs `pm_init` (DS publish, mproc seed, stub E release). No signal
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

    let mut msg = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };
    loop {
        if sef.receive(&mut msg) != 0 {
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
            // Unknown request: drop it.
            _ => {}
        }
    }
}

/// SEF fresh-init callback: publish PM's endpoint to DS, seed the mproc
/// table, and release the frozen stub E onto the shared USER priv slot.
#[cfg_attr(test, allow(dead_code))]
fn pm_init(_endpoint: Endpoint, name: &[u8; SYS_GETINFO_NAME_LEN]) -> i32 {
    let rc = sef_publish_to_ds(name);
    if rc != OK {
        return rc;
    }
    mproc::seed();
    // Stand-in for 4.6's fork path: grant the boot-frozen stub E its USER
    // privilege and let it run. E may SENDREC us before we reach the receive
    // loop — it just parks on our caller queue until then.
    privctl_set_user(boot_endpoint(SYSTEM), boot_endpoint(STUB_E_PROC_NR))
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

    // SCHED schedules the (still-frozen) child; then release the freeze. The
    // child remains RTS_RECEIVING — a blocked receiver, off the run queue —
    // until its reply below clears the last block bit.
    let _ = sched_start(sched, child_e, CHILD_PRIORITY, CHILD_QUANTUM);
    let _ = privctl_set_user(system, child_e);

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

/// Read a native-endian i32 from payload `off..off+4`.
#[cfg_attr(test, allow(dead_code))]
fn rd_i32(m: &Message, off: usize) -> i32 {
    i32::from_ne_bytes(m.payload[off..off + 4].try_into().unwrap_or([0; 4]))
}

/// Write a native-endian i32 into payload `off..off+4`.
#[cfg_attr(test, allow(dead_code))]
fn wr_i32(m: &mut Message, off: usize, v: i32) {
    m.payload[off..off + 4].copy_from_slice(&v.to_ne_bytes());
}

// The freestanding panic handler; under `cargo test` std supplies its own.
#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop()
    }
}
