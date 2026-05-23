//! Priority-banded round-robin scheduler.
//!
//! Slice 2.4 grows `proc::sched` from the slice-2.3 stub (one fixed
//! current-proc tracker) into a real run-queue scheduler. Layout mirrors
//! MINIX 3 `kernel/proc.c:1605` (`enqueue`), `:1726` (`dequeue`), and
//! `:1795` (`pick_proc`), with two simplifications:
//!
//! - Queue linkage is a [`ProcNr`] index (`Proc::next_ready`) rather than a
//!   raw `struct proc *` — the project-wide convention from `CLAUDE.md`.
//! - No per-CPU run queues. Slice 2.4 is single-CPU.
//!
//! Concurrency model: the kernel runs DAIF-masked, so neither EL1 code nor
//! the IRQ handler can be interrupted by *another* IRQ before it finishes.
//! The IRQ stub itself is the only async writer; it runs to completion
//! before returning to EL0. That gives us the same single-threaded invariant
//! as the rest of the boot path, so we use the same `UnsafeCell` + `Sync`
//! newtype pattern documented on `ProcStorage` / `PrivStorage`.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicI32, Ordering};

use minix4_kernel_shared::ProcNr;

use crate::arch::ArchRegisterFrame;
use crate::proc::Proc;
use crate::proc::flags::RTS_NO_QUANTUM;
use crate::proc::table::proc_slot_mut;

/// Number of scheduling-priority bands. Matches the constants in
/// `proc::table` (`TASK_Q = 0`, `IDLE_Q = 15`).
pub const NR_SCHED_QUEUES: usize = 16;

/// Currently-running process number. `i32::MIN` is the "no process running"
/// sentinel — chosen because it can never collide with a real `ProcNr`
/// (kernel tasks are `-5..=-1`, user processes are `0..NR_PROCS`).
pub static CURRENT_PROC_NR: AtomicI32 = AtomicI32::new(i32::MIN);

const NO_PROC: i32 = i32::MIN;

// ---------------------------------------------------------------------------
// Run-queue storage.
// ---------------------------------------------------------------------------

struct RunQueues {
    head: [Option<ProcNr>; NR_SCHED_QUEUES],
    tail: [Option<ProcNr>; NR_SCHED_QUEUES],
}

#[repr(transparent)]
struct RunQueueStorage(UnsafeCell<RunQueues>);
// SAFETY: single-CPU; the kernel runs DAIF-masked, so the only async writer
// is the IRQ stub, which runs to completion before returning to EL0. No
// concurrent access is possible.
unsafe impl Sync for RunQueueStorage {}

static RUNQ: RunQueueStorage = RunQueueStorage(UnsafeCell::new(RunQueues {
    head: [None; NR_SCHED_QUEUES],
    tail: [None; NR_SCHED_QUEUES],
}));

// ---------------------------------------------------------------------------
// Current-proc accessors.
// ---------------------------------------------------------------------------

/// Borrow the currently-running process's slot.
///
/// SAFETY: caller must respect the single-threaded-boot invariant on
/// `PROC_TABLE`. The returned `&'static mut Proc` must not alias any other
/// reference into the table.
pub unsafe fn current_proc_mut() -> Option<&'static mut Proc> {
    let n = CURRENT_PROC_NR.load(Ordering::Relaxed);
    if n == NO_PROC {
        return None;
    }
    // SAFETY: forwarded — caller holds the no-aliasing invariant.
    unsafe { proc_slot_mut(ProcNr::new(n)) }
}

// ---------------------------------------------------------------------------
// Run-queue operations.
// ---------------------------------------------------------------------------

