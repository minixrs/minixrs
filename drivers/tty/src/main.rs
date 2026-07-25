// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs TTY driver — the console, TX only (slice 5.3, decision D1).
//!
//! The first user-space *driver* in minix.rs, and the first EL0-originated text on
//! the serial line. Everything before it either came from the kernel's own PL011
//! writer or rode the `SYS_DIAGCTL` debug channel — which is deliberately a debug
//! channel, not stdio.
//!
//! TTY owns the PL011 and serves [`CDEV_WRITE`]. A client grants it a read-only
//! view of a buffer, sends the grant id, and TTY pulls the bytes across with
//! `SYS_SAFECOPY` before transmitting them. Slice 5.4 puts VFS's fd 1 and 2 on top
//! of this; slice 5.6's musl `printf` rides the same rails.
//!
//! Built as a freestanding aarch64 ELF (`drivers/tty/user.ld`, a byte copy of the
//! servers'), packed into the MXBI archive by `kernel/build.rs`, and loaded into
//! its own address space at boot — plus one extra page the other boot processes do
//! not get: the UART's MMIO registers at `TTY_UART_VA`, pre-mapped by the kernel
//! with Device attributes. See `pl011.rs`.
//!
//! ## Three things that differ from a server
//!
//! **An unknown `m_type` gets a reply, not a drop.** DS silently drops one, which
//! is fine when nothing SENDRECs it in anger — but a driver's clients *all*
//! SENDREC, and a dropped request blocks the caller forever. Every path out of the
//! dispatch below replies.
//!
//! **The granter comes from `m_source`.** TTY holds `SYS_SAFECOPY`; its clients do
//! not. A granter field in the payload would let any client aim a privileged
//! cross-address-space copy at a third party's memory through TTY. `cdev.rs` has no
//! way to express one.
//!
//! **A negative `SYS_SAFECOPY` result is relayed verbatim.** `EPERM` ("your grant
//! does not authorize this") and `EFAULT` ("your buffer is not mapped") are
//! different bugs on the client's side, and collapsing them to one errno would hide
//! which.

// Freestanding for the real (bare-metal) build, but a normal host binary under
// `cargo test` so `cdev`'s logic gets host-runnable unit tests. The test harness
// needs `std` and its own entry point, so `no_std`/`no_main` and the `_start` shim
// below are all gated to `not(test)`.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

mod cdev;
mod pl011;

use minixrs_ipc::ipc_send;
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{CDEV_MAX_IO, CDEV_WRITE, SAFECOPY_FROM, SYS_GETINFO_NAME_LEN};
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{ENOSYS, OK};
use minixrs_server_rt::{SefConfig, buf_addr, sef_publish_to_ds, sef_startup, sys_safecopy};

/// The banner TTY writes straight to the UART once its device mapping is proven
/// usable from EL0.
///
/// This line is the slice's milestone, and it is recognizable in the boot log for a
/// specific reason: it carries **no kernel trace prefix**. Every other line in the
/// log is `[as]`, `[ipc]`, `[ksys …]`, `[diag …]` — kernel-formatted. This one was
/// composed at EL0 and reached the wire through a user-space store to a device
/// register.
const BANNER: &str = "minix.rs console: tty online (EL0)\n";

/// ELF entry point. The kernel primes `SP_EL0` before `eret`, so `_start` can dive
/// straight into Rust without setting up a stack itself.
// Gate the whole shim to `not(test)`: under `cargo test` the crate links as a
// normal host executable, and an exported `_start` would collide with the C
// runtime's. `.text._start` is an ELF section name, gated to the bare-metal target
// so `cargo check --workspace` on a Mach-O host still type-checks.
#[cfg(not(test))]
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "none", unsafe(link_section = ".text._start"))]
pub extern "C" fn _start() -> ! {
    main()
}

