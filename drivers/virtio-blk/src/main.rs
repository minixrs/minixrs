// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs `virtio-blk` driver — the first consumer of `driver-rt` (slice 6.2,
//! spec T12).
//!
//! In 6.2 this driver brings its device up and idles. The kernel's boot probe
//! assigned it a virtio-mmio transport, pre-mapped that register page and a
//! contiguous DMA run into this address space, and put the transport's interrupt
//! line on this driver's allow-list. [`bring_up`] runs the eight steps of T12 once,
//! after `sef_startup`, and prints exactly one verdict line:
//!
//! 1. `GET_VIRTIO_MMIO` — `ENOENT` means no disk is attached: `no device`, idle.
//! 2. `GET_DMA`; check and clear the kernel's cookie (T7).
//! 3. `MmioTransport::new` (magic, version 2); `DeviceID` must be 2.
//! 4. `negotiate` with no optional feature — `VIRTIO_F_VERSION_1` alone.
//! 5. Read `capacity`, generation-stable.
//! 6. Lay out and enable queue 0; `DRIVER_OK`, read back.
//! 7. Claim the INTID; a claim on `INTID + 1` must answer `EPERM`.
//! 8. `virtio ok features=… capacity=… qsize=… slot=…`.
//!
//! A failing step prints `virtio FAIL step=<step> …` and abandons the rest without
//! resetting the device or releasing anything; a failure after `negotiate`
//! succeeded (config, queue, or a refused `DRIVER_OK`) also sets `FAILED` through
//! `driver-rt`'s `fail`. The driver still enters its receive loop, degraded. It
//! never exits: the kernel halts if a process holding a virtio assignment exits
//! or execs (the DMA-run debt, T7).
//!
//! The loop publishes nothing to DS (6.3 decides who carries a root) and answers
//! every request `ENOSYS`. Nothing is submitted in 6.2, so a `HARDWARE`
//! notification is unexpected: it is reported once as `step=irq-extra` and the
//! line is left masked. The steady-state interrupt order every virtio driver
//! follows is spelled once, in `driver-rt`'s crate documentation.
//!
//! No step waits on a register or a ring (I12): bring-up is straight-line.

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
    DmaRegion, DmaRun, IrqLine, MmioError, MmioTransport, Transport, VirtQueue, clamp_queue_size,
    queue_layout,
};
use minixrs_ipc::ipc_send;
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    GET_DMA, GET_VIRTIO_MMIO, GETINFO_DMA_LEN_OFF, GETINFO_DMA_PA_OFF, GETINFO_DMA_VA_OFF,
    GETINFO_VIRTIO_INTID_OFF, GETINFO_VIRTIO_SLOT_OFF, GETINFO_VIRTIO_VA_OFF,
    NOTIFY_IRQ_PENDING_OFF,
};
use minixrs_kernel_shared::com::{HARDWARE, boot_endpoint};
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::error::{ENOENT, ENOSYS, EPERM, OK};
use minixrs_kernel_shared::ipc_const::NOTIFY_MESSAGE;
use minixrs_kernel_shared::virtio::VIRTIO_ID_BLOCK;
use minixrs_server_rt::{SefConfig, diag_fmt, rd_i32, rd_u64, sef_startup, sys_getinfo};

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

// Only `_start` calls `main`; under `cargo test` `_start` is gone, so `main` (and
// the helpers it alone reaches) would read as dead code.
#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    // `init_fresh: None`: nothing is published to DS in 6.2.
    let sef = sef_startup(SefConfig {
        init_fresh: None,
        signal_handler: None,
    })
    .unwrap_or_else(|_| {
        // No recovery and nothing to print: a failed handshake means this driver
        // never learned its own endpoint, so it cannot even announce the failure.
        loop {
            core::hint::spin_loop()
        }
    });

    // After `sef_startup`, not inside an `init_fresh` callback, where a non-OK
    // return would fail startup and leave the driver spinning (T12).
    bring_up();

    let mut irq_extra_reported = false;
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
        // never by payload, and gets no reply. Nothing was submitted, so any
        // interrupt is unexpected: report it once and do not re-arm.
        if msg.m_source == boot_endpoint(HARDWARE) && msg.m_type == NOTIFY_MESSAGE {
            if !irq_extra_reported {
                irq_extra_reported = true;
                // The kernel writes the word little-endian; `rd_i32` reads
                // native, which on aarch64 is the same thing.
                let bits = rd_i32(&msg, NOTIFY_IRQ_PENDING_OFF) as u32;
                diag_fmt(format_args!("virtio FAIL step=irq-extra bits={bits:#x}"));
            }
            continue;
        }
        // Any other notification has no sender waiting in a SENDREC: drop it.
        if msg.m_type == NOTIFY_MESSAGE {
            continue;
        }
        // Capture the caller first: the reply overwrites `msg.m_source`.
        let caller_e = msg.m_source;
        // Unlike DS, reply rather than drop — the caller is inside a SENDREC and
        // would otherwise block forever.
        reply(caller_e, &mut msg, ENOSYS);
    }
}

