// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs `virtio-blk` driver — the disk behind the BDEV band, and the first
//! consumer of `driver-rt` (slices 6.2 and 6.3).
//!
//! The kernel's boot probe assigns this driver a virtio-mmio transport, pre-maps
//! that register page and a contiguous DMA run into this address space, and puts
//! the transport's interrupt line on this driver's allow-list. The driver brings
//! the device up, decides whether it carries a minix.rs root, and then serves
//! `BDEV_READ` and `BDEV_WRITE` from the device's request queue, one request at a
//! time, each completed by a real interrupt.
//!
//! ## Publishing, then the verdict
//!
//! Two things a client needs are learned at different times, and they are kept
//! apart on purpose (spec B1):
//!
//! - **Whether there is a device** is one kernel call. `vblk_init`, the
//!   `init_fresh` callback, asks `GET_VIRTIO_MMIO` and publishes this driver to DS
//!   if the kernel assigned it a transport; with no disk attached (`ENOENT`) it
//!   publishes nothing. Nothing in that callback can block on the device.
//! - **Whether the device carries a root** takes two reads, each a completion
//!   interrupt the driver blocks for. [`bring_up`] and [`root_check`] run after
//!   `sef_startup`, before the receive loop.
//!
//! Publishing only after the verdict would lose a race on every boot: while this
//! driver blocked for its first completion, MFS — next in packing order — would
//! ask DS, find nothing, and mount the ramdisk. Publishing first closes it,
//! because a client's first SENDREC then blocks in the kernel until this driver
//! reaches its receive loop, which is after the verdict.
//!
//! ## Bring-up
//!
//! [`bring_up`] runs once and prints exactly one verdict line:
//!
//! 1. `GET_VIRTIO_MMIO` — `ENOENT` means no disk is attached: `no device`.
//! 2. `GET_DMA`; check and clear the kernel's cookie.
//! 3. `MmioTransport::new` (magic, version 2); `DeviceID` must be 2.
//! 4. `negotiate` with no optional feature — `VIRTIO_F_VERSION_1` alone.
//! 5. Read `capacity`, generation-stable.
//! 6. Lay out and enable queue 0; `DRIVER_OK`, read back.
//! 7. Claim the INTID; a claim on `INTID + 1` must answer `EPERM`.
//! 8. `virtio ok features=… capacity=… qsize=… slot=…`.
//!
//! A failing step prints `virtio FAIL step=<step> …` and abandons the rest without
//! resetting the device or releasing anything; a failure after `negotiate`
//! succeeded also sets `FAILED` through `driver-rt`'s `fail`. No bring-up step
//! waits on a register or a ring. On success the transport, the queue, the
//! interrupt line and the request buffers are kept in a [`Disk`] in `main`'s
//! frame for the life of the process.
//!
//! [`root_check`] then reads block 0 and the last block through [`serve`] — the
//! same path a client request takes, so the interrupt path is proved before the
//! first client and by the same code — and applies `drivers/memory`'s device-level
//! checks: the image label, the header's block count against the device's, the
//! tail label. It prints `root ok blocks=<n> tail=1` or `root FAIL <what>`.
//!
//! ## One request in flight
//!
//! [`serve`] builds one request in the DMA run (a 16-byte header, the one
//! 4096-byte bounce page, a status byte preset to `0xFF`), publishes the chain,
//! notifies the device, and then blocks in a receive **filtered to `HARDWARE`**
//! until that request's own head has come back. The filter is what makes "one in
//! flight" need no queue here: a second client's request stays a blocked sender
//! in the kernel, and an RS ping stays pending, until the main loop receives
//! again. Each interrupt is handled in the order `driver-rt` documents —
//! read `InterruptStatus`, acknowledge exactly those bits, walk the used ring,
//! re-arm the line — by [`on_interrupt`], which the main loop also uses for an
//! interrupt that arrives with nothing in flight (a configuration change is
//! legal, and is otherwise ignored).
//!
//! A device that never completes a request hangs the driver in that receive,
//! with its client in SENDREC behind it; RS's unanswered heartbeat is the only
//! report. There is no timeout because no user-space driver holds an alarm.
//!
//! ## Blocks and sectors
//!
//! BDEV's unit is the 4096-byte block and the device's is the 512-byte sector, so
//! every device transfer is eight sectors through the bounce page. A read copies
//! the client's `len` bytes out of it with `SAFECOPY_TO`. A whole-block write
//! copies the client's bytes in with `SAFECOPY_FROM` first, so a bad grant never
//! touches the device. A partial write reads the block, copies the client's bytes
//! over its start, and writes it back: the rest of the block survives, which is
//! what `drivers/memory` gives the same request.
//!
//! ## What a client hears
//!
//! - **`ENXIO`** for every BDEV request when there is no root — no device, a
//!   failed bring-up, or labels that did not match. Minor 0 is "the root
//!   filesystem this driver serves", and there is none.
//! - **`ENXIO` / `EINVAL`** for a malformed request, decided by `server-rt`'s
//!   `bdev::validate_read`, which `drivers/memory` shares so both refuse alike.
//! - **`EIO`** when the device failed a well-formed request: a status byte other
//!   than `0` (including one the device never wrote), or a broken device.
//! - A refused grant or an unmapped buffer is relayed **verbatim** from
//!   `SYS_SAFECOPY`.
//! - **`ENOSYS`** for any other request number.
//!
//! A device that breaks the used-ring contract (`driver-rt`'s `QueueFault`) is
//! marked `FAILED`, reported once as `virtio FAIL step=io …`, and never used
//! again: its line stays masked and every later request answers `EIO`.
//!
//! The driver never exits: the kernel halts if a process holding a virtio
//! assignment exits or execs (the DMA-run debt). The granter of every copy is the
//! kernel-stamped `m_source`, never a payload field.

