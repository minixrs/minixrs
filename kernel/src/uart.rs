// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Architecture-neutral re-export of the active early-console UART.
//!
//! All arch backends expose their UART as `crate::arch::Uart`. This module
//! re-exports it under a stable name so the rest of the kernel never has
//! to `cfg`-gate console code.

pub use crate::arch::Uart;
