// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_*` ENOSYS placeholders.
//!
//! Phase 2 ships the dispatch surface and one working handler (`SYS_GETINFO`).
//! Every other Phase-2 `SYS_*` lands here as an `ENOSYS` stub so that
//! `kernel_call_dispatch` can match on every reachable `m_type` and the
//! `k_call_mask` permission check has somewhere to land. Each stub keeps
//! the canonical MINIX 3 `do_*` name so the eventual real implementation
//! can replace it without touching the dispatch table.

use minixrs_kernel_shared::error::ENOSYS;
use minixrs_kernel_shared::message::Message;

use crate::proc::{Priv, Proc};

macro_rules! enosys_stub {
    ($name:ident) => {
        pub(super) fn $name(_caller: &mut Proc, _caller_priv: &Priv, _msg: &mut Message) -> i32 {
            ENOSYS
        }
    };
}

enosys_stub!(do_privctl);
enosys_stub!(do_fork);
enosys_stub!(do_exec);
enosys_stub!(do_exit);
enosys_stub!(do_copy);
enosys_stub!(do_safecopy);
enosys_stub!(do_irqctl);
// `do_vmctl` is a real handler as of slice 3.3 — see `system::do_vmctl`.
// `do_schedule` / `do_schedctl` are real handlers as of slice 4.3 — see
// `system::do_schedule`.
// `do_setalarm` is a real handler as of slice 4.4 — see `system::do_setalarm`.
enosys_stub!(do_times);
enosys_stub!(do_diagctl);
enosys_stub!(do_setgrant);
// `do_kill` / `do_getksig` / `do_endksig` are real handlers as of slice 4.5 —
// see `system::do_sig`.
