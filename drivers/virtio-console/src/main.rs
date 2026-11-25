// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs `virtio-console` driver — the second CDEV backend, `/dev/hvc0`
//! (slice 6.6).
//!
//! The kernel's boot probe assigns this driver the virtio-mmio transport that
//! carries a console device, pre-maps that register page and a contiguous DMA
//! run into this address space, and puts the transport's interrupt line on this
//! driver's allow-list. The driver brings the device up with no optional
//! feature — one port, queue 0 to receive and queue 1 to transmit — and serves
//! `CDEV_READ` and `CDEV_WRITE` on its one minor, `CDEV_MINOR_HVC0`. Its client
//! is VFS, which reaches it through the `/dev/hvc0` node; fd 0, 1 and 2 stay on
//! TTY.
//!
//! ## Publishing, whatever the device does
//!
//! `vcon_init`, the `init_fresh` callback, publishes this driver to DS
//! **unconditionally** (spec C4) — unlike `virtio-blk`, which publishes only
//! when it has a device, because there a DS miss means "no disk". Nothing
//! selects between consoles, so a miss here would mean nothing; and with the
//! name always there VFS's lookup follows the ordinary rule (a boot-endpoint
//! fallback and a distinguishable line). The process is always in its receive
//! loop to answer.
//!
//! ## Bring-up
//!
//! [`bring_up`] runs once, after `sef_startup`, and prints exactly one verdict
//! line:
//!
//! 1. `GET_VIRTIO_MMIO` — `ENOENT` means the kernel's probe assigned this
//!    driver no device: `no device`.
//! 2. `GET_DMA`; check the kernel's cookie (step 5's first queue zeroes over
//!    it).
//! 3. `MmioTransport::new` (magic, version 2); `DeviceID` must be 3.
//! 4. `negotiate`, requesting nothing (spec C3): the accepted word is
//!    `VIRTIO_F_VERSION_1` alone. Without `F_SIZE`, `F_MULTIPORT` or
//!    `F_EMERG_WRITE` no field of the device configuration is valid, so **none
//!    is read**.
//! 5. Size both queues under the device's `QueueNumMax`, lay the four pages out
//!    (`bringup::vcon_layout`: `receiveq`, `transmitq`, the receive buffer, the
//!    transmit buffer), enable both queues; `DRIVER_OK`, read back.
//! 6. Claim the INTID; a claim on `INTID + 1` must answer `EPERM`.
//! 7. Post the one receive buffer: chain it device-writable into `receiveq` and
//!    notify the queue.
//! 8. `virtio ok features=… rxq=… txq=… slot=…` — `rxq` and `txq` are the two
//!    queue sizes.
//!
//! A failing step prints `virtio FAIL step=<step> …` and abandons the rest
//! without resetting the device or releasing anything; a failure after
//! `negotiate` succeeded also sets `FAILED` through `driver-rt`'s `fail`. No
//! bring-up step waits on a register or a ring, and none needs a completion, so
//! the first client can be answered as soon as the loop is reached. On success
//! everything a request needs is kept in a [`Port`] in `main`'s frame for the
//! life of the process.
//!
//! ## Receive: one buffer, posted or held
//!
//! There is one receive buffer (spec C5), and `port::Rx` says who has it:
//! *posted* — chained into `receiveq`, the device may fill it — or *held* — the
//! device returned it with some bytes, some of which a reader may have taken.
//! There is no ring to copy into and so nothing to overflow: while the buffer
//! is held the device has nowhere to put more input, and the host's end of the
//! chardev waits. A `CDEV_READ` is answered at once, in `driver-rt::cdev`'s
//! order (spec C6):
//!
//! - the request is validated by `CDEV_WRITE`'s rules in `CDEV_WRITE`'s order;
//! - a zero-length read answers `0` before the buffer is looked at;
//! - a broken device answers `EIO`;
//! - with the buffer posted the caller is recorded as the *waiter* and then
//!   answered `EAGAIN`;
//! - with bytes held, `min(want, held)` of them are copied **straight from the
//!   DMA buffer** to the caller's grant — no staging copy. The offset moves
//!   only after `SYS_SAFECOPY` returned `OK`, so a refused copy consumes
//!   nothing and the bytes are owed to whoever asks next.
//!
//! When a read takes the last held byte the buffer is chained into `receiveq`
//! again and the queue notified **before the reply goes out**.
//!
//! ## Transmit: one chain, waited for
//!
//! A `CDEV_WRITE` copies up to `CDEV_MAX_IO` bytes from the caller's grant into
//! the transmit buffer (a refused grant never reaches the device), chains a
//! handle of exactly that many bytes into `transmitq`, notifies, and blocks in
//! a receive **filtered to `HARDWARE`** until that chain has come back (spec
//! C7) — blocked, never polling, with a second client left queued in the
//! kernel. The reply is the count moved; a longer write is a short write and
//! VFS's loop re-sends the rest. A zero-length write answers `0` and touches
//! nothing.
//!
//! A device that never completes a transmit hangs the driver in that receive
//! with VFS behind it. Nothing reports that, and there is no timeout because no
//! user-space driver holds an alarm.
//!
//! ## One interrupt handler
//!
//! Every interrupt, wherever it is received — the main loop or the transmit
//! wait — goes through [`service`] (spec C8): [`on_interrupt`] reads
//! `InterruptStatus`, acknowledges exactly those bits, walks `receiveq` to
//! empty, walks `transmitq` to empty and re-arms the line, in `driver-rt`'s
//! documented order; **then** the waiter is sent its one `NOTIFY` if input
//! arrived, and forgotten. After the re-arm, so the line is live before the
//! waiter can come back with a read. Input that lands while a write is in
//! flight is therefore taken and its waiter notified from inside the wait; VFS
//! is mid-`SENDREC` to this driver then, and the kernel keeps the notification
//! for its next plain `RECEIVE`. A wake-up `NOTIFY` that fails keeps the waiter
//! and prints `rx FAIL notify rc=…` once per boot.
//!
//! A used element nobody was waiting for is a fault, not something to skip: a
//! `transmitq` completion with no write in flight, a `receiveq` completion
//! while the buffer is already held, any `driver-rt` `QueueFault`.
//!
//! ## What a client hears
//!
//! - **`ENXIO`** for every `CDEV_READ` and `CDEV_WRITE`, well-formed or not,
//!   when there is no working device — none assigned, or a failed bring-up
//!   (`port::absent_reply`, spec C4) — and for any minor but `CDEV_MINOR_HVC0`
//!   on a device that came up.
//! - **`EINVAL`** for a malformed request (`driver-rt::cdev`'s validator, the
//!   one TTY uses).
//! - **`EAGAIN`** for a read that finds no input; the caller is owed a `NOTIFY`.
//! - **`EIO`** for a read or a non-empty write once the device has broken.
//! - A refused grant or an unmapped buffer is relayed **verbatim** from
//!   `SYS_SAFECOPY`.
//! - **`ENOSYS`** for any other request number.
//!
//! A device that breaks the ring contract, completes something out of turn, or
//! whose line can no longer be re-armed or waited on is marked `FAILED`,
//! reported once as `virtio FAIL step=io …`, and never used again: its line
//! stays masked, and the recorded waiter is sent its `NOTIFY` so that its retry
//! hears the `EIO` instead of staying parked (spec C9; `port::wake_due`).
//!
//! The driver never exits: the kernel halts if a process holding a virtio
//! assignment exits or execs (the DMA-run debt). The granter of every copy is
//! the kernel-stamped `m_source`, never a payload field.