// Only `_start` calls `main`; under `cargo test` `_start` is gone, so `main` (and
// the helpers it alone reaches) would read as dead code.
#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    let sef = sef_startup(SefConfig {
        init_fresh: Some(tty_init),
        signal_handler: None,
    })
    .unwrap_or_else(|_| {
        // No recovery and nothing to print: a failed handshake means TTY never
        // learned its own endpoint, so it cannot even announce the failure.
        loop {
            core::hint::spin_loop()
        }
    });

    // The staging buffer lives in `main`'s frame, not in a static and not in
    // `tty_init`'s frame — the same rule `GrantPool` follows, and for the same
    // reason: the kernel writes into it while TTY is blocked inside the
    // `SYS_SAFECOPY` SENDREC, so the frame must outlive every call that names it.
    // One page of stack holds it comfortably at `CDEV_MAX_IO` = 256 bytes.
    let mut staging = [0u8; CDEV_MAX_IO];

    let mut msg = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };
    loop {
        if sef.receive(&mut msg) != OK {
            continue;
        }
        // Capture the caller *first*: it is both the reply target and the granter,
        // and the dispatch below overwrites `msg.m_source` on the way out.
        let caller_e = msg.m_source;
        match msg.m_type {
            CDEV_WRITE => {
                let rc = do_write(caller_e, &msg, &mut staging);
                reply(caller_e, &mut msg, rc);
            }
            // Unlike DS, reply rather than drop — the caller is inside a SENDREC
            // and would otherwise block forever.
            _ => reply(caller_e, &mut msg, ENOSYS),
        }
    }
}

/// SEF fresh-init callback: publish to DS, then prove the device mapping.
///
/// Order matters for the boot log. `sef_startup` has already emitted
/// `[diag tty] sef ready` through the kernel debug channel, so by the time the
/// banner appears the two independent output paths — debug channel and real
/// console — have each been exercised once, and a failure in either is
/// distinguishable from a failure in the other.
///
/// If the *device mapping* were broken this never returns: the store faults, and
/// TTY has no `ipc_to` edge to VM's fault handler for a device VA, so the process
/// dies on an EL0 data abort that `tests/qemu-boot.forbidden` catches.
#[cfg_attr(test, allow(dead_code))]
fn tty_init(_endpoint: Endpoint, name: &[u8; SYS_GETINFO_NAME_LEN]) -> i32 {
    let rc = sef_publish_to_ds(name);
    if rc != OK {
        return rc;
    }
    pl011::write(BANNER.as_bytes());
    OK
}

/// Serve one `CDEV_WRITE`. Returns the reply `m_type`: the byte count written
/// (`>= 0`), or a negative errno.
///
/// `staging` is the caller's buffer, passed in rather than declared here so it
/// lives in `main`'s frame (see the comment at its declaration).
#[cfg_attr(test, allow(dead_code))]
fn do_write(caller_e: Endpoint, msg: &Message, staging: &mut [u8; CDEV_MAX_IO]) -> i32 {
    let req = cdev::parse_write(msg);
    let n = match cdev::validate_write(req) {
        Ok(n) => n,
        Err(e) => return e,
    };
    if n == 0 {
        // A legal zero-length write. No grant is read and nothing is transmitted,
        // so a client polling with `len = 0` cannot use it to probe a grant.
        return 0;
    }

    // Pull the bytes across from the client's granted buffer. `caller_e` is the
    // kernel-stamped source of this very message — the granter can be nothing else.
    let rc = sys_safecopy(
        SAFECOPY_FROM,
        caller_e,
        req.gid,
        req.offset,
        buf_addr(&mut staging[..n]),
        n as u64,
    );
    if rc != OK {
        // Verbatim: EPERM and EFAULT tell the client different things.
        return rc;
    }

    pl011::write(&staging[..n]);
    // The count actually moved, which is `CDEV_MAX_IO` for an over-long request.
    // Replying `OK` here instead would tell a client its whole buffer went out.
    n as i32
}

/// Reply to a SENDREC caller: stamp `m_type`, zero `m_source` (the kernel
/// overwrites it on delivery), and SEND the message back.
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
