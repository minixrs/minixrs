// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs `virtio-console` driver — the second CDEV backend, `/dev/hvc0`
//! (slice 6.6).
//!
//! This commit holds the pure, host-tested half only: [`bringup`] (where the two
//! queues and the two buffers sit in the DMA run) and [`port`] (the receive
//! buffer's and the transmit chain's state machines). The driver proper — the
//! device bring-up, the CDEV request loop and the interrupt handler — lands in
//! the next commit. Until then this ELF is not packed into the boot archive, so
//! `main` never runs.

// Freestanding for the real (bare-metal) build, but a normal host binary under
// `cargo test` so `bringup`'s and `port`'s logic get host-runnable unit tests.
// The test harness needs `std` and its own entry point, so `no_std`/`no_main`
// and the `_start` shim below are all gated to `not(test)`.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

minixrs_abi_note::brand!();

mod bringup;
mod port;

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

/// A placeholder until the next commit: the driver is not packed, so nothing
/// starts it.
// Only `_start` calls `main`; under `cargo test` `_start` is gone, so `main`
// would read as dead code.
#[cfg_attr(test, allow(dead_code))]
fn main() -> ! {
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