// Freestanding for the real (bare-metal) build, but a normal host binary under
// `cargo test` so `bringup`'s and `port`'s logic get host-runnable unit tests.
// The test harness needs `std` and its own entry point, so `no_std`/`no_main`
// and the `_start` shim below are all gated to `not(test)`.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

minixrs_abi_note::brand!();

mod bringup;
mod port;

use bringup::{
    RECEIVE_QUEUE, TRANSMIT_QUEUE, VCON_IRQ_NOTIFY_ID, VCON_WANTED_FEATURES, cookie_ok, vcon_layout,
};
use minixrs_driver_rt::transport::{NegotiateError, driver_ok, fail, negotiate};
use minixrs_driver_rt::{
    DmaBuf, DmaRegion, DmaRun, IrqLine, MmioError, MmioTransport, Transport, VirtQueue, cdev,
    clamp_queue_size, queue_layout,
};
use minixrs_ipc::{ipc_notify, ipc_receive, ipc_send};
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    CDEV_MINOR_HVC0, CDEV_READ, CDEV_WRITE, GET_DMA, GET_VIRTIO_MMIO, GETINFO_DMA_LEN_OFF,
    GETINFO_DMA_PA_OFF, GETINFO_DMA_VA_OFF, GETINFO_VIRTIO_INTID_OFF, GETINFO_VIRTIO_SLOT_OFF,
    GETINFO_VIRTIO_VA_OFF, NOTIFY_IRQ_PENDING_OFF, SAFECOPY_FROM, SAFECOPY_TO,
    SYS_GETINFO_NAME_LEN,
};
use minixrs_kernel_shared::com::{HARDWARE, boot_endpoint};
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{EAGAIN, EIO, ENOENT, ENOSYS, ENXIO, EPERM, OK};
use minixrs_kernel_shared::ipc_const::NOTIFY_MESSAGE;
use minixrs_kernel_shared::virtio::VIRTIO_ID_CONSOLE;
use minixrs_server_rt::{
    SefConfig, diag_fmt, rd_i32, rd_u64, sef_publish_to_ds, sef_startup, sys_getinfo, sys_safecopy,
};
use port::{Irq, Rx, RxUsed, Tx};

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

