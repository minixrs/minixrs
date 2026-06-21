// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs VM (virtual memory) server.
//!
//! The first *real* user-space process (introduced in slice 3.4a as a build
//! milestone). Built as a freestanding aarch64 ELF (see `servers/vm/user.ld`),
//! embedded into the kernel image by `kernel/build.rs`, and loaded into its
//! own per-process AddrSpace at boot by `arch::aarch64::userland::load_boot_server`.
//!
//! Slice 3.4b made it functional: a `RECEIVE(ANY)` loop that resolves page
//! faults. When an EL0 process faults on an unmapped page, the kernel records
//! the fault, blocks the faulter on `RTS_PAGEFAULT`, and sends VM a
//! `VM_PAGEFAULT` message (`m_source` = faulter, payload = fault addr/flags).
//! VM maps a fresh page into the faulter via `SYS_VMCTL(VMCTL_PT_MAP)` and
//! unblocks it via `SYS_VMCTL(VMCTL_CLEAR_PAGEFAULT)`. No `alloc` crate — the
//! kernel owns the frame allocator; VM only drives policy.
//!
//! Slice 3.5 adds memory to VM: a per-process [`region`] table and a `VM_BRK`
//! request that grows a process's heap region. Page faults are now satisfied
//! only when the address lies inside a known region; out-of-region faults take
//! a SIGSEGV path (logged, faulter left blocked — real signals are Phase 4).
//!
//! Slice 3.6 adds `VM_MMAP` / `VM_MUNMAP`: anonymous, VM-chosen regions
//! bump-allocated from `MMAP_BASE`. `VM_MMAP` records an `Mmap` region (pages
//! fault in lazily, like the heap); `VM_MUNMAP` drops the region and unmaps each
//! backing page via `SYS_VMCTL(VMCTL_PT_UNMAP)`.
//!
//! Slice 4.1 moves the loop scaffolding onto the SEF framework
//! ([`minixrs_server_rt`]): `sef_startup` performs the `GET_WHOAMI` handshake
//! and `sef.receive` filters SEF control messages, replacing the hand-rolled
//! `RECEIVE(ANY)` loop. The request handlers below are unchanged — VM is the
//! first real consumer of `server-rt`, proving it before new servers depend
//! on it.

// Freestanding for the real (bare-metal) build, but a normal host binary under
// `cargo test` so `region`'s logic gets host-runnable unit tests. The test
// harness needs `std` and its own entry point, so `no_std`/`no_main` and the
// `_start` shim below are all gated to `not(test)`.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

mod region;

use minixrs_ipc::{ipc_send, ipc_sendrec};
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    SYS_GETINFO_NAME_LEN, SYS_VMCTL, VM_BRK, VM_MMAP, VM_MUNMAP, VM_PAGEFAULT,
    VMCTL_CLEAR_PAGEFAULT, VMCTL_PROT_WRITE, VMCTL_PT_MAP, VMCTL_PT_UNMAP,
};
use minixrs_kernel_shared::com::{SYSTEM, boot_endpoint};
use minixrs_kernel_shared::endpoint::{Endpoint, endpoint_proc};
use minixrs_kernel_shared::error::OK;
use minixrs_server_rt::{SefConfig, sef_publish_to_ds, sef_startup};

/// aarch64 4 KiB page size — VM only needs to page-align fault addresses.
const PAGE_SIZE: u64 = 4096;

/// ELF entry point. The kernel primes `SP_EL0` before `eret`, so `_start`
/// can dive straight into Rust without setting up a stack itself.
// Gate the whole shim to `not(test)`: under `cargo test` the crate links as a
// normal host executable, and an exported `_start` would collide with the C
// runtime's `_start` (a hard "duplicate symbol" error on the GNU/Linux linker).
#[cfg(not(test))]
#[unsafe(no_mangle)]
// `.text._start` is an ELF section name; gate it to the bare-metal target so
// `cargo check --workspace` on a Mach-O host (which rejects the specifier)
// still type-checks this crate. `ENTRY(_start)` in user.ld roots the symbol
// either way, so ordering it first in `.text` is a nicety, not a requirement.
#[cfg_attr(target_os = "none", unsafe(link_section = ".text._start"))]
pub extern "C" fn _start() -> ! {
    main()
}