// Freestanding for the real (bare-metal) build, but a normal host binary under
// `cargo test` so `bringup`'s logic gets host-runnable unit tests. The test
// harness needs `std` and its own entry point, so `no_std`/`no_main` and the
// `_start` shim below are all gated to `not(test)`.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

minixrs_abi_note::brand!();

mod blk;
mod bringup;

use bringup::{
    VBLK_CONFIG_CAPACITY_OFF, VBLK_IRQ_NOTIFY_ID, VBLK_WANTED_FEATURES, capacity_from_config,
    cookie_ok, vblk_layout,
};
use minixrs_driver_rt::transport::{NegotiateError, config_read, driver_ok, fail, negotiate};
use minixrs_driver_rt::{
    DmaBuf, DmaRegion, DmaRun, IrqLine, MmioError, MmioTransport, Transport, VirtQueue,
    clamp_queue_size, queue_layout,
};
use minixrs_ipc::{ipc_receive, ipc_send};
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    BDEV_READ, BDEV_WRITE, GET_DMA, GET_VIRTIO_MMIO, GETINFO_DMA_LEN_OFF, GETINFO_DMA_PA_OFF,
    GETINFO_DMA_VA_OFF, GETINFO_VIRTIO_INTID_OFF, GETINFO_VIRTIO_SLOT_OFF, GETINFO_VIRTIO_VA_OFF,
    NOTIFY_IRQ_PENDING_OFF, SAFECOPY_FROM, SAFECOPY_TO, SYS_GETINFO_NAME_LEN,
};
use minixrs_kernel_shared::com::{HARDWARE, boot_endpoint};
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{EINVAL, EIO, ENOENT, ENOSYS, ENXIO, EPERM, OK};
use minixrs_kernel_shared::ipc_const::NOTIFY_MESSAGE;
use minixrs_kernel_shared::rootfs::IMAGE_HDR_LEN;
use minixrs_kernel_shared::virtio::VIRTIO_ID_BLOCK;
use minixrs_server_rt::bdev::{parse_read, validate_read};
use minixrs_server_rt::{
    SefConfig, diag_fmt, rd_i32, rd_u64, sef_publish_to_ds, sef_startup, sys_getinfo, sys_safecopy,
};

/// The queue virtio-blk uses (its only one: `requestq`).
const REQUEST_QUEUE: u16 = 0;

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

