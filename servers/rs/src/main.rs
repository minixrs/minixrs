// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs RS (reincarnation server).
//!
//! RS is the *root system process* — the parent/monitor of every system service
//! (`ROOT_SYS_PROC`, and the `sig_mgr` for all). Slice 4.4 stands up its
//! heartbeat loop: RS boots through SEF, publishes its endpoint to DS, arms a
//! periodic kernel alarm via `SYS_SETALARM`, and on each alarm pings the boot
//! servers and tallies which ones answered. Restart-on-crash is **minimal** in
//! Phase 4 — RS *detects* an unresponsive peer (the `monitor` accounting) but,
//! running at EL0 with no console and no exec yet, takes no live action; full
//! reload via the boot archive is a later slice.
//!
//! ## Heartbeat protocol (all observable kernel-side; EL0 cannot print)
//! - **alarm** = a `NOTIFY` from `CLOCK` (kernel-originated on `SYS_SETALARM`
//!   expiry). On each one RS sweeps the previous round, re-pings every peer, and
//!   re-arms the one-shot alarm (so it is effectively periodic).
//! - **ping** = `ipc_notify(peer)`. The peer's SEF runtime classifies a `NOTIFY`
//!   from RS as a ping and acks with `ipc_notify(RS)`.
//! - **ack** = a `NOTIFY` from a monitored peer. RS records it ([`monitor::mark_alive`]).
//!
//! Built as a freestanding aarch64 ELF (`servers/rs/user.ld`), packed into the
//! boot-image archive by `kernel/build.rs`, and loaded into its `RS_PROC_NR`
//! slot at boot. RS already has its priv slot fully wired by `init_boot_image`
//! (`SRV_T` trap mask + `ipc_to` over all boot servers + `k_call_mask` covering
//! `SYS_SETALARM`), so it needs no per-server priv code. The liveness accounting
//! lives in [`monitor`], whose pure logic carries the host unit tests.

// Freestanding for the bare-metal build, but a normal host binary under
// `cargo test` so `monitor`'s logic gets host-runnable unit tests. Same gating
// as the SCHED server.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

mod monitor;

use minixrs_ipc::{ipc_notify, ipc_sendrec};
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{SYS_GETINFO_NAME_LEN, SYS_SETALARM};
use minixrs_kernel_shared::com::{
    CLOCK, DS_PROC_NR, PM_PROC_NR, SCHED_PROC_NR, SYSTEM, VFS_PROC_NR, VM_PROC_NR, boot_endpoint,
};
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::ipc_const::NOTIFY_MESSAGE;
use minixrs_server_rt::{SefConfig, sef_publish_to_ds, sef_startup};

/// Boot servers RS heartbeats. Boot endpoints (generation 0) are valid for the
/// whole kernel lifetime — boot procs never exit in Phase 4 — so a static list
/// suffices; DS-driven discovery is a later enhancement.
const PEERS: [Endpoint; 5] = [
    boot_endpoint(DS_PROC_NR),
    boot_endpoint(VM_PROC_NR),
    boot_endpoint(SCHED_PROC_NR),
    boot_endpoint(VFS_PROC_NR),
    boot_endpoint(PM_PROC_NR),
];

/// Heartbeat period, in clock ticks. At 100 Hz this is ≈1 s, giving ~8–10 fires
/// in an 8 s QEMU run — frequent enough to observe, sparse enough not to flood.
const ALARM_PERIOD: u64 = 100;

/// ELF entry point. The kernel primes `SP_EL0` before `eret`, so `_start` dives
/// straight into Rust. Gated to `not(test)` (under `cargo test` the crate links
/// as a host binary with the C runtime's `_start`); `.text._start` is gated to
/// the bare-metal target so a Mach-O host `cargo check` still type-checks.
#[cfg(not(test))]
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "none", unsafe(link_section = ".text._start"))]
pub extern "C" fn _start() -> ! {
    main()
}

#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    // `sef_startup` learns RS's endpoint/name via SYS_GETINFO(GET_WHOAMI) and
    // runs `rs_init`, which publishes RS's endpoint to DS. No signal handling
    // yet (PM drives signals from a later slice). On failure there is nothing to
    // print from EL0 — park forever.
    let sef = sef_startup(SefConfig {
        init_fresh: Some(rs_init),
        signal_handler: None,
    })
    .unwrap_or_else(|_| {
        loop {
            core::hint::spin_loop()
        }
    });

    let system = boot_endpoint(SYSTEM);
    let clock = boot_endpoint(CLOCK);

    // Begin monitoring: record the peers, send the first heartbeat round, and arm
    // the first alarm. Pinging here means the first alarm's sweep evaluates real
    // acks rather than a spurious all-missed round.
    monitor::init(&PEERS);
    ping_all();
    sys_setalarm(system, ALARM_PERIOD);

    let mut msg = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };
    loop {
        if sef.receive(&mut msg) != 0 {
            continue;
        }
        // RS's traffic is all NOTIFY: the alarm from CLOCK, and heartbeat acks
        // from peers. The source is the discriminator.
        if msg.m_type == NOTIFY_MESSAGE {
            if msg.m_source == clock {
                handle_alarm(system);
            } else {
                // A heartbeat ack (or unrelated NOTIFY — mark_alive ignores it).
                let _ = monitor::mark_alive(msg.m_source);
            }
        }
        // Any other message: drop (RS has no request handlers in slice 4.4).
    }
}

/// SEF fresh-init callback: publish RS's endpoint to DS under its name so peers
/// can look RS up. DS registers the caller's kernel-stamped endpoint.
#[cfg_attr(test, allow(dead_code))]
fn rs_init(_endpoint: Endpoint, name: &[u8; SYS_GETINFO_NAME_LEN]) -> i32 {
    sef_publish_to_ds(name)
}

/// Alarm tick: close out the previous heartbeat round, re-ping every peer, and
/// re-arm the one-shot alarm. The dead-peer count is detect-only in Phase 4 (RS
/// cannot log from EL0 and has no restart path yet); the periodic fire itself is
/// the observable proof, traced kernel-side as `[alarm N]`.
#[cfg_attr(test, allow(dead_code))]
fn handle_alarm(system: Endpoint) {
    let _dead = monitor::sweep();
    ping_all();
    sys_setalarm(system, ALARM_PERIOD);
}

/// Send one heartbeat (`NOTIFY`) to every monitored peer. Non-blocking; a peer
/// that is not currently receiving gets the notification deferred and acks once
/// it next receives.
#[cfg_attr(test, allow(dead_code))]
fn ping_all() {
    for &peer in PEERS.iter() {
        let _ = ipc_notify(peer);
    }
}

/// Arm the caller's one-shot alarm `delta` ticks out via `SYS_SETALARM` (SENDREC
/// to SYSTEM). The reply carries the previous timer's remaining ticks, which RS
/// does not use.
#[cfg_attr(test, allow(dead_code))]
fn sys_setalarm(system: Endpoint, delta: u64) {
    let mut m = Message {
        m_source: 0,
        m_type: SYS_SETALARM,
        payload: [0u8; 96],
    };
    m.payload[0..8].copy_from_slice(&delta.to_ne_bytes());
    let _ = ipc_sendrec(system, &mut m);
}

// The freestanding panic handler; under `cargo test` std supplies its own.
#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop()
    }
}
