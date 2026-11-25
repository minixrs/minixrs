// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs TTY driver — the console: transmit since slice 5.3 (decision D1),
//! receive since slice 6.5.
//!
//! The first user-space *driver* in minix.rs, and the first EL0-originated text on
//! the serial line. Everything before it either came from the kernel's own PL011
//! writer or rode the `SYS_DIAGCTL` debug channel — which is deliberately a debug
//! channel, not stdio.
//!
//! TTY owns the PL011 and serves [`CDEV_WRITE`] and [`CDEV_READ`]. For a write a
//! client grants it a read-only view of a buffer, sends the grant id, and TTY
//! pulls the bytes across with `SYS_SAFECOPY` before transmitting them. Slice 5.4
//! puts VFS's fd 1 and 2 on top of this; slice 5.6's musl `printf` rides the same
//! rails. For a read the grant is writable and the copy runs the other way, out
//! of the input ring (`input.rs`) that the PL011's receive interrupt fills
//! (`irq.rs`).
//!
//! Built as a freestanding aarch64 ELF (`drivers/tty/user.ld`, a byte copy of the
//! servers'), packed into the MXBI archive by `kernel/build.rs`, and loaded into
//! its own address space at boot — plus one extra page the other boot processes do
//! not get: the UART's MMIO registers at `TTY_UART_VA`, pre-mapped by the kernel
//! with Device attributes. See `pl011.rs`.
//!
//! ## Four things that differ from a server
//!
//! **An unknown `m_type` gets a reply, not a drop.** DS silently drops one, which
//! is fine when nothing SENDRECs it in anger — but a driver's clients *all*
//! SENDREC, and a dropped request blocks the caller forever. Every path out of the
//! dispatch below replies.
//!
//! **The granter comes from `m_source`.** TTY holds `SYS_SAFECOPY`; its clients do
//! not. A granter field in the payload would let any client aim a privileged
//! cross-address-space copy at a third party's memory through TTY. The CDEV
//! request (`server-rt::cdev`) has no way to express one.
//!
//! **A negative `SYS_SAFECOPY` result is relayed verbatim.** `EPERM` ("your grant
//! does not authorize this") and `EFAULT` ("your buffer is not mapped") are
//! different bugs on the client's side, and collapsing them to one errno would hide
//! which.
//!
//! **A `CDEV_READ` is answered at once, whether or not there is input** (slice
//! 6.5, spec R1). TTY never withholds the reply: its one client is VFS, and a
//! VFS blocked in a `SENDREC` to TTY serves nobody else. With bytes buffered the
//! reply is the count copied — at least one, never more than was asked for. With
//! none it is `EAGAIN`, and TTY records the caller's endpoint as its *waiter*:
//! when the receive interrupt next delivers a byte, TTY sends that endpoint one
//! `NOTIFY` and forgets it, and the caller asks again with an ordinary
//! `CDEV_READ`. VFS's half — parking the read, retrying on the notification — is
//! in `servers/vfs`.
//!
//! - The request is validated by `CDEV_WRITE`'s rules in `CDEV_WRITE`'s order,
//!   and a zero-length read then answers `0` *before the ring is looked at*.
//! - The bytes are peeked, copied, and only then consumed, so a copy the kernel
//!   refuses (`EPERM`, `EFAULT`, relayed verbatim) loses no input.
//! - `0` stays EOF and is never sent for a non-zero request: the bytes are raw,
//!   with no echo and no line discipline (spec R6), so a console has no EOF yet.
//! - Once no byte can ever arrive — TTY failed to claim its interrupt line, or
//!   the line has stopped since (a proof round gone wrong, `rx FAIL storm`,
//!   `rx FAIL enable`) — the answer is `EIO`, not an `EAGAIN` whose
//!   notification cannot come. A reader already parked when the line stops is
//!   sent its one `NOTIFY` then, so that its retry hears the `EIO` (slice 6.6,
//!   spec C9).
//!
//! The decisions in that list — the validator, the answer order, the waiter —
//! are `driver-rt::cdev`'s, shared with `virtio-console` since slice 6.6; this
//! file does what they say.
//!
//! The waiter is notified **after** the interrupt line is re-armed, so the line
//! is live again before VFS can come back with a read. The order is `main`'s:
//! `irq.rs` re-arms and reports, and the loop below sends the `NOTIFY`.

// Freestanding for the real (bare-metal) build, but a normal host binary under
// `cargo test` so the pure logic (`input`, `irq`'s phases) gets host-runnable
// unit tests. The test harness
// needs `std` and its own entry point, so `no_std`/`no_main` and the `_start` shim
// below are all gated to `not(test)`.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

minixrs_abi_note::brand!();

mod input;
mod irq;
mod pl011;