/// A device that came up: everything a request needs, kept for the life of the
/// process in `main`'s frame.
struct Disk {
    t: MmioTransport,
    queue: VirtQueue,
    line: IrqLine,
    /// The 16-byte request header (device-readable).
    header: DmaBuf,
    /// The status byte (device-writable), preset before every request.
    status: DmaBuf,
    /// The one 4096-byte bounce page: a block on its way to or from the device.
    data: DmaBuf,
    /// The device's size in 512-byte sectors.
    capacity: u64,
    /// The device broke the ring contract (or the driver could no longer wait
    /// for it). Set once by [`break_device`]; every later request is `EIO`.
    broken: bool,
}

/// What this driver serves as BDEV minor 0 (spec B1, H6).
#[derive(Clone, Copy)]
enum Root {
    /// No device, a failed bring-up, or a device that is not a minix.rs root:
    /// every BDEV request answers `ENXIO`.
    None,
    /// A verified root of `blocks` BDEV blocks.
    Ready { blocks: u64 },
}

// Only `_start` calls `main`; under `cargo test` `_start` is gone, so `main` (and
// the helpers it alone reaches) would read as dead code.
#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    let sef = sef_startup(SefConfig {
        init_fresh: Some(vblk_init),
        signal_handler: None,
    })
    .unwrap_or_else(|_| {
        // No recovery and nothing to print: a failed handshake means this driver
        // never learned its own endpoint, so it cannot even announce the failure.
        loop {
            core::hint::spin_loop()
        }
    });

    // After `sef_startup`, not inside `vblk_init`: both block on the device, and
    // the DS publish must precede every step that can (spec B1). The first client
    // request cannot be answered early either — it waits in the kernel until the
    // loop below receives, which is after the verdict.
    let mut disk = bring_up();
    let root = match disk.as_mut() {
        Some(disk) => root_check(disk),
        None => Root::None,
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
        // never by payload, and gets no reply. Nothing is in flight here (`serve`
        // does not return while a request is), so this is at most a
        // configuration-change interrupt: acknowledge, walk, re-arm, ignore. A
        // used element now would surface from the walk as a queue fault. A broken
        // device's line is left masked.
        if msg.m_source == boot_endpoint(HARDWARE) && msg.m_type == NOTIFY_MESSAGE {
            if let Some(disk) = disk.as_mut() {
                // The kernel writes the word little-endian; `rd_i32` reads
                // native, which on aarch64 is the same thing.
                let pending = rd_i32(&msg, NOTIFY_IRQ_PENDING_OFF) as u32;
                if !disk.broken && disk.line.fired(pending) {
                    on_interrupt(disk, None);
                }
            }
            continue;
        }
        // Any other notification has no sender waiting in a SENDREC: drop it.
        if msg.m_type == NOTIFY_MESSAGE {
            continue;
        }
        // Capture the caller first: it is both the reply target and the granter
        // (never a payload field), and the reply overwrites `msg.m_source`.
        let caller_e = msg.m_source;
        let rc = match msg.m_type {
            BDEV_READ | BDEV_WRITE => match (disk.as_mut(), root) {
                (Some(disk), Root::Ready { blocks }) if msg.m_type == BDEV_READ => {
                    do_read(disk, blocks, caller_e, &msg)
                }
                (Some(disk), Root::Ready { blocks }) => do_write(disk, blocks, caller_e, &msg),
                // No root: minor 0 does not exist on this driver, whatever else
                // the request says (spec B1).
                _ => ENXIO,
            },
            // Unlike DS, reply rather than drop — the caller is inside a SENDREC
            // and would otherwise block forever.
            _ => ENOSYS,
        };
        reply(caller_e, &mut msg, rc);
    }
}

/// SEF fresh-init callback: publish to DS if, and only if, the kernel assigned
/// this driver a device (spec B1).
///
/// One kernel call decides it, and nothing here touches the device: the publish
/// has to precede every step that can block, or a client that looks `vblk` up
/// while bring-up waits for a completion finds nothing. With no disk attached
/// (`ENOENT`) — or any other refusal — nothing is published, and [`bring_up`]
/// asks again and reports it.
///
/// Always `OK`: a non-`OK` return fails `sef_startup` and leaves the driver
/// spinning with no way to say why. A failed publish is reported instead (the
/// debug channel is up before this callback runs), and the driver carries on
/// undiscoverable.
#[cfg_attr(test, allow(dead_code))]
fn vblk_init(_endpoint: Endpoint, name: &[u8; SYS_GETINFO_NAME_LEN]) -> i32 {
    let mut m = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };
    if sys_getinfo(GET_VIRTIO_MMIO, &mut m) == OK {
        let rc = sef_publish_to_ds(name);
        if rc != OK {
            diag_fmt(format_args!("ds FAIL rc={rc}"));
        }
    }
    OK
}

