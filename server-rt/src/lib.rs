// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Server runtime — a minimal SEF (System Event Framework) for minix.rs system
//! servers (PM, VFS, VM, RS, DS, SCHED).
//!
//! SEF is the shape every server's `main` follows, lifted from MINIX 3's
//! `lib/libsys/sef.c` but pared to the essentials:
//!
//! ```ignore
//! let sef = sef_startup(SefConfig { init_fresh: Some(my_init), signal_handler: None })?;
//! let mut msg = Message { m_source: 0, m_type: 0, payload: [0u8; 96] };
//! loop {
//!     if sef.receive(&mut msg) != OK { continue; }
//!     match msg.m_type { /* application requests */ }
//! }
//! ```
//!
//! [`sef_startup`] learns the server's own endpoint + name via
//! `SYS_GETINFO(GET_WHOAMI)`, announces the server on the kernel debug channel
//! ([`diag_print`] — servers run at EL0 and have no other way to speak), and
//! runs the registered fresh-init callback.
//! [`Sef::receive`] wraps `ipc_receive(ANY, …)` and transparently handles SEF
//! control messages (RS heartbeat pings, signals, re-init), returning only
//! application traffic to the server.
//!
//! Unlike MINIX's global `sef_setcb_*` registration, callbacks are passed in a
//! [`SefConfig`] and carried in the returned [`Sef`] handle — no static mutable
//! state, so the whole crate is `#![forbid(unsafe_code)]` (all `unsafe` lives in
//! `minixrs-ipc`). Live-update / state-transfer SEF features are deferred.

// `no_std` for the real (freestanding) build, but a normal host crate under
// `cargo test` so the pure [`classify`] logic gets host-runnable unit tests.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

mod classify;
mod diag;
mod ds;
mod grant;
mod init;
mod kcall;
mod payload;
mod sef;
mod signal;

pub use classify::{SefEvent, classify};
pub use diag::{DIAG_LINE_MAX, diag_fmt, diag_print};
pub use ds::{sef_publish_to_ds, sef_retrieve_from_ds};
pub use grant::GrantPool;
pub use init::SefInitCb;
pub use kcall::{sys_copy, sys_getinfo, sys_safecopy};
pub use payload::{buf_addr, rd_i32, rd_name, rd_u64, wr_i32, wr_u64};
pub use sef::{Sef, SefConfig, sef_startup};
pub use signal::SefSignalCb;
