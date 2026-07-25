// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SENDA` — asynchronous send (table-based).
//!
//! `ENOSYS` stub, and a standing non-goal — the one IPC primitive of the six
//! that minix.rs has never implemented. The full version (walking an
//! `asynmsg_t` table in user memory, honoring `AMF_VALID` / `AMF_DONE` /
//! `AMF_NOTIFY`, recording deferred deliveries in `priv.asyn_pending`, and
//! integrating with `mini_receive`'s pickup path) is roughly the size of
//! `mini_send` and `mini_receive` combined, and nothing in the system has ever
//! needed it: no server RECEIVEs from ASYNCM. It stays a stub until a real
//! consumer appears.

use minixrs_kernel_shared::ProcNr;
use minixrs_kernel_shared::com::NR_SYS_PROCS;
use minixrs_kernel_shared::error::ENOSYS;

use crate::proc::table::N_PROC_SLOTS;
use crate::proc::{Priv, Proc};

/// `SENDA` primitive — unimplemented.
///
/// If it is ever built, validate `user_table_va` (and reject
/// `table_size == 0` etc.) *before* returning any other error, so the
/// caller-visible error precedence — EFAULT > everything else — stays stable
/// across the stub→real transition. That is the ordering `mini_send` already
/// documents and relies on.
///
/// Note this body is currently unreachable: `trap_gate` denies SENDA outright,
/// because its bit 16 does not fit the `u16` `Priv::trap_mask`. The `ENOSYS`
/// return only becomes observable if `trap_mask` widens — see the note on
/// `ipc::trap_gate`.
pub fn mini_senda(
    _proc_table: &mut [Proc; N_PROC_SLOTS],
    _priv_table: &mut [Priv; NR_SYS_PROCS],
    _caller_nr: ProcNr,
    _user_table_va: u64,
    _table_size: usize,
) -> i32 {
    ENOSYS
}
