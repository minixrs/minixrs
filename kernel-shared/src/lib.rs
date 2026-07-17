// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Types and constants shared between the minix.rs microkernel, system servers,
//! drivers, and the user-space `minix-ipc` library.
//!
//! Everything in this crate is `#![no_std]` and must remain so. Behaviour
//! belongs in the kernel or in `minix-ipc` / `server-rt`, not here. Values
//! are pinned to MINIX 3's ABI where possible (see per-module docs for the
//! specific reference header).

#![no_std]

pub mod callnr;
pub mod com;
pub mod endpoint;
pub mod error;
pub mod ipc_const;
pub mod message;
pub mod signal;
pub mod sys_limits;

pub use endpoint::{Endpoint, GenNr, PrivId, ProcNr, SysId};
pub use message::Message;