/// Stop using the device: write `FAILED`, remember it, and say why — once. The
/// interrupt line is left masked by not re-arming it.
#[cfg_attr(test, allow(dead_code))]
fn break_device(disk: &mut Disk, why: core::fmt::Arguments<'_>) {
    if disk.broken {
        return;
    }
    disk.broken = true;
    fail(&mut disk.t);
    diag_fmt(format_args!("virtio FAIL step=io {why}"));
}

/// Handle one interrupt on the device's line, in `driver-rt`'s documented order:
/// read `InterruptStatus`, acknowledge exactly those bits, walk the used ring,
/// re-arm. Returns whether the chain headed at `expect` completed.
///
/// `expect` is the one request in flight, or `None` when nothing is. Any other
/// used element is a fault: the queue reports an id that is out of range, not in
/// flight, or published with nothing in flight, and the walk stops at once — a
/// fault is sticky, so popping again would only report it again. The device is
/// then broken and its line stays masked.
///
/// On a healthy device the line is re-armed after every interrupt, including one
/// whose walk completed nothing; without that the next completion never arrives.
#[cfg_attr(test, allow(dead_code))]
fn on_interrupt(disk: &mut Disk, expect: Option<u16>) -> bool {
    // The acknowledgement precedes the walk: `InterruptStatus` is a level, and
    // an ack after the walk could clear a completion the walk never saw.
    let bits = disk.t.interrupt_status();
    disk.t.interrupt_ack(bits);

    let mut completed = false;
    loop {
        match disk.queue.pop_used() {
            Ok(Some((head, _))) if Some(head) == expect && !completed => completed = true,
            // Unreachable with one chain in flight: the queue itself refuses a
            // head that is not, as `NotInFlight`.
            Ok(Some((head, _))) => {
                break_device(disk, format_args!("head={head}"));
                return completed;
            }
            Ok(None) => break,
            Err(fault) => {
                break_device(disk, format_args!("{fault:?}"));
                return completed;
            }
        }
    }

    // A line that cannot be re-armed delivers nothing more, so the next request
    // would wait forever: that device is unusable too.
    let rc = disk.line.rearm();
    if rc != OK {
        break_device(disk, format_args!("rearm rc={rc}"));
    }
    completed
}

/// Carry out one device request — `req_type` is `VIRTIO_BLK_T_IN` (the device
/// fills the bounce page from `sector`) or `VIRTIO_BLK_T_OUT` (the device stores
/// the bounce page at `sector`) — and wait for its completion. `Ok(())` when the
/// device reported success; `Err(EIO)` for a device error, a status byte the
/// device never wrote, or a broken device.
///
/// One request is in flight at a time, and this does not return until it has
/// completed (spec B3). The wait is a receive filtered to `HARDWARE`: blocked,
/// never polling, and blind to every other sender, which is what keeps a second
/// client queued in the kernel instead of in this driver. A device that never
/// completes hangs the driver here.
#[cfg_attr(test, allow(dead_code))]
fn serve(disk: &mut Disk, req_type: u32, sector: u64) -> Result<(), i32> {
    if disk.broken {
        return Err(EIO);
    }
    let write = match req_type {
        blk::VIRTIO_BLK_T_IN => false,
        blk::VIRTIO_BLK_T_OUT => true,
        // No caller passes anything else; the device is not touched.
        _ => return Err(EINVAL),
    };

    // The status byte is preset to a value the device never reports, so a
    // completion that left it alone is told apart from a success.
    let header = blk::encode_header(req_type, sector);
    if !disk.header.write_from(0, &header) || !disk.status.write_from(0, &[blk::STATUS_UNWRITTEN]) {
        // Unreachable: `vblk_layout` sized both buffers for exactly these writes.
        break_device(disk, format_args!("buffers"));
        return Err(EIO);
    }

    // Device-readable buffers first, then device-writable ones.
    let head = if write {
        disk.queue.add(&[disk.header, disk.data], &[disk.status])
    } else {
        disk.queue.add(&[disk.header], &[disk.data, disk.status])
    };
    let Some(head) = head else {
        // With nothing in flight all eight descriptors are free, so this is a
        // queue whose bookkeeping no longer adds up.
        break_device(disk, format_args!("add free={}", disk.queue.num_free()));
        return Err(EIO);
    };
    disk.t.notify(REQUEST_QUEUE);

    let mut m = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };
    loop {
        let rc = ipc_receive(boot_endpoint(HARDWARE), &mut m);
        if rc != OK {
            // Never retried in a loop: a receive that fails once would fail
            // again at once, and that is a spin. The request stays in flight on
            // a device nobody is listening to, so the device is finished.
            break_device(disk, format_args!("receive rc={rc}"));
            return Err(EIO);
        }
        // Another line's bit (this driver claims only one) is not ours to handle.
        let pending = rd_i32(&m, NOTIFY_IRQ_PENDING_OFF) as u32;
        if !disk.line.fired(pending) {
            continue;
        }
        let completed = on_interrupt(disk, Some(head));
        if disk.broken {
            return Err(EIO);
        }
        if completed {
            break;
        }
    }

    let mut status = [blk::STATUS_UNWRITTEN];
    if !disk.status.read_into(0, &mut status) {
        // Unreachable: the same byte was written above.
        return Err(EIO);
    }
    blk::status_errno(status[0])
}

