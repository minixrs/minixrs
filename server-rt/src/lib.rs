// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Server runtime — a minimal SEF (System Event Framework) for minix.rs system
//! servers (VM today; PM/VFS/RS/DS/SCHED in later Phase 4 slices).
//!
//! SEF is the shape every server's `main` follows, lifted from MINIX 3's
//! `lib/libsys/sef.c` but pared to the essentials:
//!
//! ```ignore
//! let sef = sef_startup(SefConfig { init_fresh: Some(my_init), signal_handler: None })?;
//! let mut msg = Message { m_source: 0, m_type: 0, payload: [0u8; 96] };
//! loop {
//!     if sef.receive(&mut msg) != 0 { continue; }
//!     match msg.m_type { /* application requests */ }
//! }
//! ```
//!
//! [`sef_startup`] learns the server's own endpoint + name via
//! `SYS_GETINFO(GET_WHOAMI)` and runs the registered fresh-init callback.
//! [`Sef::receive`] wraps `ipc_receive(ANY, …)` and transparently handles SEF
//! control messages (RS heartbeat pings, signals, re-init), returning only
//! application traffic to the server.
//!
//! Unlike MINIX's global `sef_setcb_*` registration, callbacks are passed in a
//! [`SefConfig`] and carried in the returned [`Sef`] handle — no static mutable
//! state, so the whole crate is `#![forbid(unsafe_code)]` (all `unsafe` lives in
//! `minix-ipc`). Live-update / state-transfer SEF features are deferred.

// `no_std` for the real (freestanding) build, but a normal host crate under
// `cargo test` so the pure [`classify`] logic gets host-runnable unit tests.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

mod classify;
mod init;
mod sef;
mod signal;

pub use classify::{SefEvent, classify};
pub use init::SefInitCb;
pub use sef::{Sef, SefConfig, sef_startup};
pub use signal::SefSignalCb;