/// A console that came up: everything a request needs, kept for the life of
/// the process in `main`'s frame.
struct Port {
    t: MmioTransport,
    /// `receiveq` (queue 0): the one receive buffer, when it is posted.
    rxq: VirtQueue,
    /// `transmitq` (queue 1): the one chain in flight, while a write waits.
    txq: VirtQueue,
    line: IrqLine,
    /// The receive buffer (device-writable). Read only while `rx` is held.
    rx_buf: DmaBuf,
    /// The transmit buffer (device-readable). A write chains its front.
    tx_buf: DmaBuf,
    /// Who has the receive buffer, and how much of it has been read.
    rx: Rx,
    /// The transmit chain in flight, if any.
    tx: Tx,
    /// The device broke the ring contract, completed something out of turn,
    /// or can no longer be waited on. Set once by [`break_device`]; every
    /// later read and non-empty write is `EIO`.
    broken: bool,
}

/// The endpoint owed a `NOTIFY` (spec C6), and whether a failed one has been
/// reported yet. `main`-frame state, like the [`Port`]; apart from it because a
/// driver with no device still has one.
struct Wake {
    /// Whoever was last answered `EAGAIN`.
    waiter: cdev::Waiter,
    /// Whether `rx FAIL notify` has been printed this boot.
    notify_failed: bool,
}

// Only `_start` calls `main`; under `cargo test` `_start` is gone, so `main` (and
// the helpers it alone reaches) would read as dead code.
#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    let sef = sef_startup(SefConfig {
        init_fresh: Some(vcon_init),
        signal_handler: None,
    })
    .unwrap_or_else(|_| {
        // No recovery and nothing to print: a failed handshake means this driver
        // never learned its own endpoint, so it cannot even announce the failure.
        loop {
            core::hint::spin_loop()
        }
    });

    // After `sef_startup`, not inside `vcon_init`: a callback that fails leaves
    // the driver spinning with no way to say why, where this prints a verdict.
    // `None` is no working device — none assigned, or a failed step — and every
    // CDEV request then answers `ENXIO` (spec C4).
    let mut port = bring_up();
    let mut wake = Wake {
        waiter: cdev::Waiter::new(),
        notify_failed: false,
    };

    let mut msg = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };
    loop {
        if sef.receive(&mut msg) != OK {
            continue;
        }
        // An interrupt notification is identified by the kernel-stamped source,
        // never by payload, and gets no reply. No write is in flight here
        // (`do_write` does not return while one is), so this is input, or a
        // configuration change that walks two empty rings; a `transmitq`
        // completion now would surface from the walk as a fault. A broken
        // device's line is left masked.
        if msg.m_source == boot_endpoint(HARDWARE) && msg.m_type == NOTIFY_MESSAGE {
            if let Some(port) = port.as_mut() {
                // The kernel writes the word little-endian; `rd_i32` reads
                // native, which on aarch64 is the same thing.
                let pending = rd_i32(&msg, NOTIFY_IRQ_PENDING_OFF) as u32;
                if !port.broken && port.line.fired(pending) {
                    service(port, &mut wake);
                }
            }
            continue;
        }
        // Any other notification has no sender waiting in a SENDREC: drop it,
        // never answer it.
        if msg.m_type == NOTIFY_MESSAGE {
            continue;
        }
        // Capture the caller first: it is the reply target, the granter (never
        // a payload field) and the waiter, and the reply overwrites
        // `msg.m_source`.
        let caller_e = msg.m_source;
        let rc = match msg.m_type {
            // The gate comes first and takes nothing of the request: with no
            // working device every read and write is `ENXIO`, a malformed one
            // and a zero-length one included (spec C4). It is a function in
            // `port.rs`, not a test here, so that a host test holds it.
            CDEV_READ | CDEV_WRITE => match (port::absent_reply(port.is_some()), port.as_mut()) {
                (Some(e), _) => e,
                // Always replied to, and at once: with nothing held the answer
                // is `EAGAIN`, never a withheld reply.
                (None, Some(port)) if msg.m_type == CDEV_READ => {
                    do_read(port, &mut wake, caller_e, &msg)
                }
                (None, Some(port)) => do_write(port, &mut wake, caller_e, &msg),
                // Unreachable: `absent_reply` was asked about this very option.
                (None, None) => ENXIO,
            },
            // Unlike DS, reply rather than drop — the caller is inside a SENDREC
            // and would otherwise block forever.
            _ => ENOSYS,
        };
        reply(caller_e, &mut msg, rc);
    }
}

/// SEF fresh-init callback: publish to DS, whatever the device turns out to be
/// (spec C4).
///
/// Unconditional, unlike `virtio-blk`'s: no client reads a miss as an answer,
/// and a `vcon` that is always in DS lets VFS's lookup keep the ordinary
/// fallback rule. Nothing here touches the device or the kernel's assignment —
/// [`bring_up`] asks, after the handshake, and reports.
///
/// Always `OK`: a non-`OK` return fails `sef_startup` and leaves the driver
/// spinning with no way to say why. A failed publish is reported instead (the
/// debug channel is up before this callback runs), and the driver carries on,
/// reachable through VFS's boot-endpoint fallback.
#[cfg_attr(test, allow(dead_code))]
fn vcon_init(_endpoint: Endpoint, name: &[u8; SYS_GETINFO_NAME_LEN]) -> i32 {
    let rc = sef_publish_to_ds(name);
    if rc != OK {
        diag_fmt(format_args!("ds FAIL rc={rc}"));
    }
    OK
}