// Only `_start` calls `main`; under `cargo test` `_start` is gone, so `main`
// (and the message helpers it alone reaches) would read as dead code.
#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
    // Drive the loop through the SEF framework (slice 4.1): `sef_startup` learns
    // VM's endpoint/name via SYS_GETINFO(GET_WHOAMI) and `sef.receive` strips
    // SEF control messages, handing back only application requests. `vm_init`
    // (slice 4.2) publishes VM's endpoint to DS so other servers can find it; no
    // signal handling. If the startup handshake fails there is no recovery and
    // nothing to print from EL0 — park forever.
    let sef = sef_startup(SefConfig {
        init_fresh: Some(vm_init),
        signal_handler: None,
    })
    .unwrap_or_else(|_| {
        loop {
            core::hint::spin_loop()
        }
    });

    let system = boot_endpoint(SYSTEM);
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
            VM_PAGEFAULT => handle_pagefault(system, msg.m_source, rd_u64(&msg, 0)),
            VM_BRK => handle_brk(&mut msg),
            VM_MMAP => handle_mmap(&mut msg),
            VM_MUNMAP => handle_munmap(system, &mut msg),
            // Unknown request: drop it.
            _ => {}
        }
    }
}

/// SEF fresh-init callback: publish VM's endpoint to DS under its name, so other
/// servers can look VM up by name (slice 4.2). DS registers the caller's
/// kernel-stamped endpoint, so `_endpoint` from `GET_WHOAMI` is not sent.
fn vm_init(_endpoint: Endpoint, name: &[u8; SYS_GETINFO_NAME_LEN]) -> i32 {
    sef_publish_to_ds(name)
}

/// Resolve a page fault for `faulting_e` at `far`.
///
/// Slice 3.5 gates this on the faulter's [`region`] table: only faults inside a
/// known region (today, the heap) are satisfied. A fault outside every region
/// is a SIGSEGV — VM returns without mapping or clearing, leaving the faulter
/// blocked on `RTS_PAGEFAULT` (the only "kill" available until PM + signals in
/// Phase 4). VM cannot print from EL0, so this path is silent; the symptom is a
/// process that stops making progress.
fn handle_pagefault(system: Endpoint, faulting_e: Endpoint, far: u64) {
    let nr = endpoint_proc(faulting_e).get();
    if !region::contains(nr, far) {
        // SIGSEGV: unmapped access outside any region. Leave it blocked.
        return;
    }

    let page = far & !(PAGE_SIZE - 1);

    // SYS_VMCTL(VMCTL_PT_MAP, target=faulting_e, vaddr=page, prot=WRITE).
    // The kernel allocates + maps the frame; we ignore the returned PA — the
    // region table records the VA range, and the kernel owns the frames.
    let mut m = Message {
        m_source: 0,
        m_type: SYS_VMCTL,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, 0, VMCTL_PT_MAP);
    wr_i32(&mut m, 4, faulting_e);
    wr_u64(&mut m, 8, page);
    wr_i32(&mut m, 16, VMCTL_PROT_WRITE);
    let _ = ipc_sendrec(system, &mut m);

    // SYS_VMCTL(VMCTL_CLEAR_PAGEFAULT, target=faulting_e) — unblock the faulter.
    let mut m = Message {
        m_source: 0,
        m_type: SYS_VMCTL,
        payload: [0u8; 96],
    };
    wr_i32(&mut m, 0, VMCTL_CLEAR_PAGEFAULT);
    wr_i32(&mut m, 4, faulting_e);
    let _ = ipc_sendrec(system, &mut m);
}

