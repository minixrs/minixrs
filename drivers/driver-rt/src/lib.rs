// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Driver runtime library: IRQ line, virtio transport and negotiation, typed DMA
//! handles, split virtqueue. (The BDEV request parse and validation a block driver
//! needs live in `server-rt::bdev`, not here.)
//!
//! Every virtio driver's loop follows the same order (I12): wait for
//! [`IrqLine::fired`]; read [`Transport::interrupt_status`] and
//! [`Transport::interrupt_ack`] exactly those bits; walk the used ring with
//! [`VirtQueue::pop_used`] until it returns `Ok(None)`; then [`IrqLine::rearm`].
//! An `Err(`[`QueueFault`]`)` from the walk means the device broke the ring
//! contract: nothing was skipped, and the driver stops using the device.
//!
//! The ack precedes the walk because `InterruptStatus` is level-triggered: a
//! completion that lands after the final empty `pop_used` but before an ack placed
//! after the walk would be cleared by that ack, and the re-arm would find the line
//! low, losing the interrupt. Acking first means any later completion re-raises the
//! line, and the walk (which runs after the ack) still sees everything already
//! published. The queue takes no part in interrupt handling and never waits on the
//! ring.
//!
//! The crate's `unsafe` lives in two modules. `mmio.rs` holds the register
//! accesses. `dma.rs` holds every access to memory shared with the device: one
//! `unsafe` constructor ([`DmaRegion::new`]) adopts the kernel-reported run, a
//! queue's memory is split off it first as a [`QueueMem`] that no buffer handle
//! can reach, and the bounds-checked [`DmaBuf`]s handed out of the remainder
//! carry the volatile accessors. `queue.rs` contains no `unsafe` of its own — its
//! ring accesses go through the handle it took out of its `QueueMem` — and its
//! tests are what drive `dma.rs` under Miri.

// `no_std` for the real (freestanding) build, but a normal host crate under
// `cargo test` so the pure logic gets host-runnable unit tests (as `server-rt`).
#![cfg_attr(not(test), no_std)]

pub mod dma;
pub mod irq;
pub mod mmio;
pub mod queue;
pub mod transport;

pub use dma::{DmaBuf, DmaRegion, DmaRun, QueueMem};
pub use irq::IrqLine;
pub use mmio::{MmioError, MmioTransport};
pub use queue::{
    DESC_F_NEXT, DESC_F_WRITE, QUEUE_SIZE, QueueFault, QueueLayout, VirtQueue, clamp_queue_size,
    queue_layout,
};
pub use transport::{QueueAddrs, Transport};
