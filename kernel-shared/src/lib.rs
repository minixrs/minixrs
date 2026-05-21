//! Types and constants shared between the MINIX 4 microkernel, system servers,
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

pub use endpoint::{Endpoint, GenNr, ProcNr};
pub use message::Message;