/// Append `nr` to the tail of its priority band's run queue.
///
/// Mirrors MINIX 3 `kernel/proc.c:1605` `enqueue`. Panics if the process is
/// already linked into a queue.
///
/// SAFETY: caller must hold the single-threaded-boot invariant; no other
/// mutable reference into `PROC_TABLE` or `RUNQ` may be live.
pub unsafe fn enqueue(nr: ProcNr) {
    let prio = unsafe {
        let p = proc_slot_mut(nr).expect("enqueue: nr out of range");
        assert!(p.next_ready.is_none(), "enqueue: process already queued");
        p.priority as usize
    };
    assert!(prio < NR_SCHED_QUEUES, "enqueue: priority out of range");

    // SAFETY: RUNQ is a distinct static from PROC_TABLE; the prior
    // proc_slot_mut borrow has been dropped.
    let runq = unsafe { &mut *RUNQ.0.get() };
    match runq.tail[prio].replace(nr) {
        None => {
            // Queue was empty — set head too.
            runq.head[prio] = Some(nr);
        }
        Some(prev_tail) => {
            // SAFETY: prev_tail was in the queue, so its slot is valid.
            let tail_p = unsafe {
                proc_slot_mut(prev_tail).expect("enqueue: prev tail out of range")
            };
            tail_p.next_ready = Some(nr);
        }
    }
}

/// Splice `nr` out of its priority band's run queue. No-op if not queued.
///
/// Mirrors MINIX 3 `kernel/proc.c:1726` `dequeue`. Slice 2.4's queues are
/// tiny (at most two entries), so the linear walk is fine.
///
/// SAFETY: caller must hold the single-threaded-boot invariant; no other
/// mutable reference into `PROC_TABLE` or `RUNQ` may be live.
pub unsafe fn dequeue(nr: ProcNr) {
    let prio = unsafe {
        proc_slot_mut(nr).expect("dequeue: nr out of range").priority as usize
    };
    // SAFETY: RUNQ is a distinct static from PROC_TABLE.
    let runq = unsafe { &mut *RUNQ.0.get() };

    let mut prev: Option<ProcNr> = None;
    let mut cur = runq.head[prio];
    while let Some(c) = cur {
        // SAFETY: c was on the queue, so its slot is valid; no other live
        // reference into c's slot exists here.
        let next_after_c = unsafe {
            proc_slot_mut(c).expect("dequeue: c out of range").next_ready
        };
        if c == nr {
            // Clear the dequeued node's link.
            // SAFETY: same — single live reference at a time.
            unsafe {
                proc_slot_mut(c).expect("dequeue: c out of range").next_ready = None;
            }
            match prev {
                None => runq.head[prio] = next_after_c,
                Some(pn) => {
                    // SAFETY: pn was traversed already and is in the queue.
                    unsafe {
                        proc_slot_mut(pn)
                            .expect("dequeue: prev out of range")
                            .next_ready = next_after_c;
                    }
                }
            }
            if runq.tail[prio] == Some(c) {
                runq.tail[prio] = prev;
            }
            return;
        }
        prev = Some(c);
        cur = next_after_c;
    }
    // Not found — leave the queue untouched.
}

/// Return the head of the highest-priority non-empty run queue.
///
/// Mirrors MINIX 3 `kernel/proc.c:1795` `pick_proc`. Lower priority value =
/// higher priority, so we scan `0..NR_SCHED_QUEUES` and return the first
/// non-empty head.
pub fn pick_proc() -> Option<ProcNr> {
    // SAFETY: read-only snapshot under the single-threaded invariant.
    let runq = unsafe { &*RUNQ.0.get() };
    runq.head.iter().find_map(|&h| h)
}

// ---------------------------------------------------------------------------
// Dispatch helpers.
// ---------------------------------------------------------------------------

unsafe extern "C" {
    /// Restores `tpidr_el1`-pointed `ArchRegisterFrame` and erets into EL0.
    /// Defined in `kernel/src/arch/aarch64/trap.S`.
    fn el1_return_to_user() -> !;
}

