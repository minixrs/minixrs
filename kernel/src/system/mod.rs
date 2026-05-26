//! Kernel-call dispatch — the `SYSTEM` task's role.
//!
//! In MINIX 3, `SYSTEM` is a kernel pseudo-task: it has a proc-table slot but
//! no scheduler context. When a server `SENDREC`s the `SYSTEM` endpoint, the
//! IPC fast path in `proc.c::do_ipc` short-circuits into `kernel_call()`
//! (`kernel/system.c`), which runs synchronously in the *caller's* EL1 stack
//! and writes the reply back into the caller's user buffer. The caller never
//! actually blocks on a receiver; SYSTEM's `IMAGE` entry exists only so that
//! the endpoint encodes to a valid `ProcNr`.
//!
//! MINIX 4 inherits exactly that shape. [`ipc::dispatch`] detects
//! `src_dst_e == boot_endpoint(SYSTEM)` in the SENDREC arm and diverts here
//! instead of calling [`ipc::send::mini_send`]; the rest of the IPC engine is
//! untouched.
//!
//! [`ipc::dispatch`]: crate::ipc
//! [`ipc::send::mini_send`]: crate::ipc

mod do_getinfo;
mod stubs;

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use minix4_kernel_shared::ProcNr;
use minix4_kernel_shared::callnr::{
    KERNEL_CALL, NR_KERN_CALLS_PHASE2, NR_SYS_CALLS, SYS_COPY, SYS_DIAGCTL,
    SYS_EXEC, SYS_EXIT, SYS_FORK, SYS_GETINFO, SYS_IRQCTL, SYS_PRIVCTL,
    SYS_SAFECOPY, SYS_SCHEDULE, SYS_SETALARM, SYS_SETGRANT, SYS_TIMES, SYS_VMCTL,
};
use minix4_kernel_shared::com::{NR_SYS_PROCS, SYSTEM, boot_endpoint};
use minix4_kernel_shared::endpoint::Endpoint;
use minix4_kernel_shared::error::{EBADREQUEST, ECALLDENIED, OK};
use minix4_kernel_shared::message::Message;

use crate::ipc::{copy_msg_from_user, copy_msg_to_user};
use crate::proc::bitmap::{get_call_bit, get_sys_bit};
use crate::proc::table::{N_PROC_SLOTS, proc_index};
use crate::proc::{Priv, Proc};
use crate::uart::Uart;

/// Boot-time endpoint of the `SYSTEM` kernel task. Generation never bumps for
/// kernel tasks in Phase 2 — they are statically slotted at boot and never
/// exit — so this single constant identifies SYSTEM for the entire kernel
/// lifetime.
#[inline]
pub fn system_endpoint() -> Endpoint {
    boot_endpoint(SYSTEM)
}

/// Cadence of the boot-time kernel-call trace. Mirrors `ipc::TRACE_EVERY`.
const KCALL_TRACE_EVERY: u64 = 100;
/// Running total of kernel calls dispatched, sampled by the trace.
static CALL_COUNT: AtomicU64 = AtomicU64::new(0);