/// Stop using the device: write `FAILED`, remember it, and say why — once. The
/// interrupt line is left masked by not re-arming it.
///
/// This does not wake the waiter. A caller outside the interrupt path follows
/// it with [`wake_waiter`] ([`break_and_wake`] is the two together);
/// [`service`] does so for a break inside the handler.
#[cfg_attr(test, allow(dead_code))]
fn break_device(port: &mut Port, why: core::fmt::Arguments<'_>) {
    if port.broken {
        return;
    }
    port.broken = true;
    fail(&mut port.t);
    diag_fmt(format_args!("virtio FAIL step=io {why}"));
}

/// Send the recorded waiter its one `NOTIFY` and forget it (spec C6).
///
/// `NOTIFY` never blocks; if the waiter is busy — VFS is inside a `SENDREC` to
/// this driver whenever a read or a write is being served — the kernel latches
/// it until the waiter's next plain `RECEIVE`, never as the reply to that
/// `SENDREC`. With nobody recorded nothing is sent, and held bytes simply wait
/// for the next `CDEV_READ`.
///
/// A `NOTIFY` that fails is the only wake-up a parked reader gets not going
/// out. The failure is reported once per boot — TTY's line exactly. The waiter
/// is recorded again, but unlike TTY nothing here will retry it: the buffer is
/// now held, so no further receive interrupt can come until a read drains it,
/// and a transmit interrupt owes no wake-up. It stays recorded only until
/// another reader's `EAGAIN` replaces it. Every error `NOTIFY` can answer here
/// is permanent for that endpoint, so a retry would fail the same way.
#[cfg_attr(test, allow(dead_code))]
fn wake_waiter(wake: &mut Wake) {
    if let Some(who) = wake.waiter.take() {
        let rc = ipc_notify(who);
        if rc != OK {
            wake.waiter.record(who);
            if !wake.notify_failed {
                wake.notify_failed = true;
                diag_fmt(format_args!("rx FAIL notify rc={rc}"));
            }
        }
    }
}

/// [`break_device`], then the wake-up a stopped device owes its waiter (spec
/// C9): for a break found outside the interrupt handler.
#[cfg_attr(test, allow(dead_code))]
fn break_and_wake(port: &mut Port, wake: &mut Wake, why: core::fmt::Arguments<'_>) {
    let was_broken = port.broken;
    break_device(port, why);
    if port::wake_due(false, was_broken, port.broken) {
        wake_waiter(wake);
    }
}

/// Chain the receive buffer, device-writable, into `receiveq` and tell the
/// device. `false`, with nothing published, if the queue has no descriptor for
/// it — which, with one buffer and it not posted, is a queue whose bookkeeping
/// no longer adds up.
///
/// Takes the three fields rather than a [`Port`] because bring-up posts the
/// buffer before a `Port` exists.
#[cfg_attr(test, allow(dead_code))]
fn post_rx(t: &mut MmioTransport, rxq: &mut VirtQueue, rx_buf: DmaBuf) -> bool {
    if rxq.add(&[], &[rx_buf]).is_none() {
        return false;
    }
    t.notify(RECEIVE_QUEUE);
    true
}

/// Handle one interrupt on the device's line, in `driver-rt`'s documented order
/// with a second walk (spec C8): read `InterruptStatus`, acknowledge exactly
/// those bits, walk `receiveq` until it is empty, walk `transmitq` until it is
/// empty, re-arm. Returns what the two walks found.
///
/// `receiveq`: a used element moves the buffer from posted to held
/// ([`Irq::input`]); a zero-length one is chained straight back in. One that
/// arrives while the buffer is already held is a fault. `transmitq`: a used
/// element must be the chain in flight ([`Irq::tx_done`]); with none in flight,
/// or for another head, it is a fault.
///
/// A fault — `port`'s or the queue's own — ends the device and the walk at
/// once: a `QueueFault` is sticky, so popping again would only report it again.
/// The line then stays masked. On a healthy device the line is re-armed after
/// every interrupt, including one whose walks found nothing (a configuration
/// change); without that the next completion never arrives.
///
/// The caller is [`service`], which sends the wake-up this leaves owed.
#[cfg_attr(test, allow(dead_code))]
fn on_interrupt(port: &mut Port) -> Irq {
    // The acknowledgement precedes the walks: `InterruptStatus` is a level, and
    // an ack after them could clear a completion neither walk saw.
    let bits = port.t.interrupt_status();
    port.t.interrupt_ack(bits);

    let mut irq = Irq::default();
    loop {
        match port.rxq.pop_used() {
            // The length is the device's word; `Rx::on_used` clamps it.
            Ok(Some((_, written))) => match port.rx.on_used(written) {
                Ok(RxUsed::Input) => irq.input = true,
                // Nothing was delivered and nothing is held: the buffer goes
                // straight back, and nobody is woken.
                Ok(RxUsed::Repost) => {
                    if !post_rx(&mut port.t, &mut port.rxq, port.rx_buf) {
                        let free = port.rxq.num_free();
                        break_device(port, format_args!("rx add free={free}"));
                        return irq;
                    }
                }
                Err(fault) => {
                    break_device(port, format_args!("{fault:?}"));
                    return irq;
                }
            },
            Ok(None) => break,
            Err(fault) => {
                break_device(port, format_args!("rxq {fault:?}"));
                return irq;
            }
        }
    }
    loop {
        match port.txq.pop_used() {
            Ok(Some((head, _))) => match port.tx.complete(head) {
                Ok(()) => irq.tx_done = true,
                Err(fault) => {
                    break_device(port, format_args!("{fault:?}"));
                    return irq;
                }
            },
            Ok(None) => break,
            Err(fault) => {
                break_device(port, format_args!("txq {fault:?}"));
                return irq;
            }
        }
    }

    // A line that cannot be re-armed delivers nothing more: no input would ever
    // arrive and the next write would wait forever. That device is unusable.
    let rc = port.line.rearm();
    if rc != OK {
        break_device(port, format_args!("rearm rc={rc}"));
    }
    irq
}

