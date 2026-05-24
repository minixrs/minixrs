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
pub fn mini_senda(
    _proc_table: &mut [Proc; N_PROC_SLOTS],
    _priv_table: &mut [Priv; NR_SYS_PROCS],
    _caller_nr: ProcNr,
    _user_table_va: u64,
    _table_size: usize,
) -> i32 {
    ENOSYS
}