use minixrs_driver_rt::cdev;
use minixrs_ipc::{ipc_notify, ipc_send};
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    CDEV_MAX_IO, CDEV_MINOR_CONSOLE, CDEV_READ, CDEV_WRITE, GET_DMA, GET_VIRTIO_MMIO,
    NOTIFY_IRQ_PENDING_OFF, SAFECOPY_FROM, SAFECOPY_TO, SYS_GETINFO_NAME_LEN,
};
use minixrs_kernel_shared::com::{HARDWARE, boot_endpoint};
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{EAGAIN, EIO, ENOSYS, EPERM, OK};
use minixrs_kernel_shared::ipc_const::NOTIFY_MESSAGE;
use minixrs_server_rt::{
    SefConfig, buf_addr, diag_fmt, rd_i32, sef_publish_to_ds, sef_startup, sys_getinfo,
    sys_safecopy,
};

/// The GETINFO selectors gated to the virtio drivers; TTY is not one, so each
/// must answer `EPERM` (slice 6.2, spec T8).
const GETINFO_DENIED: [i32; 2] = [GET_VIRTIO_MMIO, GET_DMA];

/// Ask the kernel for each of [`GETINFO_DENIED`] and print one verdict line.
#[cfg_attr(test, allow(dead_code))]
fn getinfo_deny_probe() {
    let mut m = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };
    for (i, &sel) in GETINFO_DENIED.iter().enumerate() {
        let rc = sys_getinfo(sel, &mut m);
        if rc != EPERM {
            diag_fmt(format_args!("getinfo.deny FAIL case={i} rc={rc}"));
            return;
        }
    }
    diag_fmt(format_args!("getinfo.deny ok n={}", GETINFO_DENIED.len()));
}

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
// runtime's. `.text._start` is an ELF section name, gated to the minixrs target
// so `cargo check --workspace` on a Mach-O host still type-checks.
#[cfg(not(test))]
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "minixrs", unsafe(link_section = ".text._start"))]
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

    getinfo_deny_probe();

    // Claim the PL011's interrupt line and prove the call's refusals, then make
    // the line fire (slice 6.1); once the proof is in, the same object turns
    // receive on and services it (slice 6.5). A failure prints its own
    // `[diag tty] irq…FAIL` line and TTY carries on serving TX, which never
    // needed the line.
    // `None` is a refused claim: no interrupt can ever arrive, so a read is
    // answered `EIO` instead of being left to wait for one (spec R1) — as it
    // is once a line that was claimed has stopped (slice 6.6, spec C9).
    let mut irq_service = irq::IrqService::start(irq::claim_and_probe());

    // The staging buffer lives in `main`'s frame, not in a static and not in
    // `tty_init`'s frame — the same rule `GrantPool` follows, and for the same
    // reason: the kernel reads and writes it while TTY is blocked inside the
    // `SYS_SAFECOPY` SENDREC, so the frame must outlive every call that names it.
    // The stack holds it comfortably at `CDEV_MAX_IO` = 256 bytes. One buffer
    // serves both directions: a request is finished before the next is received.
    let mut staging = [0u8; CDEV_MAX_IO];

    // Received bytes not yet read (spec R3), and the endpoint owed a `NOTIFY`
    // when the next one arrives (spec R1). Both are `main`-frame values, like
    // the staging buffer: plain data, no static, no `unsafe`.
    let mut ring = input::Ring::new();
    let mut waiter = cdev::Waiter::new();
    // Whether the one-shot notification probe below has run.
    let mut notify_probed = false;
    // Whether a failed wake-up `NOTIFY` has already been reported.
    let mut notify_failed = false;

    let mut msg = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };
    loop {
        if sef.receive(&mut msg) != OK {
            continue;
        }
        // An interrupt notification (slice 6.1, spec I12) is identified by the
        // kernel-stamped source — `HARDWARE` cannot be forged by a process, a
        // payload can — and gets no reply: nobody is waiting on one.
        if msg.m_source == boot_endpoint(HARDWARE) && msg.m_type == NOTIFY_MESSAGE {
            // The kernel writes the word little-endian; `rd_i32` reads native,
            // which on aarch64 is the same thing.
            let bits = rd_i32(&msg, NOTIFY_IRQ_PENDING_OFF) as u32;
            // The line stopping is this notification ending reception: the
            // service could receive before handling it and cannot after. That
            // happens at most once a boot, since a stopped line never restarts.
            // `cdev::wake_due` folds it with arrival into one decision.
            let wake = match irq_service.as_mut() {
                Some(service) => {
                    let was_stopped = !service.can_receive();
                    let arrived = service.on_notify(bits, &mut ring);
                    cdev::wake_due(arrived, was_stopped, !service.can_receive())
                }
                None => false,
            };
            // Input arrived: tell whoever was refused with `EAGAIN` to ask
            // again — once, then forget them (spec R1). The line stopping is
            // the other reason to: no input will ever arrive now, and the
            // retry hears `EIO` instead of staying parked (spec C9). Both at
            // once is still one `NOTIFY`. `on_notify` has already
            // re-armed the line by now, and that order is the point (spec R4):
            // the line is live before the waiter can come back with a read.
            // `NOTIFY` never blocks; if the waiter is busy the kernel latches
            // it until its next plain `RECEIVE` — never as the reply to a
            // SENDREC (spec R9). With nobody waiting the bytes simply
            // stay in the ring for the next `CDEV_READ`.
            if wake && let Some(who) = waiter.take() {
                let rc = ipc_notify(who);
                if rc != OK {
                    // The only wake-up a parked reader gets did not go out.
                    // Say so once: a reader left parked with no line in the
                    // log would be indistinguishable from one nobody typed
                    // at. The waiter is kept, but the retry is nominal — every
                    // error `NOTIFY` can answer here (a dead or unknown
                    // endpoint, no `ipc_to` edge) is permanent for that
                    // endpoint, so it fails again until another reader's
                    // `EAGAIN` replaces the slot.
                    waiter.record(who);
                    if !notify_failed {
                        notify_failed = true;
                        diag_fmt(format_args!("rx FAIL notify rc={rc}"));
                    }
                }
            }
            continue;
        }
        // Any other notification is dropped, never answered: nobody is inside
        // a SENDREC waiting on one, and a reply would be a SEND to a process
        // that is not receiving from TTY. Nothing sends TTY one today; slice
        // 6.8's keyboard driver will (phase decision H8), and its arm goes
        // here.
        if msg.m_type == NOTIFY_MESSAGE {
            continue;
        }
        // Capture the caller *first*: it is both the reply target and the granter,
        // and the dispatch below overwrites `msg.m_source` on the way out.
        let caller_e = msg.m_source;
        // The SENDREC-versus-notification probe (slice 6.5, spec R9): notify
        // the first client **before** replying to its first request. That
        // client is inside a SENDREC to TTY, so the kernel may not deliver the
        // notification now and latches it; the client's *next* SENDREC to TTY
        // then meets a latched notification from the very process it is asking.
        // The kernel must hand it the reply and keep the notification for its
        // next plain `RECEIVE` — MINIX 3's "pending notifications, except for
        // SENDREC" rule in `mini_receive`. Without that rule the notification
        // comes back *as* the reply: VFS, whose prologue is the first client,
        // prints `cdev.short FAIL rc=4096` instead of `cdev.short ok n=256`.
        // To the client it is one spurious "ask again", which the `CDEV_READ`
        // contract already allows.
        if !notify_probed {
            notify_probed = true;
            let _ = ipc_notify(caller_e);
        }
        match msg.m_type {
            CDEV_WRITE => {
                let rc = do_write(caller_e, &msg, &mut staging);
                reply(caller_e, &mut msg, rc);
            }
            // Always replied to, and at once: with nothing buffered the answer
            // is `EAGAIN`, never a withheld reply (spec R1).
            CDEV_READ => {
                let rc = do_read(
                    caller_e,
                    &msg,
                    &mut staging,
                    &mut ring,
                    &mut waiter,
                    irq_service
                        .as_ref()
                        .is_some_and(irq::IrqService::can_receive),
                );
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
    let req = minixrs_server_rt::cdev::parse(msg);
    let n = match cdev::validate_write(req, CDEV_MINOR_CONSOLE) {
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

/// Serve one `CDEV_READ` (slice 6.5, spec R1). Returns the reply `m_type`: the
/// byte count delivered (`>= 0`), or a negative errno — `EAGAIN` when nothing is
/// buffered, in which case `caller_e` has been recorded in `waiter` and is owed
/// one `NOTIFY`; `EIO` when `can_receive` is `false`, which is `main` saying no
/// input can arrive any more (no line, or a stopped one).
///
/// The decision is `cdev::read_plan`'s; this function does what it says. The
/// waiter is recorded before `EAGAIN` is returned, and so before it is sent:
/// TTY is single-threaded and takes no interrupt between the two, so the
/// notification for the next byte cannot be decided before the waiter exists.
///
/// `staging` is `main`'s buffer, for the reason `do_write` gives.
#[cfg_attr(test, allow(dead_code))]
fn do_read(
    caller_e: Endpoint,
    msg: &Message,
    staging: &mut [u8; CDEV_MAX_IO],
    ring: &mut input::Ring,
    waiter: &mut cdev::Waiter,
    can_receive: bool,
) -> i32 {
    let req = minixrs_server_rt::cdev::parse(msg);
    let want = match cdev::validate_read(req, CDEV_MINOR_CONSOLE) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let n = match cdev::read_plan(want, ring.len(), can_receive) {
        // A legal zero-length read: no grant is written, no waiter recorded.
        cdev::ReadPlan::Zero => return 0,
        // No byte can ever arrive: refuse, rather than park the caller.
        cdev::ReadPlan::Stopped => return EIO,
        cdev::ReadPlan::Empty => {
            waiter.record(caller_e);
            return EAGAIN;
        }
        cdev::ReadPlan::Copy(n) => n,
    };

    // Peek, do not pop: the bytes stay in the ring until the copy has
    // succeeded, so a refused copy consumes nothing (spec R1).
    let n = ring.peek(&mut staging[..n]);

    // Push the bytes into the client's granted buffer. `caller_e` is the
    // kernel-stamped source of this very message — the granter can be nothing
    // else.
    let rc = sys_safecopy(
        SAFECOPY_TO,
        caller_e,
        req.gid,
        req.offset,
        buf_addr(&mut staging[..n]),
        n as u64,
    );
    if rc != OK {
        // Verbatim, as for a write — and the input is still owed to whoever
        // asks next.
        return rc;
    }

    ring.consume(n);
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
