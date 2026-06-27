// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs SCHED (scheduler) server.
//!
//! The user-space scheduler (slice 4.3). minix.rs keeps a priority-banded run
//! queue in the kernel but makes it *delegatable*: a proc whose
//! `Proc::scheduler` names SCHED is handed to SCHED on quantum exhaustion. The
//! kernel sends `SCHEDULING_NO_QUANTUM` (`m_source` = the preempted proc) and
//! leaves the proc off the run queue; SCHED runs its policy and re-admits the
//! proc via `SYS_SCHEDULE`. This mirrors MINIX 3's distinctive design while
//! keeping the proven kernel run queue as the kernel-scheduled fallback.
//!
//! Requests handled:
//! - `SCHEDULING_NO_QUANTUM` (kernel → SCHED): re-admit the preempted proc.
//! - `SCHEDULING_START` / `SCHEDULING_STOP` / `SCHEDULING_SET_NICE` (PM/RS →
//!   SCHED): claim / release / renice a managed proc. Implemented here for PM/RS
//!   to drive from slice 4.5+; in 4.3 the kernel pre-delegates a busy stub
//!   directly, so only the `NO_QUANTUM` round-trip is exercised live.
//!
//! Built as a freestanding aarch64 ELF (`servers/sched/user.ld`), packed into
//! the boot-image archive by `kernel/build.rs`, and loaded into its own
//! per-process AddrSpace at boot. Like every server it drives its loop through
//! the SEF framework ([`minixrs_server_rt`]); the policy lives in [`policy`],
//! whose pure logic carries the host unit tests.

// Freestanding for the real (bare-metal) build, but a normal host binary under
// `cargo test` so `policy`'s logic gets host-runnable unit tests. The test
// harness needs `std` and its own entry point, so `no_std`/`no_main` and the
// `_start` shim below are all gated to `not(test)`.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

mod policy;

use minixrs_ipc::{ipc_send, ipc_sendrec};
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    SCHEDCTL_FLAG_KERNEL, SCHEDULING_NO_QUANTUM, SCHEDULING_SET_NICE, SCHEDULING_START,
    SCHEDULING_STOP, SYS_GETINFO_NAME_LEN, SYS_SCHEDCTL, SYS_SCHEDULE,
};
use minixrs_kernel_shared::com::{SYSTEM, boot_endpoint};
use minixrs_kernel_shared::endpoint::{Endpoint, endpoint_proc};
use minixrs_kernel_shared::error::OK;
use minixrs_server_rt::{SefConfig, sef_publish_to_ds, sef_startup};

/// ELF entry point. The kernel primes `SP_EL0` before `eret`, so `_start` can
/// dive straight into Rust without setting up a stack itself.
// Gate the whole shim to `not(test)`: under `cargo test` the crate links as a
// normal host executable, and an exported `_start` would collide with the C
// runtime's `_start`. `.text._start` is an ELF section name, gated to the
// bare-metal target so `cargo check --workspace` on a Mach-O host still
// type-checks (the host rejects the specifier).
#[cfg(not(test))]
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "none", unsafe(link_section = ".text._start"))]
pub extern "C" fn _start() -> ! {
    main()
}

// Only `_start` calls `main`; under `cargo test` `_start` is gone, so `main`
// (and the helpers it alone reaches) would read as dead code.
#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    // `sef_startup` learns SCHED's endpoint/name via SYS_GETINFO(GET_WHOAMI) and
    // runs `sched_init`, which publishes SCHED's endpoint to DS. No signal
    // handling. If startup fails there is no recovery and nothing to print from
    // EL0 — park forever.
    let sef = sef_startup(SefConfig {
        init_fresh: Some(sched_init),
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
            // Kernel → SCHED: re-admit the preempted proc (one-way, no reply).
            SCHEDULING_NO_QUANTUM => handle_no_quantum(system, msg.m_source),
            // PM/RS → SCHED requests (SENDREC; reply to the caller).
            SCHEDULING_START => handle_start(system, &mut msg),
            SCHEDULING_STOP => handle_stop(system, &mut msg),
            SCHEDULING_SET_NICE => handle_set_nice(system, &mut msg),
            // Unknown request: drop it.
            _ => {}
        }
    }
}

/// SEF fresh-init callback: publish SCHED's endpoint to DS under its name, so
/// PM/RS can look SCHED up by name (slice 4.2). DS registers the caller's
/// kernel-stamped endpoint, so the `GET_WHOAMI` endpoint is not sent.
#[cfg_attr(test, allow(dead_code))]
fn sched_init(_endpoint: Endpoint, name: &[u8; SYS_GETINFO_NAME_LEN]) -> i32 {
    sef_publish_to_ds(name)
}

