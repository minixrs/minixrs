// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! SEF fresh-init callback.

use minixrs_kernel_shared::Endpoint;
use minixrs_kernel_shared::callnr::SYS_GETINFO_NAME_LEN;

/// Fresh-init callback, run once by [`crate::sef_startup`] after the server has
/// learned its own `endpoint` and `name`. A server uses it to set up state and
/// (from slice 4.2 on) publish its endpoint to DS. Returns `OK` (0) on success
/// or a negative error code, which aborts startup. The minimal Phase-4 SEF has
/// no separate restart-init variant; that is deferred with live update.
pub type SefInitCb = fn(endpoint: Endpoint, name: &[u8; SYS_GETINFO_NAME_LEN]) -> i32;
