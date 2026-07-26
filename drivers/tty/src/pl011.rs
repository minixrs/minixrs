// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! PL011 UART transmit, from EL0 (slice 5.3, decision D1).
//!
//! This is the whole reason minix.rs has a TTY *driver* rather than a TTY
//! *server*: the register accesses below are the first hardware the kernel does
//! not own. The kernel pre-maps the controller's MMIO page into this process's
//! address space at [`TTY_UART_VA`] with Device attributes while building it (see
//! `arch::aarch64::userland::load_boot_server`), because D1 deferred a
//! VM-mediated `VMCTL_MAP_PHYS` to Phase 6 — a driver has no way to ask for a
//! physical mapping yet.
//!
//! **TX only, polled, no interrupts.** RX needs `SYS_IRQCTL` and an interrupt
//! path, which is Phase 6. Polling `FR.TXFF` before each `DR` store is the
//! documented send sequence (ARM PL011 TRM r1p5 §3.3).
//!
//! ## Register offsets are deliberately re-declared
//!
//! `kernel/src/arch/aarch64/uart.rs` declares the same three constants. They are
//! not shared, and cannot be: the kernel crate is bare-metal-only and pinned to
//! `aarch64-unknown-none` by `forced-target`, so it can never be a dependency of a
//! user-space crate. Moving them into `kernel-shared` was considered and rejected —
//! a PL011 register layout is not a shared *ABI*, it is a hardware fact that two
//! independent drivers happen to both know. The duplication is noted in both files.
//!
//! ## Why this is the crate's only `unsafe`
//!
//! Everything else in the driver is message parsing (`cdev.rs`) or IPC through
//! `minixrs-ipc`. The two volatile accesses here are what `#![forbid(unsafe_code)]`
//! on `server-rt` exists to keep out of shared code.

// Only `main.rs`'s `not(test)`-gated paths reach these, so under `cargo test`
// (which drops `_start` and `main`) they would read as dead code.
#![cfg_attr(test, allow(dead_code))]

use core::ptr::{read_volatile, write_volatile};

use minixrs_kernel_shared::uspace::TTY_UART_VA;

/// Data register — a byte written here is transmitted.
const DR_OFFSET: u64 = 0x00;
/// Flag register.
const FR_OFFSET: u64 = 0x18;
/// `FR` bit 5: transmit FIFO full. Poll until clear before storing to `DR`.
const FR_TXFF: u32 = 1 << 5;

/// Transmit one byte, blocking while the TX FIFO is full.
///
/// Under QEMU's TCG the FIFO never actually fills, so the poll is not observably
/// load-bearing here — it is correctness for real hardware, where dropping it
/// silently loses characters under load.
pub fn putc(c: u8) {
    let fr = (TTY_UART_VA + FR_OFFSET) as *mut u32;
    let dr = (TTY_UART_VA + DR_OFFSET) as *mut u32;
    // SAFETY: `TTY_UART_VA` is mapped into this process's address space by the
    // kernel at boot — one page, `Prot::DEVICE_RW` (EL0 read+write, never
    // executable, Device memory), covering the PL011's whole register block. TTY
    // is the only process with that mapping, and this process is single-threaded
    // with no signal handlers, so there is no concurrent access to order against.
    // Volatile is required in both directions: the compiler must not cache `FR`
    // across the poll (it would spin forever) or elide/duplicate the `DR` store
    // (each store transmits one character).
    unsafe {
        while read_volatile(fr) & FR_TXFF != 0 {}
        write_volatile(dr, c as u32);
    }
}

/// Transmit `bytes`, translating LF to CRLF.
///
/// The serial console is a terminal, not a file: a bare LF moves down a line
/// without returning the carriage, so output staircases. Translating here rather
/// than in the client keeps every writer honest — and matches what the kernel's
/// own `Pl011: fmt::Write` impl already does, so kernel traces and EL0 text
/// interleave without one of them looking broken.
///
/// (`grep -aF` markers cannot express `\r`, so the boot log cannot prove this
/// arm; it is verified by the console being readable.)
pub fn write(bytes: &[u8]) {
    for &b in bytes {
        if b == b'\n' {
            putc(b'\r');
        }
        putc(b);
    }
}
