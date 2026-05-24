//! `SENDA` — asynchronous send (table-based).
//!
//! Stub for slice 2.5. The full implementation (walking an `asynmsg_t`
//! table in user memory, honoring `AMF_VALID` / `AMF_DONE` / `AMF_NOTIFY`,
//! recording deferred deliveries in `priv.asyn_pending`, and integrating
//! with `mini_receive`'s pickup path) is substantial — roughly the size
//! of `mini_send` and `mini_receive` combined — and has no observable
//! consumer in Phase 2 (no server yet RECEIVEs from ASYNCM). Pushing it
//! to a later slice keeps slice 2.5 focused on the two-stub ping-pong
//! milestone.

use minix4_kernel_shared::ProcNr;
use minix4_kernel_shared::com::NR_SYS_PROCS;
use minix4_kernel_shared::error::ENOSYS;

use crate::proc::table::N_PROC_SLOTS;
use crate::proc::{Priv, Proc};

/// `SENDA` primitive. TODO(slice 2.6+): wire up real async delivery.
///
/// When the real implementation lands, bounds-check `user_table_va`
/// (and reject `table_size == 0` etc.) *before* returning any other
/// error so the caller-visible error precedence — EFAULT > everything
/// else — stays stable across the stub→real transition.
///
/// Also note: today this primitive is dispatcher-denied via
/// `trap_gate` (SENDA's bit 16 doesn't fit in the current `u16`
/// `trap_mask`, so the gate always returns ETRAPDENIED before reaching
/// this function). The `ENOSYS` body is only reachable once
/// `trap_mask` widens — see the TODO on `ipc::trap_gate`.
pub fn mini_senda(
    _proc_table: &mut [Proc; N_PROC_SLOTS],
    _priv_table: &mut [Priv; NR_SYS_PROCS],
    _caller_nr: ProcNr,
    _user_table_va: u64,
    _table_size: usize,
) -> i32 {
    ENOSYS
}
