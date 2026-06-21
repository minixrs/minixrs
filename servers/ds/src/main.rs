// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs DS (Data Store) server.
//!
//! The system's name directory (slice 4.2). Every server publishes its own
//! endpoint under a key — its NUL-padded name — at init via `DS_PUBLISH`, so
//! others can discover each other by name instead of hard-coding boot proc
//! numbers. `DS_RETRIEVE` looks a name up; `DS_CHECK` tests for presence.
//!
//! Built as a freestanding aarch64 ELF (see `servers/ds/user.ld`), packed into
//! the kernel's boot-image archive by `kernel/build.rs`, and loaded into its
//! own per-process AddrSpace at boot by `arch::aarch64::userland::load_boot_server`.
//!
//! Like the VM server, DS drives its loop through the SEF framework
//! ([`minixrs_server_rt`]): `sef_startup` performs the `GET_WHOAMI` handshake and
//! `sef.receive` filters SEF control messages, handing back only application
//! requests. The store itself lives in [`registry`], whose pure logic carries the
//! host unit tests.

// Freestanding for the real (bare-metal) build, but a normal host binary under
// `cargo test` so `registry`'s logic gets host-runnable unit tests. The test
// harness needs `std` and its own entry point, so `no_std`/`no_main` and the
// `_start` shim below are all gated to `not(test)`.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

mod registry;

use minixrs_ipc::ipc_send;
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{DS_CHECK, DS_PUBLISH, DS_RETRIEVE, SYS_GETINFO_NAME_LEN};
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{ESRCH, OK};
use minixrs_server_rt::{SefConfig, sef_startup};

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
    // `sef_startup` learns DS's endpoint/name via SYS_GETINFO(GET_WHOAMI) and
    // runs `ds_init`, which seeds DS's own entry. DS must NOT publish via the
    // shared `sef_publish_to_ds` helper — a SENDREC to itself would deadlock —
    // so it writes its own binding directly in `ds_init`. No signal handling.
    // If startup fails there is no recovery and nothing to print from EL0.
    let sef = sef_startup(SefConfig {
        init_fresh: Some(ds_init),
        signal_handler: None,
    })
    .unwrap_or_else(|_| {
        loop {
            core::hint::spin_loop()
        }
    });

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
            DS_PUBLISH => handle_publish(&mut msg),
            DS_RETRIEVE => handle_retrieve(&mut msg),
            DS_CHECK => handle_check(&mut msg),
            // Unknown request: drop it (no reply — a bad SENDREC just times out
            // on the caller, which has no client in this slice anyway).
            _ => {}
        }
    }
}

/// SEF fresh-init callback: seed DS's own name→endpoint binding directly.
///
/// DS cannot use [`minixrs_server_rt::sef_publish_to_ds`] like every other
/// server — that SENDRECs to DS, and DS SENDRECing to itself before it reaches
/// its receive loop would deadlock. Writing the binding in-process is both
/// correct and cheaper. Returns the registry result so a (theoretical) failure
/// aborts startup, matching the other servers' init contract.
#[cfg_attr(test, allow(dead_code))]
fn ds_init(endpoint: Endpoint, name: &[u8; SYS_GETINFO_NAME_LEN]) -> i32 {
    registry::publish(name, endpoint)
}

/// Handle `DS_PUBLISH`: bind the key (payload `0..NAME_LEN`) to the endpoint
/// (payload `16..20`). Reply to the SENDREC caller with the registry result.
#[cfg_attr(test, allow(dead_code))]
fn handle_publish(msg: &mut Message) {
    let caller_e = msg.m_source;
    let key = rd_key(msg);
    let ep = rd_i32(msg, 16);

    msg.m_type = registry::publish(&key, ep);
    msg.m_source = 0; // kernel overwrites on delivery
    let _ = ipc_send(caller_e, msg);
}

/// Handle `DS_RETRIEVE`: look up the key (payload `0..NAME_LEN`). Reply
/// `m_type = OK` with the endpoint in payload `16..20`, or `ESRCH` if absent.
#[cfg_attr(test, allow(dead_code))]
fn handle_retrieve(msg: &mut Message) {
    let caller_e = msg.m_source;
    let key = rd_key(msg);

    let reply_type = match registry::retrieve(&key) {
        Some(ep) => {
            wr_i32(msg, 16, ep);
            OK
        }
        None => ESRCH,
    };
    msg.m_type = reply_type;
    msg.m_source = 0; // kernel overwrites on delivery
    let _ = ipc_send(caller_e, msg);
}

/// Handle `DS_CHECK`: test the key (payload `0..NAME_LEN`). Reply `m_type = OK`
/// with a status in payload `16..20` (1 = present, 0 = absent) — absence is a
/// status, not an error, so a CHECK never aborts the caller's SENDREC.
#[cfg_attr(test, allow(dead_code))]
fn handle_check(msg: &mut Message) {
    let caller_e = msg.m_source;
    let key = rd_key(msg);
    let present = registry::check(&key);

    wr_i32(msg, 16, present as i32);
    msg.m_type = OK;
    msg.m_source = 0; // kernel overwrites on delivery
    let _ = ipc_send(caller_e, msg);
}

/// Read the NUL-padded key from payload `0..SYS_GETINFO_NAME_LEN`.
#[cfg_attr(test, allow(dead_code))]
fn rd_key(m: &Message) -> [u8; SYS_GETINFO_NAME_LEN] {
    let mut k = [0u8; SYS_GETINFO_NAME_LEN];
    k.copy_from_slice(&m.payload[0..SYS_GETINFO_NAME_LEN]);
    k
}

// Native-endian payload accessors, mirroring the VM server's helpers.
#[cfg_attr(test, allow(dead_code))]
fn rd_i32(m: &Message, off: usize) -> i32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&m.payload[off..off + 4]);
    i32::from_ne_bytes(b)
}

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