/// Everything one interrupt asks for, wherever it was received — the main loop
/// or [`do_write`]'s wait (spec C8): [`on_interrupt`], and **then** the
/// wake-up, if input arrived or the device has just broken
/// (`port::wake_due`).
///
/// The order is the point. The handler has re-armed the line by the time the
/// `NOTIFY` goes out, so the line is live before the waiter can come back with
/// a read. (A device that broke has no line to re-arm; its waiter's retry
/// hears `EIO`.)
#[cfg_attr(test, allow(dead_code))]
fn service(port: &mut Port, wake: &mut Wake) -> Irq {
    let was_broken = port.broken;
    let irq = on_interrupt(port);
    if port::wake_due(irq.input, was_broken, port.broken) {
        wake_waiter(wake);
    }
    irq
}

/// Serve one `CDEV_READ` on a device that came up (spec C6). Returns the reply
/// `m_type`: the byte count delivered (`>= 0`), or a negative errno — `EAGAIN`
/// when the buffer is posted, in which case `caller_e` has been recorded as the
/// waiter and is owed one `NOTIFY`; `EIO` once the device has broken.
///
/// The decision is `cdev::read_plan`'s and `port::Rx`'s; this function does
/// what they say, in this order: validate; a zero-length read is `0` before
/// the buffer is looked at; a broken device is `EIO`; nothing held records the
/// waiter and **then** returns `EAGAIN` — the driver is single-threaded and
/// takes no interrupt between the two, so the notification for the next input
/// cannot be decided before the waiter exists.
///
/// Bytes held are copied straight from the DMA buffer to the caller's grant,
/// and consumed only once that copy has returned `OK`: a refused copy returns
/// its errno from here with `rx` untouched. A read that takes the last held
/// byte re-posts the buffer and notifies `receiveq` before returning the count.
#[cfg_attr(test, allow(dead_code))]
fn do_read(port: &mut Port, wake: &mut Wake, caller_e: Endpoint, msg: &Message) -> i32 {
    let req = minixrs_server_rt::cdev::parse(msg);
    let want = match cdev::validate_read(req, CDEV_MINOR_HVC0) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let n = match cdev::read_plan(want, port.rx.available(), !port.broken) {
        // A legal zero-length read: no grant is written, no waiter recorded.
        cdev::ReadPlan::Zero => return 0,
        // No input can ever arrive: refuse, rather than park the caller.
        cdev::ReadPlan::Stopped => return EIO,
        cdev::ReadPlan::Empty => {
            wake.waiter.record(caller_e);
            return EAGAIN;
        }
        cdev::ReadPlan::Copy(n) => n,
    };

    // Peek, do not consume: where the bytes are, and how many of the `n`
    // planned are there (all of them — `read_plan` was given `available`).
    // The address is the buffer's own plus an offset inside it.
    let src = port.rx.peek(n).and_then(|(off, n)| {
        let addr = port.rx_buf.va().checked_add(u64::try_from(off).ok()?)?;
        Some((addr, n))
    });
    let Some((addr, n)) = src else {
        // Unreachable: `read_plan` answers `Copy` only with bytes held, and
        // the buffer's addresses were range-checked when it was laid out.
        let rx = port.rx;
        break_and_wake(port, wake, format_args!("peek {rx:?}"));
        return EIO;
    };

    // Straight from the DMA buffer into the client's granted buffer: one copy.
    // The device is not writing these bytes — it returned the buffer, and gets
    // it back only below. `caller_e` is the kernel-stamped source of this very
    // message; the granter can be nothing else.
    let rc = sys_safecopy(SAFECOPY_TO, caller_e, req.gid, req.offset, addr, n as u64);
    if rc != OK {
        // Verbatim: EPERM ("your grant does not authorize this") and EFAULT
        // ("your buffer is not mapped") are different bugs on the client's
        // side. Nothing was consumed: the bytes are owed to whoever asks next.
        return rc;
    }

    match port.rx.advance(n) {
        Ok(false) => {}
        // That was the last held byte: the device gets its buffer back, and is
        // told, before the reply goes out.
        Ok(true) => {
            if !post_rx(&mut port.t, &mut port.rxq, port.rx_buf) {
                // The caller has its bytes and they are consumed, so the count
                // is still the truthful reply; the next request hears `EIO`.
                let free = port.rxq.num_free();
                break_and_wake(port, wake, format_args!("rx add free={free}"));
            }
        }
        // Unreachable: `n` is `peek`'s. The driver's own count no longer adds
        // up, so nothing about the buffer is trusted from here on.
        Err(fault) => {
            break_and_wake(port, wake, format_args!("{fault:?}"));
            return EIO;
        }
    }
    n as i32
}