/// Run T12's eight steps once, printing one verdict line: `no device`, the first
/// failing step's `virtio FAIL step=…`, or `virtio ok …`.
#[cfg_attr(test, allow(dead_code))]
fn bring_up() {
    let mut m = Message {
        m_source: 0,
        m_type: 0,
        payload: [0u8; 96],
    };

    // Step 1: where the registers are. ENOENT is H5's healthy no-disk path.
    let rc = sys_getinfo(GET_VIRTIO_MMIO, &mut m);
    if rc == ENOENT {
        diag_fmt(format_args!("no device"));
        return;
    }
    if rc != OK {
        diag_fmt(format_args!("virtio FAIL step=getinfo rc={rc}"));
        return;
    }
    let regs_va = rd_u64(&m, GETINFO_VIRTIO_VA_OFF);
    let intid = rd_i32(&m, GETINFO_VIRTIO_INTID_OFF) as u32;
    let slot = rd_i32(&m, GETINFO_VIRTIO_SLOT_OFF) as u32;

    // Step 2: the DMA run, and the proof that its VA and PA are the same memory.
    let rc = sys_getinfo(GET_DMA, &mut m);
    if rc != OK {
        diag_fmt(format_args!("virtio FAIL step=dma rc={rc}"));
        return;
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
        return;
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
        return;
    }
    if !cookie.write_from(0, &[0u8; 8]) {
        // Unreachable: the same eight bytes were just read.
        return;
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
            return;
        }
        Err(MmioError::LegacyVersion(version)) => {
            diag_fmt(format_args!(
                "virtio FAIL step=transport version={version} need -global virtio-mmio.force-legacy=false"
            ));
            return;
        }
    };
    let id = t.device_id();
    if id != VIRTIO_ID_BLOCK {
        diag_fmt(format_args!("virtio FAIL step=id id={id}"));
        return;
    }

    // Step 4: VERSION_1 and nothing else.
    let features = match negotiate(&mut t, VBLK_WANTED_FEATURES) {
        Ok(f) => f,
        Err(NegotiateError::NoVersion1 { offered }) => {
            diag_fmt(format_args!(
                "virtio FAIL step=features offered={offered:#x}"
            ));
            return;
        }
        Err(NegotiateError::FeaturesRejected { status }) => {
            diag_fmt(format_args!(
                "virtio FAIL step=features rejected status={status:#x}"
            ));
            return;
        }
        Err(NegotiateError::ResetIncomplete { status }) => {
            diag_fmt(format_args!(
                "virtio FAIL step=features reset status={status:#x}"
            ));
            return;
        }
    };

    // Step 5: capacity. An unstable read is not reported as a capacity.
    let mut cap = [0u8; 8];
    if !config_read(&t, VBLK_CONFIG_CAPACITY_OFF, &mut cap) {
        fail(&mut t);
        diag_fmt(format_args!("virtio FAIL step=config"));
        return;
    }
    let capacity = capacity_from_config(cap);

    // Step 6: lay out and enable the request queue, then DRIVER_OK.
    let device_max = t.queue_max(REQUEST_QUEUE);
    let Some(qsize) = clamp_queue_size(device_max) else {
        fail(&mut t);
        diag_fmt(format_args!("virtio FAIL step=queue max={device_max}"));
        return;
    };
    let qlayout = queue_layout(qsize);
    let (Some(qlayout), Some(vlayout)) = (qlayout, qlayout.and_then(|q| vblk_layout(&region, q)))
    else {
        fail(&mut t);
        diag_fmt(format_args!(
            "virtio FAIL step=queue size={qsize} len={}",
            run.len
        ));
        return;
    };
    // The queue lives on its own page of the run. Its three device-visible
    // addresses come out of that `DmaBuf`, which is why enabling it is safe code.
    let Some(queue) = VirtQueue::new(vlayout.queue, qlayout) else {
        fail(&mut t);
        diag_fmt(format_args!("virtio FAIL step=queue size={qsize}"));
        return;
    };
    t.queue_enable(REQUEST_QUEUE, &queue);
    if let Err(status) = driver_ok(&mut t) {
        fail(&mut t);
        diag_fmt(format_args!("virtio FAIL step=status status={status:#x}"));
        return;
    }

    // Step 7: the line, and that the allow-list holds exactly that one.
    // The line is held, not kept: nothing re-arms it in 6.2 (an interrupt is
    // unexpected and stays masked), and a boot driver never releases one.
    if let Err(rc) = IrqLine::new(intid, VBLK_IRQ_NOTIFY_ID) {
        diag_fmt(format_args!("virtio FAIL step=irq intid={intid} rc={rc}"));
        return;
    }
    let neighbour = intid.saturating_add(1);
    match IrqLine::new(neighbour, VBLK_IRQ_NOTIFY_ID) {
        Err(EPERM) => {}
        Err(rc) => {
            diag_fmt(format_args!(
                "virtio FAIL step=irq-deny intid={neighbour} rc={rc}"
            ));
            return;
        }
        Ok(_) => {
            diag_fmt(format_args!(
                "virtio FAIL step=irq-deny intid={neighbour} rc={OK}"
            ));
            return;
        }
    }

    // Step 8.
    diag_fmt(format_args!(
        "virtio ok features={features:#x} capacity={capacity} qsize={qsize} slot={slot}"
    ));
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