/// Handle a `VM_BRK` request: set the caller's program break, growing or
/// creating its heap region. No frames are mapped here — pages fault in lazily
/// and are then satisfied by [`handle_pagefault`] via the region check. Reply
/// to the caller (it issued a SENDREC) with `m_type = OK` and the resulting
/// break in payload `0..8`, or the negative error in `m_type`.
fn handle_brk(msg: &mut Message) {
    let caller_e = msg.m_source;
    let nr = endpoint_proc(caller_e).get();
    let new_break = rd_u64(msg, 0);

    let reply_type = match region::set_brk(nr, new_break) {
        Ok(brk) => {
            wr_u64(msg, 0, brk);
            OK
        }
        Err(e) => e,
    };

    msg.m_type = reply_type;
    msg.m_source = 0; // kernel overwrites on delivery
    let _ = ipc_send(caller_e, msg);
}

/// Handle a `VM_MMAP` request: allocate an anonymous region of `len` bytes
/// (payload `0..8`), letting VM choose the base address. No frames are mapped
/// here — pages fault in lazily and are satisfied by [`handle_pagefault`] via
/// the region check. Reply (the caller issued a SENDREC) with `m_type = OK` and
/// the chosen base in payload `0..8`, or the negative error in `m_type`.
fn handle_mmap(msg: &mut Message) {
    let caller_e = msg.m_source;
    let nr = endpoint_proc(caller_e).get();
    let len = rd_u64(msg, 0);

    let reply_type = match region::mmap(nr, len) {
        Ok(addr) => {
            wr_u64(msg, 0, addr);
            OK
        }
        Err(e) => e,
    };

    msg.m_type = reply_type;
    msg.m_source = 0; // kernel overwrites on delivery
    let _ = ipc_send(caller_e, msg);
}

/// Handle a `VM_MUNMAP` request: drop the caller's mmap region based at `addr`
/// (payload `0..8`) covering `len` bytes (payload `8..16`), then unmap each
/// backing page. [`region::munmap`] returns the page-aligned `[start, end)`
/// snapshot to sweep and marks the slot free *before* we touch the page tables,
/// so the loop runs off owned values, not a re-read of the region. A page that
/// never faulted in was never mapped, so the kernel returns `EINVAL` for it —
/// harmless, ignored (same as unmapping a hole). Reply with `m_type = OK`, or
/// `EINVAL` if no region matched (nothing is unmapped in that case).
fn handle_munmap(system: Endpoint, msg: &mut Message) {
    let caller_e = msg.m_source;
    let nr = endpoint_proc(caller_e).get();
    let addr = rd_u64(msg, 0);
    let len = rd_u64(msg, 8);

    let reply_type = match region::munmap(nr, addr, len) {
        Ok((start, end)) => {
            let mut page = start;
            while page < end {
                let mut m = Message {
                    m_source: 0,
                    m_type: SYS_VMCTL,
                    payload: [0u8; 96],
                };
                wr_i32(&mut m, 0, VMCTL_PT_UNMAP);
                wr_i32(&mut m, 4, caller_e);
                wr_u64(&mut m, 8, page);
                // Ignore the result: a never-faulted page is EINVAL, harmless.
                let _ = ipc_sendrec(system, &mut m);
                page += PAGE_SIZE;
            }
            OK
        }
        Err(e) => e,
    };

    msg.m_type = reply_type;
    msg.m_source = 0; // kernel overwrites on delivery
    let _ = ipc_send(caller_e, msg);
}

// Native-endian payload accessors, mirroring the kernel's `do_vmctl` reads.
fn rd_u64(m: &Message, off: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&m.payload[off..off + 8]);
    u64::from_ne_bytes(b)
}

fn wr_i32(m: &mut Message, off: usize, v: i32) {
    m.payload[off..off + 4].copy_from_slice(&v.to_ne_bytes());
}

fn wr_u64(m: &mut Message, off: usize, v: u64) {
    m.payload[off..off + 8].copy_from_slice(&v.to_ne_bytes());
}

// The freestanding panic handler; under `cargo test` std supplies its own.
#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
