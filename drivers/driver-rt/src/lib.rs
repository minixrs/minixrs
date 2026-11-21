// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Driver runtime library (IRQ line today; BDEV/CDEV protocol + VirtIO transport to come).

// `no_std` for the real (freestanding) build, but a normal host crate under
// `cargo test` so the pure logic gets host-runnable unit tests (as `server-rt`).
#![cfg_attr(not(test), no_std)]

pub mod irq;

pub use irq::IrqLine;