/// Decide whether the device carries a minix.rs root, and say so (spec B6).
///
/// `drivers/memory`'s `selfcheck`, through the queue: block 0 must start with the
/// image label and carry the device's own block count, and the last block must
/// start with the tail label. Device-level only — no superblock is decoded.
/// Prints `root ok blocks=<n> tail=1` or one `root FAIL <what>` line.
///
/// Both reads go through [`serve`], so the label arriving in the bounce page also
/// proves the page the device writes by DMA is the memory this driver reads.
#[cfg_attr(test, allow(dead_code))]
fn root_check(disk: &mut Disk) -> Root {
    let capacity = disk.capacity;
    let geometry = blk::device_blocks(capacity).and_then(|blocks| {
        let tail_sector = blk::block_to_sector(blk::tail_block(blocks)?)?;
        Some((blocks, tail_sector))
    });
    let Some((blocks, tail_sector)) = geometry else {
        diag_fmt(format_args!("root FAIL geometry capacity={capacity}"));
        return Root::None;
    };

    let mut hdr = [0u8; IMAGE_HDR_LEN];

    let head = serve(disk, blk::VIRTIO_BLK_T_IN, 0).and_then(|()| read_hdr(disk, &mut hdr));
    if let Err(rc) = head {
        diag_fmt(format_args!("root FAIL head rc={rc}"));
        return Root::None;
    }
    if let Err(why) = blk::check_head(&hdr, blocks) {
        report_root_fail(why);
        return Root::None;
    }

    let tail =
        serve(disk, blk::VIRTIO_BLK_T_IN, tail_sector).and_then(|()| read_hdr(disk, &mut hdr));
    if let Err(rc) = tail {
        diag_fmt(format_args!("root FAIL tail rc={rc}"));
        return Root::None;
    }
    if let Err(why) = blk::check_tail(&hdr) {
        report_root_fail(why);
        return Root::None;
    }

    diag_fmt(format_args!("root ok blocks={blocks} tail=1"));
    Root::Ready { blocks }
}

/// Copy the start of the bounce page out for a label check.
#[cfg_attr(test, allow(dead_code))]
fn read_hdr(disk: &Disk, hdr: &mut [u8; IMAGE_HDR_LEN]) -> Result<(), i32> {
    // The `Err` is unreachable: the bounce page is a whole block.
    if disk.data.read_into(0, hdr) {
        Ok(())
    } else {
        Err(EIO)
    }
}

