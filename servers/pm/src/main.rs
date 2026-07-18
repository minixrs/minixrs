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
    PM_GETPID, PRIVCTL_SET_USER, SYS_ENDKSIG, SYS_EXIT, SYS_GETINFO_NAME_LEN, SYS_GETKSIG,
    SYS_PRIVCTL,
};
use minixrs_kernel_shared::com::{STUB_E_PROC_NR, SYSTEM, boot_endpoint};
use minixrs_kernel_shared::endpoint::{Endpoint, NONE, endpoint_proc};
use minixrs_kernel_shared::error::{ESRCH, OK};
use minixrs_kernel_shared::ipc_const::NOTIFY_MESSAGE;
use minixrs_server_rt::{SefConfig, sef_publish_to_ds, sef_startup};

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
    let reply = match usize::try_from(endpoint_proc(caller_e).get())
        .ok()
        .and_then(mproc::getpid)
    {
        Some((pid, ppid)) => {
            wr_i32(msg, 0, ppid);
            pid
        }
        None => ESRCH,
    };
    msg.m_type = reply;
    msg.m_source = 0; // kernel overwrites on delivery
    let _ = ipc_send(caller_e, msg);
}

/// Drain the kernel's pending-signal queue: `SYS_GETKSIG` until it reports
/// `NONE`, disposing of each signalled proc. Default disposition is
/// terminate (`SYS_EXIT`) for user processes; boot servers are skipped.
///
/// Contract with the kernel (see `system::do_sig`): every endpoint GETKSIG
/// returns must be either terminated or ENDKSIG-acknowledged, or it stays
/// signal-pending forever. Terminated targets get **no** `SYS_ENDKSIG`: as of
/// slice 4.6 `SYS_EXIT` is a full teardown — signal state zeroed, slot freed,
/// endpoint generation bumped — so a post-exit acknowledge would only bounce
/// off `okendpt` with `EDEADSRCDST`. (MINIX can acknowledge after terminate
/// because its `sys_clear` is deferred behind VFS; minix.rs tears down
/// immediately.) GETKSIG's scan gates on `sig_pending != 0`, which the exit
/// zeroed, so the dead proc is never re-returned.
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
            sys_exit(system, target_e);
        } else {
            sys_endksig(system, target_e);
        }
    }
}

/// `SYS_EXIT` — terminate `target_e` (SENDREC to SYSTEM).
#[cfg_attr(test, allow(dead_code))]
fn sys_exit(system: Endpoint, target_e: Endpoint) {
    let mut m = Message {
        m_source: 0,
        m_type: SYS_EXIT,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, 0, target_e);
    let _ = ipc_sendrec(system, &mut m);
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
