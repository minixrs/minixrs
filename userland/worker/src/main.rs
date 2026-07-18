// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs `worker` — the slice-4.7 exec target.
//!
//! A tiny freestanding EL0 program that is *not* a boot server: it is packed
//! into the boot-image archive only so `SYS_EXEC` can resolve it by name, and it
//! is never loaded at boot (`kernel/build.rs` tags its MXBI record with
//! `com::EXEC_ONLY_PROC_NR`). PM's `handle_exec` selects it; the kernel loads it
//! into a forked child's fresh address space, replacing the child's image.
//!
//! Since EL0 has no console, the worker proves it ran through observable IPC: a
//! few `PM_GETPID` round-trips (visible as `[ipc] caller=<child nr> target=0x0`,
//! returning the child's preserved pid) followed by `PM_EXIT(0)`, which tears it
//! down so the parent's `wait()` reaps it and the fork loop recycles the slot.
//!
//! Built as a freestanding aarch64 ELF (`userland/worker/user.ld`). It uses
//! `minix-ipc` directly — no `server-rt`/SEF, because it is a plain user
//! program, not a server. The `_start` shim and panic handler are gated to
//! `not(test)`.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

use minixrs_ipc::ipc_sendrec;
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{PM_EXIT, PM_GETPID};
use minixrs_kernel_shared::com::{PM_PROC_NR, boot_endpoint};

/// Number of observable `PM_GETPID` round-trips before exiting — enough to make
/// the worker's activity unmistakable in the boot trace without flooding it.
const GETPID_ROUNDS: usize = 3;

/// ELF entry point. Exec primes `SP_EL0` before `eret`, so `_start` can dive
/// straight into Rust without setting up a stack itself.
#[cfg(not(test))]
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "none", unsafe(link_section = ".text._start"))]
pub extern "C" fn _start() -> ! {
    main()
}

#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    let pm = boot_endpoint(PM_PROC_NR);

    // Prove the new image is running: a few getpid round-trips. PM replies with
    // this proc's preserved pid (exec keeps the fork child's identity).
    for _ in 0..GETPID_ROUNDS {
        let mut msg = Message {
            m_source: 0,
            m_type: PM_GETPID,
            payload: [0u8; 96],
        };
        let _ = ipc_sendrec(pm, &mut msg);
    }

    // exit(0): status 0 sits in the zeroed payload `0..4`. PM tears us down via
    // SYS_EXIT rather than replying, so this SENDREC never returns.
    let mut msg = Message {
        m_source: 0,
        m_type: PM_EXIT,
        payload: [0u8; 96],
    };
    let _ = ipc_sendrec(pm, &mut msg);

    // Unreachable: PM never replies to a dead child.
    loop {
        core::hint::spin_loop()
    }
}

// The freestanding panic handler; under `cargo test` std supplies its own.
#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop()
    }
}
