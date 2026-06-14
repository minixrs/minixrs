// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! User-space IPC library.
//!
//! Thin wrappers over the kernel's SVC trap, used by system servers (VM in
//! slice 3.4, the rest in Phase 4) and eventually by musl's `_syscall`. The
//! aarch64 trap ABI is fixed by `kernel/src/arch/aarch64/trap.S` +
//! `user_stub.S` (the hand-coded EL0 stubs the kernel still ships as
//! regression coverage), and these wrappers reproduce it exactly:
//!
//!   - `x0` = endpoint (`src`/`dest`/`src_dest`); also receives the result.
//!   - `x1` = IPC primitive number (`SEND`/`RECEIVE`/`SENDREC`/…).
//!   - `x2` = pointer to the 104-byte [`Message`] buffer.
//!   - `svc #0`; the kernel's `do_ipc` writes the result into the saved `x0`.
//!
//! The kernel saves and restores all of `x0..x30` around the trap (see
//! `trap.S`), so from the caller's view only `x0` is clobbered — the asm
//! marks just that. It is *not* `nomem`: the kernel reads the outgoing
//! message from `*msg` and writes the reply back into it.

#![no_std]

use minixrs_kernel_shared::ipc_const::{NOTIFY, RECEIVE, SEND, SENDNB, SENDREC};
use minixrs_kernel_shared::{Endpoint, Message};

/// Blocking send: block until `dest` accepts the message in `msg`.
#[inline]
pub fn ipc_send(dest: Endpoint, msg: &mut Message) -> i32 {
    ipc_trap(dest, SEND, msg)
}

/// Blocking receive: block until a message from `src` (or `ANY`) arrives,
/// filling `msg` in place.
#[inline]
pub fn ipc_receive(src: Endpoint, msg: &mut Message) -> i32 {
    ipc_trap(src, RECEIVE, msg)
}

/// Atomic send-then-receive against `src_dest` — the common client/server
/// round-trip. `msg` carries the request out and the reply back.
#[inline]
pub fn ipc_sendrec(src_dest: Endpoint, msg: &mut Message) -> i32 {
    ipc_trap(src_dest, SENDREC, msg)
}

/// Non-blocking send: deliver `msg` to `dest` if it is already waiting to
/// receive, otherwise return `ENOTREADY` instead of blocking the caller.
#[inline]
pub fn ipc_sendnb(dest: Endpoint, msg: &mut Message) -> i32 {
    ipc_trap(dest, SENDNB, msg)
}

/// Non-blocking notification: set the notify-pending bit in `dest`'s bitmap
/// (or deliver immediately if `dest` is already RECEIVE-blocked and willing).
/// A notification carries no payload — the kernel synthesises the delivered
/// message — so this takes no [`Message`] buffer.
#[inline]
pub fn ipc_notify(dest: Endpoint) -> i32 {
    ipc_trap_no_msg(dest, NOTIFY)
}

/// Issue one IPC trap. `endpoint` → `x0`, `primitive` → `x1`, `&mut *msg`
/// → `x2`, `svc #0`; the kernel's reply code comes back in `x0`.
#[cfg(target_arch = "aarch64")]
#[inline]
fn ipc_trap(endpoint: Endpoint, primitive: i32, msg: &mut Message) -> i32 {
    let mut x0: i64 = endpoint as i64;
    // SAFETY: the kernel SVC entry (`trap.S`) saves/restores x1..x30 around
    // the trap and writes only the result into the saved x0, so the sole
    // clobber is x0. `msg` is a valid, aligned, exclusive 104-byte buffer;
    // the kernel reads the outgoing message and writes the reply through it,
    // so this must not be `nomem`.
    unsafe {
        core::arch::asm!(
            "svc #0",
            inlateout("x0") x0,
            in("x1") primitive as i64,
            in("x2") msg as *mut Message,
            options(nostack),
        );
    }
    x0 as i32
}

/// Issue an IPC trap that carries no message buffer (NOTIFY only). `endpoint`
/// → `x0`, `primitive` → `x1`, `x2` = null; the kernel's reply comes back in
/// `x0`. Distinct from [`ipc_trap`] so the message-carrying wrappers keep their
/// `&mut Message` type safety — never route `SEND`/`RECEIVE`/`SENDREC`/`SENDNB`
/// through here.
#[cfg(target_arch = "aarch64")]
#[inline]
fn ipc_trap_no_msg(endpoint: Endpoint, primitive: i32) -> i32 {
    let mut x0: i64 = endpoint as i64;
    // SAFETY: the kernel SVC entry (`trap.S`) saves/restores x1..x30 and writes
    // only the result into the saved x0, so the sole clobber is x0. NOTIFY
    // carries no message: `mini_notify` (kernel/src/ipc/notify.rs) reads only
    // the endpoint and never dereferences x2, so the null pointer is sound.
    // Unlike `ipc_trap`, this trap touches no caller memory the kernel can see,
    // so `nomem` is correct here (and lets the compiler skip spilling a buffer).
    unsafe {
        core::arch::asm!(
            "svc #0",
            inlateout("x0") x0,
            in("x1") primitive as i64,
            in("x2") 0_u64,
            options(nostack, nomem),
        );
    }
    x0 as i32
}

/// Non-aarch64 fallback so the crate stays `cargo check --workspace`-able on
/// the host. These wrappers only have meaning at EL0 on aarch64; calling one
/// off-target is a build-configuration bug.
#[cfg(not(target_arch = "aarch64"))]
#[inline]
fn ipc_trap(_endpoint: Endpoint, _primitive: i32, _msg: &mut Message) -> i32 {
    unreachable!("minix-ipc IPC traps are aarch64-only")
}

/// Non-aarch64 fallback for [`ipc_trap_no_msg`]; see [`ipc_trap`]'s fallback.
#[cfg(not(target_arch = "aarch64"))]
#[inline]
fn ipc_trap_no_msg(_endpoint: Endpoint, _primitive: i32) -> i32 {
    unreachable!("minix-ipc IPC traps are aarch64-only")
}