/// Serve one `CDEV_WRITE` on a device that came up (spec C7). Returns the reply
/// `m_type`: the byte count sent (`>= 0`, at most `CDEV_MAX_IO`), or a negative
/// errno.
///
/// The caller's bytes are taken into the transmit buffer *before* the device
/// is touched: a refused grant or an unmapped buffer is relayed verbatim and
/// nothing is chained. Then a handle of exactly the bytes to send goes into
/// `transmitq`, the queue is notified, and this does not return until that
/// chain has come back.
///
/// The wait is a receive filtered to `HARDWARE`: blocked, never polling, and
/// blind to every other sender, which keeps a second client queued in the
/// kernel instead of in this driver. Every interrupt taken in it goes through
/// [`service`], the main loop's own handler — so input that arrives during a
/// write is taken into the held buffer and its waiter notified from here. A
/// device that never completes hangs the driver in this loop.
#[cfg_attr(test, allow(dead_code))]
fn do_write(port: &mut Port, wake: &mut Wake, caller_e: Endpoint, msg: &Message) -> i32 {
    let req = minixrs_server_rt::cdev::parse(msg);
    let n = match cdev::validate_write(req, CDEV_MINOR_HVC0) {
        Ok(n) => n,
        Err(e) => return e,
    };
    if n == 0 {
        // A legal zero-length write. No grant is read and the device is not
        // touched — broken or not — so it cannot be used to probe a grant.
        return 0;
    }
    if port.broken {
        return EIO;
    }
    // The validator has already clamped to `CDEV_MAX_IO`; `tx_len` says the
    // same thing about the transmit buffer, so the count fits it whatever the
    // validator's clamp becomes. A longer request is a short write.
    let n = port::tx_len(n);
    let Some(chain) = port.tx_buf.prefix(n) else {
        // Unreachable: `1 <= n <= CDEV_MAX_IO`, the buffer's length.
        break_and_wake(port, wake, format_args!("tx len={n}"));
        return EIO;
    };

    // Pull the bytes across from the client's granted buffer, straight into
    // the transmit buffer. Nothing is in flight, so the device is not reading
    // it. `caller_e` is the kernel-stamped source — the granter can be nothing
    // else.
    let rc = sys_safecopy(
        SAFECOPY_FROM,
        caller_e,
        req.gid,
        req.offset,
        port.tx_buf.va(),
        n as u64,
    );
    if rc != OK {
        // Verbatim, as in `do_read`. Nothing has been given to the device.
        return rc;
    }

    // One device-readable descriptor of exactly `n` bytes.
    let Some(head) = port.txq.add(&[chain], &[]) else {
        // With nothing in flight every descriptor is free, so this is a queue
        // whose bookkeeping no longer adds up.
        let free = port.txq.num_free();
        break_and_wake(port, wake, format_args!("tx add free={free}"));
        return EIO;
    };
    if let Err(fault) = port.tx.start(head) {
        // Unreachable: the previous write did not return while its chain was
        // in flight.
        break_and_wake(port, wake, format_args!("{fault:?}"));
        return EIO;
    }
    port.t.notify(TRANSMIT_QUEUE);

    let mut m = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };
    loop {
        let rc = ipc_receive(boot_endpoint(HARDWARE), &mut m);
        if rc != OK {
            // Never retried in a loop: a receive that fails once would fail
            // again at once, and that is a spin. The chain stays in flight on
            // a device nobody is listening to, so the device is finished.
            break_and_wake(port, wake, format_args!("receive rc={rc}"));
            return EIO;
        }
        // Another line's bit (this driver claims only one) is not ours to handle.
        let pending = rd_i32(&m, NOTIFY_IRQ_PENDING_OFF) as u32;
        if !port.line.fired(pending) {
            continue;
        }
        let irq = service(port, wake);
        // Checked before `tx_done`: a write whose chain came back in a walk
        // that then faulted, or whose line could not be re-armed, is answered
        // EIO even if the device sent the bytes. One conservative reply from a
        // device that is dead from here on.
        if port.broken {
            return EIO;
        }
        if irq.tx_done {
            break;
        }
    }
    // The count actually moved, which is `CDEV_MAX_IO` for an over-long
    // request.
    n as i32
}

