// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Process and privilege tables.
//!
//! Slice 2.2 ships the type and storage shells: `Proc`, `Priv`, the static
//! arrays, and a boot-image table that populates them. No execution paths
//! consume the tables yet — slice 2.3 will start reading from them when SVC
//! entry lands, and slice 2.5 when the IPC primitives go live.

pub(crate) mod bitmap;
pub mod dump;
pub mod flags;
pub mod page_fault;
pub mod priv_struct;
pub mod proc_struct;
pub mod sched;
pub mod table;

// Re-exports for later slices: `Proc`, `Priv`, `IoRange`, and `MemRange` are
// not yet referenced from outside `proc/` itself, but the API surface is
// stable enough that exposing it now keeps slice 2.5 / 2.6 from touching this
// file again.
#[allow(unused_imports)]
pub use dump::dump_tables;
#[allow(unused_imports)]
pub use page_fault::{HeapWindow, PageFaultState};
#[allow(unused_imports)]
pub use priv_struct::{IoRange, MemRange, Priv};
#[allow(unused_imports)]
pub use proc_struct::Proc;
#[allow(unused_imports)]
pub use table::init;