/// Print the `root FAIL` line for a failed label or block-count check.
#[cfg_attr(test, allow(dead_code))]
fn report_root_fail(why: blk::RootFail) {
    match why {
        blk::RootFail::Label => diag_fmt(format_args!("root FAIL label")),
        blk::RootFail::Blocks { hdr, dev } => {
            diag_fmt(format_args!("root FAIL blocks hdr={hdr} dev={dev}"))
        }
        blk::RootFail::TailLabel => diag_fmt(format_args!("root FAIL tail label")),
    }
}

/// Serve one `BDEV_READ`. Returns the reply `m_type`: the byte count read
/// (`>= 0`), or a negative errno.
///
/// The device fills the bounce page with the whole block; the client's `len`
/// bytes are then pushed into its granted buffer. `caller_e` is the
/// kernel-stamped source of this very message — the granter can be nothing else —
/// and the grant offset is `0`, as on `drivers/memory`.
#[cfg_attr(test, allow(dead_code))]
fn do_read(disk: &mut Disk, blocks: u64, caller_e: Endpoint, msg: &Message) -> i32 {
    let req = parse_read(msg);
    let n = match validate_read(req, blocks) {
        Ok((_, n)) => n,
        Err(e) => return e,
    };
    if n == 0 {
        // A legal zero-length read: no grant use, no device I/O. This is the
        // request a client probes the device with.
        return 0;
    }
    // Unreachable after validation (`block < blocks`, and `blocks * 8` is the
    // device's own capacity), but checked.
    let Some(sector) = blk::block_to_sector(req.block) else {
        return EINVAL;
    };
    if let Err(rc) = serve(disk, blk::VIRTIO_BLK_T_IN, sector) {
        return rc;
    }
    let rc = sys_safecopy(SAFECOPY_TO, caller_e, req.gid, 0, disk.data.va(), n as u64);
    if rc != OK {
        // Verbatim: EPERM ("your grant does not authorize this") and EFAULT ("your
        // buffer is not mapped") are different bugs on the client's side.
        return rc;
    }
    n as i32
}

/// Serve one `BDEV_WRITE`. Returns the reply `m_type`: the byte count stored
/// (`>= 0`), or a negative errno.
///
/// A whole block is copied from the client into the bounce page and written. A
/// partial block is read from the device first, so the bytes after the client's
/// `len` survive the write-back (spec B4). Either way the client's bytes are
/// taken *before* the device write: a refused grant or an unmapped buffer is
/// relayed verbatim and the device is never written.
#[cfg_attr(test, allow(dead_code))]
fn do_write(disk: &mut Disk, blocks: u64, caller_e: Endpoint, msg: &Message) -> i32 {
    let req = parse_read(msg);
    let n = match validate_read(req, blocks) {
        Ok((_, n)) => n,
        Err(e) => return e,
    };
    let plan = blk::write_plan(n);
    if plan == blk::WritePlan::Empty {
        // A legal zero-length write: no grant use, no device I/O.
        return 0;
    }
    // Unreachable after validation, but checked (see `do_read`).
    let Some(sector) = blk::block_to_sector(req.block) else {
        return EINVAL;
    };
    if plan == blk::WritePlan::Partial
        && let Err(rc) = serve(disk, blk::VIRTIO_BLK_T_IN, sector)
    {
        return rc;
    }
    let rc = sys_safecopy(
        SAFECOPY_FROM,
        caller_e,
        req.gid,
        0,
        disk.data.va(),
        n as u64,
    );
    if rc != OK {
        // Verbatim, as in `do_read`. Nothing has been written to the device.
        return rc;
    }
    if let Err(rc) = serve(disk, blk::VIRTIO_BLK_T_OUT, sector) {
        return rc;
    }
    n as i32
}