/// `SCHEDULING_NO_QUANTUM` (kernel → SCHED): the proc identified by `preempted_e`
/// used its full quantum and is blocked off the run queue. Look up (lazily
/// registering) its policy state and re-admit it via `SYS_SCHEDULE`. No reply —
/// the kernel-originated notification is one-way; `SYS_SCHEDULE` is what unblocks
/// the proc.
#[cfg_attr(test, allow(dead_code))]
fn handle_no_quantum(system: Endpoint, preempted_e: Endpoint) {
    let proc_nr = endpoint_proc(preempted_e).get();
    // Table full (≥16 managed procs): can't manage this one — leave it blocked.
    // With a single pre-delegated stub in 4.3 this never happens.
    if let Some((priority, quantum)) = policy::schedule_proc(proc_nr) {
        sys_schedule(system, preempted_e, priority, quantum);
    }
}

/// `SCHEDULING_START` (PM/RS → SCHED): begin scheduling `target` at the given
/// priority/quantum. Claim it via `SYS_SCHEDCTL`, record its policy state, and
/// assign the initial priority/quantum via `SYS_SCHEDULE`. Reply `OK` to the
/// caller. (Driven by PM/RS from slice 4.5+.)
#[cfg_attr(test, allow(dead_code))]
fn handle_start(system: Endpoint, msg: &mut Message) {
    let caller_e = msg.m_source;
    let target_e: Endpoint = rd_i32(msg, 0);
    let priority = rd_i32(msg, 4);
    let quantum = rd_i32(msg, 8);

    sys_schedctl(system, target_e, 0); // claim: scheduler = SCHED
    policy::record(endpoint_proc(target_e).get(), priority as u8, quantum);
    sys_schedule(system, target_e, priority as u8, quantum);

    reply(caller_e, msg, OK);
}

/// `SCHEDULING_STOP` (PM/RS → SCHED): stop scheduling `target` and hand it back
/// to the kernel scheduler. Reply `OK` to the caller.
#[cfg_attr(test, allow(dead_code))]
fn handle_stop(system: Endpoint, msg: &mut Message) {
    let caller_e = msg.m_source;
    let target_e: Endpoint = rd_i32(msg, 0);

    sys_schedctl(system, target_e, SCHEDCTL_FLAG_KERNEL); // release
    policy::forget(endpoint_proc(target_e).get());

    reply(caller_e, msg, OK);
}

/// `SCHEDULING_SET_NICE` (PM/RS → SCHED): change `target`'s priority. Record the
/// new band and apply it via `SYS_SCHEDULE`. Reply `OK` to the caller.
#[cfg_attr(test, allow(dead_code))]
fn handle_set_nice(system: Endpoint, msg: &mut Message) {
    let caller_e = msg.m_source;
    let target_e: Endpoint = rd_i32(msg, 0);
    let priority = rd_i32(msg, 4);

    policy::record(
        endpoint_proc(target_e).get(),
        priority as u8,
        policy::QUANTUM,
    );
    sys_schedule(system, target_e, priority as u8, policy::QUANTUM);

    reply(caller_e, msg, OK);
}

/// Issue `SYS_SCHEDULE(target, priority, quantum)` to the kernel (SENDREC to
/// SYSTEM). The kernel sets the target's band + quantum and (re-)admits it to
/// the run queue. The reply code is ignored — a failure leaves the proc blocked,
/// which is the same outcome as not scheduling it.
#[cfg_attr(test, allow(dead_code))]
fn sys_schedule(system: Endpoint, target: Endpoint, priority: u8, quantum: i32) {
    let mut m = Message {
        m_source: 0,
        m_type: SYS_SCHEDULE,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, 0, target);
    wr_i32(&mut m, 4, priority as i32);
    wr_i32(&mut m, 8, quantum);
    let _ = ipc_sendrec(system, &mut m);
}

/// Issue `SYS_SCHEDCTL(flags, target)` to the kernel (SENDREC to SYSTEM):
/// `flags = 0` claims the target for SCHED; `SCHEDCTL_FLAG_KERNEL` releases it.
#[cfg_attr(test, allow(dead_code))]
fn sys_schedctl(system: Endpoint, target: Endpoint, flags: i32) {
    let mut m = Message {
        m_source: 0,
        m_type: SYS_SCHEDCTL,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, 0, flags);
    wr_i32(&mut m, 4, target);
    let _ = ipc_sendrec(system, &mut m);
}

/// Reply to a SENDREC caller with `code` in `m_type`.
#[cfg_attr(test, allow(dead_code))]
fn reply(caller_e: Endpoint, msg: &mut Message, code: i32) {
    msg.m_type = code;
    msg.m_source = 0; // kernel overwrites on delivery
    let _ = ipc_send(caller_e, msg);
}

/// Read a native-endian i32 from payload `off..off+4` (mirrors the VM/DS helpers).
#[cfg_attr(test, allow(dead_code))]
fn rd_i32(m: &Message, off: usize) -> i32 {
    i32::from_ne_bytes(
        m.payload[off..off + 4]
            .try_into()
            .expect("payload in range"),
    )
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
