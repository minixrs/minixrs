//! Cyclic-dependency detection for IPC blocking.
//!
//! Translation of MINIX 3 `kernel/proc.c:713 deadlock()`. When a caller is
//! about to block on `SEND` to `dst` (or `RECEIVE` from `src`), this walks
//! the chain of "what is each process blocked on" looking for a cycle back
//! to the caller. The classic size-2 SEND ↔ RECEIVE rendezvous (caller
//! wants to SEND, target is blocked in RECEIVE waiting for us — or the
//! mirror image) is legal and explicitly excluded; any larger cycle, or
//! a same-orientation 2-cycle (both SEND, both RECEIVE), is a deadlock.

use core::sync::atomic::Ordering;

use minix4_kernel_shared::ProcNr;
use minix4_kernel_shared::endpoint::{Endpoint, endpoint_proc};
use minix4_kernel_shared::ipc_const::SEND;

use crate::proc::Proc;
use crate::proc::flags::{RTS_RECEIVING, RTS_SENDING};
use crate::proc::table::{N_PROC_SLOTS, proc_index};

/// Returns `true` iff blocking `caller_nr` on `target_e` under primitive
/// `function` would create a cyclic dependency that is NOT the legal
/// size-2 SEND ↔ RECEIVE pair.
///
/// `function` must be `SEND` or `RECEIVE` (the only callers in MINIX 4 —
/// MINIX 3's NOTIFY/SENDNB paths don't invoke the deadlock check). The
/// "function << 2" trick maps each value onto its corresponding RTS bit:
/// `SEND(1) << 2 = RTS_SENDING(4)`, `RECEIVE(2) << 2 = RTS_RECEIVING(8)`.
pub fn deadlock_check(
    table: &[Proc; N_PROC_SLOTS],
    function: i32,
    caller_nr: ProcNr,
    target_e: Endpoint,
) -> bool {
    // Just verify caller_nr is in range; we don't otherwise inspect the
    // caller — the cycle check threads through endpoints, not slot indices.
    if proc_index(caller_nr).is_none() {
        return false;
    }

    let mut group: u32 = 1;
    let mut cur_e = target_e;

    loop {
        let cur_nr = endpoint_proc(cur_e);
        // Self-send / self-recv is the trivial size-1 cycle.
        if cur_nr == caller_nr {
            return true;
        }
        let Some(idx) = proc_index(cur_nr) else {
            return false;
        };
        let xp = &table[idx];
        group += 1;

        // Size-2 SEND ↔ RECEIVE legalization (MINIX 3 proc.c:756 trick).
        // If the bit XOR isolates `RTS_SENDING` it means caller and `xp`
        // are blocked in opposite directions — a legal rendezvous, not a
        // deadlock.
        let xp_rts = xp.rts_flags.load(Ordering::Relaxed);
        if group == 2 {
            let shifted = (function as u32).wrapping_shl(2);
            if (xp_rts ^ shifted) & RTS_SENDING != 0 {
                return false;
            }
        }

        // Follow `xp`'s blocked-on edge.
        let next_e = if xp_rts & RTS_SENDING != 0 {
            xp.sendto_e
        } else if xp_rts & RTS_RECEIVING != 0 {
            xp.getfrom_e
        } else {
            // `xp` is not blocked anywhere — chain terminates without a
            // cycle.
            return false;
        };

        if endpoint_proc(next_e) == caller_nr {
            // Closed the loop back to caller, and the size-2 legal-pair
            // escape above didn't fire — that's a real deadlock.
            return true;
        }

        // Safety net against bookkeeping bugs.
        if group as usize > N_PROC_SLOTS {
            return true;
        }

        cur_e = next_e;
    }
}

// Silence unused-import warnings when no consumer has been hooked up yet.
#[allow(dead_code)]
const _: i32 = SEND;
