// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs VFS (virtual file system) server — skeletal boot (slice 4.2).
//!
//! This slice stands VFS up as a real boot server: it boots through the SEF
//! framework and publishes its endpoint to DS, proving the multi-server boot
//! path and the DS registry end to end. It does *no* file operations yet — the
//! PM↔VFS fork/exec work protocol needs file descriptors and is Phase 5 — so the
//! receive loop simply drops any application traffic that arrives.
//!
//! Built as a freestanding aarch64 ELF (see `servers/vfs/user.ld`), packed into
//! the kernel's boot-image archive by `kernel/build.rs`, and loaded into its own
//! per-process AddrSpace at boot by `arch::aarch64::userland::load_boot_server`.

// Freestanding for the real (bare-metal) build, but a normal host binary under
// `cargo test` (no host tests yet — the SEF/IPC glue is QEMU-verified). The
// `_start` shim and panic handler are gated to `not(test)`.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::SYS_GETINFO_NAME_LEN;
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_server_rt::{SefConfig, sef_publish_to_ds, sef_startup};

/// ELF entry point. The kernel primes `SP_EL0` before `eret`, so `_start` can
/// dive straight into Rust without setting up a stack itself.
#[cfg(not(test))]
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "none", unsafe(link_section = ".text._start"))]
pub extern "C" fn _start() -> ! {
    main()
}

#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    // `sef_startup` learns VFS's endpoint/name and runs `vfs_init`, which
    // publishes VFS's endpoint to DS. The publish SENDREC blocks until DS is in
    // its receive loop — safe at boot (DS's init does no IPC). No signal
    // handling. On startup failure there is no recovery and nothing to print.
    let sef = sef_startup(SefConfig {
        init_fresh: Some(vfs_init),
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
        // No file ops yet (Phase 5): receive and drop application traffic. The
        // SEF framework still services control traffic (pings/signals/re-init)
        // inside `receive`; only the application messages it hands back are
        // discarded here.
        let _ = sef.receive(&mut msg);
    }
}

/// SEF fresh-init callback: publish VFS's endpoint to DS under its name.
#[cfg_attr(test, allow(dead_code))]
fn vfs_init(endpoint: Endpoint, name: &[u8; SYS_GETINFO_NAME_LEN]) -> i32 {
    sef_publish_to_ds(endpoint, name)
}

// The freestanding panic handler; under `cargo test` std supplies its own.
#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop()
    }
}
