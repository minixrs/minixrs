// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Driver runtime library (IRQ line, virtio transport and negotiation, split virtqueue;
//! BDEV/CDEV protocol to come).
//!
//! Every virtio driver's loop follows the same order (I12): wait for
//! [`IrqLine::fired`]; read [`Transport::interrupt_status`] and
//! [`Transport::interrupt_ack`] exactly those bits; walk the used ring with
//! [`VirtQueue::pop_used`] until it returns `None`; then [`IrqLine::rearm`].
//!
//! The ack precedes the walk because `InterruptStatus` is level-triggered: a
//! completion that lands after the final empty `pop_used` but before an ack placed
//! after the walk would be cleared by that ack, and the re-arm would find the line
//! low, losing the interrupt. Acking first means any later completion re-raises the
//! line, and the walk (which runs after the ack) still sees everything already
//! published. The queue takes no part in interrupt handling and never waits on the
//! ring.

// `no_std` for the real (freestanding) build, but a normal host crate under
// `cargo test` so the pure logic gets host-runnable unit tests (as `server-rt`).
#![cfg_attr(not(test), no_std)]

pub mod irq;
pub mod mmio;
pub mod queue;
pub mod transport;

pub use irq::IrqLine;
pub use mmio::{MmioError, MmioTransport};
pub use queue::{
    DESC_F_NEXT, DESC_F_WRITE, DmaRun, QUEUE_SIZE, QueueLayout, VirtQueue, clamp_queue_size,
    queue_layout,
};
pub use transport::{QueueAddrs, Transport};