/// Park `&mut p.regs` in `TPIDR_EL1` so the SVC/IRQ entry paths and
/// `el1_return_to_user` can locate the current process's register frame.
///
/// SAFETY: `p` must be the slot whose number is currently parked in
/// `CURRENT_PROC_NR`. Caller must hold the single-threaded invariant.
unsafe fn set_tpidr_to(p: &mut Proc) {
    let regs_ptr: *mut ArchRegisterFrame = &mut p.regs;
    // SAFETY: TPIDR_EL1 is EL1-only and used only by our own SVC + IRQ stubs
    // and `el1_return_to_user`. No other code reads or writes it.
    unsafe {
        core::arch::asm!(
            "msr tpidr_el1, {0}",
            "isb",
            in(reg) regs_ptr,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// First entry into EL0 from `kmain`.
///
/// Pre-condition: the desired-to-run procs have already been [`enqueue`]d.
/// Picks the highest-priority head, parks its frame in `TPIDR_EL1`, and
/// jumps to the assembly restore-and-`eret` stub. Never returns.
///
/// SAFETY: at least one process must be enqueued (otherwise we'd `panic` in
/// EL1 with the boot stack still active). The picked process's `regs` must
/// be fully initialized — see `userland_bootstrap`. Must be called with
/// DAIF (I) still masked at EL1; the per-proc SPSR carries the EL0 mask
/// state that takes effect on `eret`.
pub unsafe fn run() -> ! {
    let first_nr = pick_proc().expect("run: empty run queue");
    CURRENT_PROC_NR.store(first_nr.get(), Ordering::Relaxed);
    // SAFETY: single-threaded boot; we hold the only reference into the
    // first proc's slot.
    let first = unsafe { proc_slot_mut(first_nr).expect("run: nr out of range") };
    // SAFETY: same — exclusive borrow above.
    unsafe { set_tpidr_to(first) };
    // SAFETY: trap.S contract documented at `el1_return_to_user`.
    unsafe { el1_return_to_user() }
}

/// Quantum-exhaust path: re-enqueue the current proc at the tail of its
/// priority band, pick the next runnable, and update `TPIDR_EL1`.
///
/// Called from `clock::tick` when the running proc's `quantum_left` hits
/// zero. The IRQ stub's trailing `bl el1_return_to_user` reads the
/// (possibly new) `TPIDR_EL1` and restores that frame, so the context
/// switch happens transparently.
///
/// If `pick_proc` somehow finds no runnable proc (we just re-enqueued
/// current, so it should always find at least that one), we leave
/// `TPIDR_EL1` alone and stay on the current proc.
///
/// SAFETY: caller holds the single-threaded invariant; called only from
/// IRQ context with no other PROC_TABLE / RUNQ borrows live.
pub unsafe fn reschedule() {
    let cur_raw = CURRENT_PROC_NR.load(Ordering::Relaxed);
    if cur_raw != NO_PROC {
        let cur_nr = ProcNr::new(cur_raw);
        // Refill quantum and clear the quantum-exhaust flag *before*
        // re-enqueuing, so the re-enqueued process is in the runnable state.
        // SAFETY: single-threaded; cur_nr came from CURRENT_PROC_NR.
        unsafe {
            let cur = proc_slot_mut(cur_nr).expect("reschedule: current out of range");
            cur.quantum_left = cur.quantum_ms as u64;
            cur.rts_flags.fetch_and(!RTS_NO_QUANTUM, Ordering::Relaxed);
        }
        // SAFETY: cur_nr borrow above has been dropped.
        unsafe {
            dequeue(cur_nr);
            enqueue(cur_nr);
        }
    }

    let next_nr = match pick_proc() {
        Some(nr) => nr,
        None => return, // No runnable procs — stay on current.
    };
    CURRENT_PROC_NR.store(next_nr.get(), Ordering::Relaxed);
    // SAFETY: next_nr is in range (came from a queue we just maintained).
    let next = unsafe { proc_slot_mut(next_nr).expect("reschedule: next out of range") };
    // SAFETY: exclusive borrow above.
    unsafe { set_tpidr_to(next) };
}
