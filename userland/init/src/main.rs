// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs `init` — PID 1, the first user process (slice 4.8).
//!
//! Unlike the demo stubs it replaces, `init` is a real boot module: it is packed
//! into the boot-image archive with its true proc number (`INIT_PROC_NR = 10`)
//! and loaded + made runnable by the ordinary boot loop
//! (`kernel/src/arch/aarch64/userland.rs`), not hand-released by PM. It runs as
//! an ordinary user process — the shared `USER_PRIV_ID` privilege (SENDREC to PM
//! only, no kernel calls) — so the whole process lifecycle flows through PM in
//! the POSIX shape (user → PM, never user → kernel).
//!
//! `init` is the live exercise for the Phase-4 process machinery that the
//! slice-4.6/4.7 stub E demonstrated: it forks a child, the child execs the
//! `worker` binary (which runs a few `PM_GETPID` round-trips then exits), and
//! the parent `wait`s
//! to reap the zombie before looping to fork again. Each cycle recycles the same
//! fork-pool slot with an advancing endpoint generation — observable in the boot
//! trace as `SYS_FORK` / `SYS_EXEC` / `SYS_EXIT` triples.
//!
//! Built as a freestanding aarch64 ELF (`userland/init/user.ld`). It uses
//! `minix-ipc` directly — no `server-rt`/SEF, because it is a plain user program,
//! not a server. The `_start` shim and panic handler are gated to `not(test)`.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

use minixrs_ipc::ipc_sendrec;
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{PM_EXEC, PM_FORK, PM_WAIT};
use minixrs_kernel_shared::com::{PM_PROC_NR, boot_endpoint};

/// The boot loader primes `SP_EL0` before `eret`, so `_start` can dive straight
/// into Rust without setting up a stack itself.
#[cfg(not(test))]
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "none", unsafe(link_section = ".text._start"))]
pub extern "C" fn _start() -> ! {
    main()
}

/// Build a request message to PM: no payload, `m_source` is stamped by the kernel.
#[cfg_attr(test, allow(dead_code))]
fn pm_msg(m_type: i32) -> Message {
    Message {
        m_source: 0,
        m_type,
        payload: [0u8; 96],
    }
}

#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    let pm = boot_endpoint(PM_PROC_NR);

    loop {
        // fork(): PM replies to both halves of this SENDREC — the child sees
        // `m_type == 0`, the parent sees the child pid (`> 0`); a negative value
        // is an errno (e.g. `EAGAIN` when the fork table is full).
        let mut m = pm_msg(PM_FORK);
        let _ = ipc_sendrec(pm, &mut m);

        match m.m_type {
            0 => {
                // Child: replace this image with the `worker` binary. PM issues
                // `SYS_EXEC` and the kernel resumes us at worker's `_start`, so
                // this SENDREC never returns on success.
                let mut e = pm_msg(PM_EXEC);
                let _ = ipc_sendrec(pm, &mut e);
                // Unreachable on success; park defensively if exec ever failed.
                loop {
                    core::hint::spin_loop()
                }
            }
            n if n > 0 => {
                // Parent: reap the child (blocks until it exits), then loop to
                // fork the next one.
                let mut w = pm_msg(PM_WAIT);
                let _ = ipc_sendrec(pm, &mut w);
            }
            _ => {
                // Transient fork failure (table full): back off briefly, retry.
                for _ in 0..1024 {
                    core::hint::spin_loop()
                }
            }
        }
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