/// Run the bring-up steps once, printing one verdict line: `no device`, the
/// first failing step's `virtio FAIL step=…`, or `virtio ok …`. `Some` only for
/// the last: the device, its two queues, its line and its two buffers, with
/// the receive buffer posted.
#[cfg_attr(test, allow(dead_code))]
fn bring_up() -> Option<Port> {
    let mut m = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };

    // Step 1: where the registers are. ENOENT is the healthy no-device path:
    // the kernel's probe found no console to assign.
    let rc = sys_getinfo(GET_VIRTIO_MMIO, &mut m);
    if rc == ENOENT {
        diag_fmt(format_args!("no device"));
        return None;
    }
    if rc != OK {
        diag_fmt(format_args!("virtio FAIL step=getinfo rc={rc}"));
        return None;
    }
    let regs_va = rd_u64(&m, GETINFO_VIRTIO_VA_OFF);
    let intid = rd_i32(&m, GETINFO_VIRTIO_INTID_OFF) as u32;
    let slot = rd_i32(&m, GETINFO_VIRTIO_SLOT_OFF) as u32;

    // Step 2: the DMA run, and the proof that its VA and PA are the same memory.
    let rc = sys_getinfo(GET_DMA, &mut m);
    if rc != OK {
        diag_fmt(format_args!("virtio FAIL step=dma rc={rc}"));
        return None;
    }
    let run = DmaRun {
        va: rd_u64(&m, GETINFO_DMA_VA_OFF),
        pa: rd_u64(&m, GETINFO_DMA_PA_OFF),
        len: rd_u64(&m, GETINFO_DMA_LEN_OFF),
    };
    // SAFETY: `run` is the kernel's own `GET_DMA` report (`rc == OK` above): it
    // mapped `run.len` bytes of RAM starting at `run.pa` read-write at `run.va`
    // in this address space alone, and never unmaps them — a process holding a
    // virtio assignment may not exit or exec (T7). No Rust reference to that
    // memory exists or is ever made: this driver reaches it only through the
    // `DmaRegion` built here and the two `QueueMem`s and two `DmaBuf`s that
    // come out of it (and the `prefix` of one of those). This is the only
    // place the run is adopted.
    let region = unsafe { DmaRegion::new(run) };
    // The cookie is the run's first eight bytes; the stamp is 8-aligned (T7).
    let Some(region) = region.filter(|_| run.va.is_multiple_of(8) && run.len >= 8) else {
        diag_fmt(format_args!(
            "virtio FAIL step=dma va={:#x} len={}",
            run.va, run.len
        ));
        return None;
    };
    // A volatile byte copy that makes no buffer handle: these eight bytes are in
    // the page step 5 splits off for `receiveq`, which no `DmaBuf` may ever
    // cover. The kernel writes the word in its native order, which on aarch64
    // is little-endian.
    let mut word = [0u8; 8];
    let read = region.peek(0, &mut word);
    let word = u64::from_le_bytes(word);
    if !read || !cookie_ok(word, run.pa) {
        diag_fmt(format_args!(
            "virtio FAIL step=dma cookie={word:#x} pa={:#x}",
            run.pa
        ));
        return None;
    }
    // The stamp is not cleared here: that would take a writable handle on the
    // queue page. `VirtQueue::new` zeroes the queue's bytes, these among them.

    // Step 3: the transport, and that it is the console.
    // SAFETY: `regs_va` is `VIRTIO_MMIO_PAGE_VA` plus the slot's 0x200-aligned
    // offset in its page, which the kernel mapped Device-RW in this address space
    // alone (T5) and reported through `GET_VIRTIO_MMIO`; the 0x200-byte window
    // lies inside that page, and nothing else here touches it. `new` re-checks
    // the magic, which is the proof the VA is the device the kernel probed.
    let mut t = match unsafe { MmioTransport::new(regs_va) } {
        Ok(t) => t,
        Err(MmioError::BadMagic(magic)) => {
            diag_fmt(format_args!("virtio FAIL step=transport magic={magic:#x}"));
            return None;
        }
        Err(MmioError::LegacyVersion(version)) => {
            diag_fmt(format_args!(
                "virtio FAIL step=transport version={version} need -global virtio-mmio.force-legacy=false"
            ));
            return None;
        }
    };
    let id = t.device_id();
    if id != VIRTIO_ID_CONSOLE {
        diag_fmt(format_args!("virtio FAIL step=id id={id}"));
        return None;
    }

    // Step 4: VERSION_1 and nothing else (spec C3). No optional feature is
    // requested, so no field of the device configuration is valid and none is
    // read. The firmware used this device before the kernel started;
    // `negotiate` begins with a reset, which is what takes it back.
    let features = match negotiate(&mut t, VCON_WANTED_FEATURES) {
        Ok(f) => f,
        Err(NegotiateError::NoVersion1 { offered }) => {
            diag_fmt(format_args!(
                "virtio FAIL step=features offered={offered:#x}"
            ));
            return None;
        }
        Err(NegotiateError::FeaturesRejected { status }) => {
            diag_fmt(format_args!(
                "virtio FAIL step=features rejected status={status:#x}"
            ));
            return None;
        }
        Err(NegotiateError::ResetIncomplete { status }) => {
            diag_fmt(format_args!(
                "virtio FAIL step=features reset status={status:#x}"
            ));
            return None;
        }
    };

    // Step 5: size, lay out and enable both queues, then DRIVER_OK. Without
    // the multiport feature the device has exactly these two.
    let mut sizes = [0u16; 2];
    for (queue, size) in [RECEIVE_QUEUE, TRANSMIT_QUEUE].into_iter().zip(&mut sizes) {
        let device_max = t.queue_max(queue);
        let Some(clamped) = clamp_queue_size(device_max) else {
            fail(&mut t);
            diag_fmt(format_args!(
                "virtio FAIL step=queue q={queue} max={device_max}"
            ));
            return None;
        };
        *size = clamped;
    }
    let [rx_size, tx_size] = sizes;
    let qlayouts = queue_layout(rx_size).zip(queue_layout(tx_size));
    let (Some((rx_layout, tx_layout)), Some(vlayout)) = (
        qlayouts,
        qlayouts.and_then(|(rx, tx)| vcon_layout(region, rx, tx)),
    ) else {
        fail(&mut t);
        diag_fmt(format_args!(
            "virtio FAIL step=queue rxq={rx_size} txq={tx_size} len={}",
            run.len
        ));
        return None;
    };
    // Each queue lives on its own page of the run, split off before any buffer
    // existed and moved into its queue here, so nothing else in this driver can
    // write a descriptor. The device-visible addresses come out of those
    // `QueueMem`s, which is why enabling a queue is safe code.
    let Some(mut rxq) = VirtQueue::new(vlayout.rxq, rx_layout) else {
        fail(&mut t);
        diag_fmt(format_args!(
            "virtio FAIL step=queue q={RECEIVE_QUEUE} size={rx_size}"
        ));
        return None;
    };
    let Some(txq) = VirtQueue::new(vlayout.txq, tx_layout) else {
        fail(&mut t);
        diag_fmt(format_args!(
            "virtio FAIL step=queue q={TRANSMIT_QUEUE} size={tx_size}"
        ));
        return None;
    };
    t.queue_enable(RECEIVE_QUEUE, &rxq);
    t.queue_enable(TRANSMIT_QUEUE, &txq);
    if let Err(status) = driver_ok(&mut t) {
        fail(&mut t);
        diag_fmt(format_args!("virtio FAIL step=status status={status:#x}"));
        return None;
    }

    // Step 6: the line, and that the allow-list holds exactly that one. The
    // claim leaves the line unmasked; every delivery masks it until `rearm`.
    let line = match IrqLine::new(intid, VCON_IRQ_NOTIFY_ID) {
        Ok(line) => line,
        Err(rc) => {
            fail(&mut t);
            diag_fmt(format_args!("virtio FAIL step=irq intid={intid} rc={rc}"));
            return None;
        }
    };
    let neighbour = intid.saturating_add(1);
    match IrqLine::new(neighbour, VCON_IRQ_NOTIFY_ID) {
        Err(EPERM) => {}
        Err(rc) => {
            fail(&mut t);
            diag_fmt(format_args!(
                "virtio FAIL step=irq-deny intid={neighbour} rc={rc}"
            ));
            return None;
        }
        Ok(_) => {
            fail(&mut t);
            diag_fmt(format_args!(
                "virtio FAIL step=irq-deny intid={neighbour} rc={OK}"
            ));
            return None;
        }
    }

    // Step 7: give the device its one receive buffer. From here input can
    // arrive; its interrupt becomes a notification the main loop receives.
    if !post_rx(&mut t, &mut rxq, vlayout.rx) {
        fail(&mut t);
        diag_fmt(format_args!(
            "virtio FAIL step=rx-post free={}",
            rxq.num_free()
        ));
        return None;
    }

    // Step 8.
    diag_fmt(format_args!(
        "virtio ok features={features:#x} rxq={rx_size} txq={tx_size} slot={slot}"
    ));
    Some(Port {
        t,
        rxq,
        txq,
        line,
        rx_buf: vlayout.rx,
        tx_buf: vlayout.tx,
        rx: Rx::new(),
        tx: Tx::new(),
        broken: false,
    })
}

/// Reply to a SENDREC caller: stamp `m_type`, zero `m_source` (the kernel
/// overwrites it on delivery), and SEND the message back. A copy of TTY's, for the
/// reason TTY's exists.
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
