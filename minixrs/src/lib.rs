// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs — "MINIX 3, in Rust, for the 64-bit era".
//!
//! This umbrella crate re-exports the reusable, host-buildable libraries of the
//! [minix.rs](https://github.com/minixrs/minixrs) microkernel OS. It exists so the
//! `minixrs` name resolves to a single entry point on crates.io; the kernel,
//! servers, drivers, and userland programs are freestanding binaries that are not
//! published.
//!
//! # Modules
//!
//! - [`kernel_shared`] — the MINIX ABI: message types, endpoints, call numbers
//!   shared between the kernel and user space.
//! - [`ipc`] — user-space IPC wrappers over the kernel's SVC trap (aarch64).
//! - [`server_rt`] — the server runtime / SEF framework.
//! - [`driver_rt`] — the driver runtime.

#![no_std]

pub use minixrs_driver_rt as driver_rt;
pub use minixrs_ipc as ipc;
pub use minixrs_kernel_shared as kernel_shared;
pub use minixrs_server_rt as server_rt;
