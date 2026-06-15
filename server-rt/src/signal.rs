// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! SEF signal handling.

/// Signal-handler callback, invoked by [`crate::Sef::receive`] when a
/// `SEF_SIGNAL` control message arrives. The signal number `signo` is decoded
/// from the message payload by the framework. A server that registers no
/// handler simply ignores signals (the minimal Phase-4 default — full POSIX
/// signal semantics live in PM, slice 4.5).
pub type SefSignalCb = fn(signo: i32);