/// Run the eight bring-up steps once, printing one verdict line: `no device`, the
/// first failing step's `virtio FAIL step=…`, or `virtio ok …`. `Some` only for
/// the last: the device, its queue, its line and its request buffers.
#[cfg_attr(test, allow(dead_code))]
fn bring_up() -> Option<Disk> {
    let mut m = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };

    // Step 1: where the registers are. ENOENT is the healthy no-disk path.
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
    // `DmaRegion` built here and the `DmaBuf`s it hands out.
    let region = unsafe { DmaRegion::new(run) };
    // The cookie is the run's first eight bytes; the stamp is 8-aligned (T7).
    let with_cookie = region
        .filter(|_| run.va.is_multiple_of(8))
        .and_then(|region| Some((region.buf(0, 8)?, region)));
    let Some((cookie, region)) = with_cookie else {
        diag_fmt(format_args!(
            "virtio FAIL step=dma va={:#x} len={}",
            run.va, run.len
        ));
        return None;
    };
    // Volatile byte copies through the `DmaBuf`: the memory is shared with the
    // kernel's stamp and, from step 6 on, with the device. The kernel writes the
    // word in its native order, which on aarch64 is little-endian.
    let mut word = [0u8; 8];
    let read = cookie.read_into(0, &mut word);
    let word = u64::from_le_bytes(word);
    if !read || !cookie_ok(word, run.pa) {
        diag_fmt(format_args!(
            "virtio FAIL step=dma cookie={word:#x} pa={:#x}",
            run.pa
        ));
        return None;
    }
    if !cookie.write_from(0, &[0u8; 8]) {
        // Unreachable: the same eight bytes were just read.
        return None;
    }

    // Step 3: the transport, and that it is the disk.
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
    if id != VIRTIO_ID_BLOCK {
        diag_fmt(format_args!("virtio FAIL step=id id={id}"));
        return None;
    }

    // Step 4: VERSION_1 and nothing else.
    let features = match negotiate(&mut t, VBLK_WANTED_FEATURES) {
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

    // Step 5: capacity. An unstable read is not reported as a capacity.
    let mut cap = [0u8; 8];
    if !config_read(&t, VBLK_CONFIG_CAPACITY_OFF, &mut cap) {
        fail(&mut t);
        diag_fmt(format_args!("virtio FAIL step=config"));
        return None;
    }
    let capacity = capacity_from_config(cap);

    // Step 6: lay out and enable the request queue, then DRIVER_OK.
    let device_max = t.queue_max(REQUEST_QUEUE);
    let Some(qsize) = clamp_queue_size(device_max) else {
        fail(&mut t);
        diag_fmt(format_args!("virtio FAIL step=queue max={device_max}"));
        return None;
    };
    let qlayout = queue_layout(qsize);
    let (Some(qlayout), Some(vlayout)) = (qlayout, qlayout.and_then(|q| vblk_layout(&region, q)))
    else {
        fail(&mut t);
        diag_fmt(format_args!(
            "virtio FAIL step=queue size={qsize} len={}",
            run.len
        ));
        return None;
    };
    // The queue lives on its own page of the run. Its three device-visible
    // addresses come out of that `DmaBuf`, which is why enabling it is safe code.
    let Some(queue) = VirtQueue::new(vlayout.queue, qlayout) else {
        fail(&mut t);
        diag_fmt(format_args!("virtio FAIL step=queue size={qsize}"));
        return None;
    };
    t.queue_enable(REQUEST_QUEUE, &queue);
    if let Err(status) = driver_ok(&mut t) {
        fail(&mut t);
        diag_fmt(format_args!("virtio FAIL step=status status={status:#x}"));
        return None;
    }

    // Step 7: the line, and that the allow-list holds exactly that one. The
    // claim leaves the line unmasked; every delivery masks it until `rearm`.
    let line = match IrqLine::new(intid, VBLK_IRQ_NOTIFY_ID) {
        Ok(line) => line,
        Err(rc) => {
            diag_fmt(format_args!("virtio FAIL step=irq intid={intid} rc={rc}"));
            return None;
        }
    };
    let neighbour = intid.saturating_add(1);
    match IrqLine::new(neighbour, VBLK_IRQ_NOTIFY_ID) {
        Err(EPERM) => {}
        Err(rc) => {
            diag_fmt(format_args!(
                "virtio FAIL step=irq-deny intid={neighbour} rc={rc}"
            ));
            return None;
        }
        Ok(_) => {
            diag_fmt(format_args!(
                "virtio FAIL step=irq-deny intid={neighbour} rc={OK}"
            ));
            return None;
        }
    }

    // Step 8.
    diag_fmt(format_args!(
        "virtio ok features={features:#x} capacity={capacity} qsize={qsize} slot={slot}"
    ));
    Some(Disk {
        t,
        queue,
        line,
        header: vlayout.header,
        status: vlayout.status,
        data: vlayout.data,
        capacity,
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