/// `SENDREC`-to-SYSTEM fast path.
///
/// Reads the request out of `user_msg_va`, runs the dispatcher, and writes
/// the reply (m_source = SYSTEM, m_type = result, possibly mutated payload)
/// back to the same buffer. The caller stays runnable and `el1_svc_tail`
/// resumes it on return.
///
/// Returns the IPC-layer result (`OK`, `EFAULT`, `ECALLDENIED`); the
/// kernel-call result code is carried in the reply's `m_type` field.
pub fn kernel_call_sendrec(
    proc_table: &mut [Proc; N_PROC_SLOTS],
    priv_table: &mut [Priv; NR_SYS_PROCS],
    caller_nr: ProcNr,
    user_msg_va: u64,
) -> i32 {
    // Kernel invariants: any proc that reached the SVC handler has a slot
    // and a priv_id installed (proc::init or userland_bootstrap), and
    // SYSTEM is populated by proc::init. A None here is a structural bug
    // in those bootstrap paths, not a user-recoverable error — panic at
    // the call site rather than mask it behind ECALLDENIED.
    let caller_idx = proc_index(caller_nr).expect("caller in proc table");
    let caller_priv_id = proc_table[caller_idx]
        .priv_id
        .expect("caller priv populated");
    let system_idx = proc_index(SYSTEM).expect("SYSTEM in proc table");
    let system_priv_id = proc_table[system_idx]
        .priv_id
        .expect("SYSTEM priv populated by proc::init");

    // Apply the same ipc_to permission check that `mini_send` does
    // (`ipc/send.rs:59`) — the SYSTEM endpoint isn't a special trust
    // boundary, just a routing shortcut, so a caller without SYSTEM in its
    // ipc_to bitmap must still be denied here.
    if !get_sys_bit(
        &priv_table[caller_priv_id.as_usize()].ipc_to,
        system_priv_id,
    ) {
        return ECALLDENIED;
    }

    let mut msg = match copy_msg_from_user(user_msg_va) {
        Ok(m) => m,
        Err(e) => return e,
    };
    let original_m_type = msg.m_type;

    // Stamp m_source so handlers see the verified caller endpoint rather
    // than whatever the user scribbled into the request.
    msg.m_source = proc_table[caller_idx].endpoint;

    // Dispatch. proc_table and priv_table are disjoint statics, so
    // borrowing one slot in each is permitted.
    let result = {
        let caller = &mut proc_table[caller_idx];
        let caller_priv = &priv_table[caller_priv_id.as_usize()];
        kernel_call_dispatch(caller, caller_priv, &mut msg)
    };

    // Build the reply.
    msg.m_source = system_endpoint();
    msg.m_type = result;
    if let Err(e) = copy_msg_to_user(user_msg_va, &msg) {
        return e;
    }

    let n = CALL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n % KCALL_TRACE_EVERY == 0 {
        let call_idx = original_m_type - KERNEL_CALL;
        let mut uart = Uart::new();
        let _ = writeln!(
            uart,
            "[ksys {n}] caller={caller_nr} call={call_idx} result={result}",
        );
    }

    OK
}

/// Per-call dispatcher.
///
/// Steps: range-check `m_type - KERNEL_CALL`, gate against
/// `Priv::k_call_mask`, then match on `m_type` to invoke the handler. Returns
/// the result code that becomes the reply's `m_type`.
fn kernel_call_dispatch(
    caller: &mut Proc,
    caller_priv: &Priv,
    msg: &mut Message,
) -> i32 {
    let call_idx = msg.m_type - KERNEL_CALL;
    if call_idx < 0 || (call_idx as usize) >= NR_SYS_CALLS {
        return EBADREQUEST;
    }
    let call_idx = call_idx as usize;

    if !get_call_bit(&caller_priv.k_call_mask, call_idx) {
        return ECALLDENIED;
    }

    // Match must cover every SYS_* defined in callnr.rs. The const assert
    // locks the arm count — adding a new SYS_* without a new arm here is a
    // compile error.
    const _: () = assert!(
        NR_KERN_CALLS_PHASE2 == 14,
        "expand kernel_call_dispatch when a new SYS_* lands",
    );
    match msg.m_type {
        SYS_GETINFO => do_getinfo::do_getinfo(caller, caller_priv, msg),
        SYS_PRIVCTL => stubs::do_privctl(caller, caller_priv, msg),
        SYS_FORK => stubs::do_fork(caller, caller_priv, msg),
        SYS_EXEC => stubs::do_exec(caller, caller_priv, msg),
        SYS_EXIT => stubs::do_exit(caller, caller_priv, msg),
        SYS_COPY => stubs::do_copy(caller, caller_priv, msg),
        SYS_SAFECOPY => stubs::do_safecopy(caller, caller_priv, msg),
        SYS_IRQCTL => stubs::do_irqctl(caller, caller_priv, msg),
        SYS_VMCTL => stubs::do_vmctl(caller, caller_priv, msg),
        SYS_SCHEDULE => stubs::do_schedule(caller, caller_priv, msg),
        SYS_SETALARM => stubs::do_setalarm(caller, caller_priv, msg),
        SYS_TIMES => stubs::do_times(caller, caller_priv, msg),
        SYS_DIAGCTL => stubs::do_diagctl(caller, caller_priv, msg),
        SYS_SETGRANT => stubs::do_setgrant(caller, caller_priv, msg),
        _ => EBADREQUEST,
    }
}
