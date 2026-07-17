// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! PM's process table (slice 4.5) — the minix.rs `mproc`.
//!
//! Modeled on MINIX 3 `servers/pm/mproc.h`: a static table indexed by the
//! *same* process number as the kernel's proc table, each entry carrying the
//! POSIX-visible identity (pid, parent) the kernel deliberately knows nothing
//! about. MINIX seeds it from `sys_getimage`; minix.rs seeds it from the
//! shared boot-image constants in `kernel-shared::com` — same information,
//! no kernel call needed while the boot layout is static.
//!
//! Seeding (mirrors MINIX `pm_init`): PM is pid 0 and its own parent; INIT is
//! pid 1; every other boot server takes the next free pid in slot order with
//! RS as parent (RS itself is parented to PM so no server is its own
//! ancestor); the Phase-4 demo stubs are ordinary user processes parented to
//! INIT. Boot servers carry [`MF_PRIV_PROC`], which the kill path refuses to
//! terminate — delivering signals *to* system processes (MINIX's sig2mess)
//! waits for a consumer (RS restarts).
//!
//! Table shape follows `vm/region.rs` / `ds/registry.rs`: a fixed-capacity
//! `UnsafeCell` static behind a `#[repr(transparent)]` newtype, with all
//! logic in pure `*_in` helpers that carry the host unit tests; the thin
//! wrappers reaching the static are the only `unsafe`.

use core::cell::UnsafeCell;

use minixrs_kernel_shared::com::{
    INIT_PROC_NR, NR_BOOT_PROCS, NR_STUB_PROCS, PM_PROC_NR, RS_PROC_NR,
};

/// Table capacity. Slots `0..NR_BOOT_PROCS + NR_STUB_PROCS` (= 16) are seeded
/// at init; the headroom is for slice 4.6's fork.
pub const NR_MPROCS: usize = 32;

/// Entry holds a live process.
pub const MF_IN_USE: u8 = 1 << 0;
/// Boot system process — the kill path refuses to terminate it.
pub const MF_PRIV_PROC: u8 = 1 << 1;
/// Terminated by the kill path. The slot is kept (not reusable) until 4.6's
/// exit/wait lands zombie + reap semantics.
pub const MF_DEAD: u8 = 1 << 2;

/// One `mproc` entry. The slot index *is* the kernel proc number; the
/// endpoint is derivable as `boot_endpoint(slot)` while everything is
/// generation 0 (4.6's fork adds generations and will store endpoints).
#[derive(Clone, Copy)]
pub struct MProc {
    pub pid: i32,
    pub parent_slot: usize,
    pub flags: u8,
}

impl MProc {
    pub const EMPTY: Self = Self {
        pid: 0,
        parent_slot: 0,
        flags: 0,
    };
}

/// Disposition of a kill request against a slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillAction {
    /// Ordinary user process: caller should terminate it (`SYS_EXIT`).
    Terminate,
    /// Boot system process: refuse — sig2mess is a later slice.
    SkipPrivileged,
    /// Unknown, unused, or already-dead slot: nothing to do.
    NotFound,
}

/// Seed the table with the boot image + demo stubs. Pure; called once from
/// PM's SEF init via [`seed`].
fn seed_in(t: &mut [MProc; NR_MPROCS]) {
    let pm = PM_PROC_NR.get() as usize;
    let rs = RS_PROC_NR.get() as usize;
    let init = INIT_PROC_NR.get() as usize;

    // PM: pid 0, its own parent (MINIX `pm_init` patches itself the same way).
    t[pm] = MProc {
        pid: 0,
        parent_slot: pm,
        flags: MF_IN_USE | MF_PRIV_PROC,
    };
    // INIT: pid 1. Not running yet (no ELF until 4.8) but seeded so the demo
    // stubs have a parent with a pid.
    t[init] = MProc {
        pid: 1,
        parent_slot: rs,
        flags: MF_IN_USE | MF_PRIV_PROC,
    };

    // Remaining boot servers: next free pids in slot order, parented to RS
    // (the root system process) — except RS itself, parented to PM so no
    // process is its own ancestor besides PM.
    let mut next_pid = 2;
    for (slot, e) in t.iter_mut().enumerate().take(NR_BOOT_PROCS) {
        if slot == pm || slot == init {
            continue;
        }
        *e = MProc {
            pid: next_pid,
            parent_slot: if slot == rs { pm } else { rs },
            flags: MF_IN_USE | MF_PRIV_PROC,
        };
        next_pid += 1;
    }

    // Demo stubs: ordinary user processes, parented to INIT, pids continuing
    // past the servers (slots 11..=15 land on pids 11..=15).
    for e in t.iter_mut().skip(NR_BOOT_PROCS).take(NR_STUB_PROCS) {
        *e = MProc {
            pid: next_pid,
            parent_slot: init,
            flags: MF_IN_USE,
        };
        next_pid += 1;
    }
}

/// Look up `(pid, ppid)` for `slot`. `None` for out-of-range, unused, or dead
/// slots — the caller replies `ESRCH`.
fn getpid_in(t: &[MProc; NR_MPROCS], slot: usize) -> Option<(i32, i32)> {
    let e = t.get(slot)?;
    if e.flags & MF_IN_USE == 0 || e.flags & MF_DEAD != 0 {
        return None;
    }
    Some((e.pid, t[e.parent_slot].pid))
}

/// Decide the kill disposition for `slot` and, when it is an ordinary live
/// user process, mark it dead. The caller performs the actual `SYS_EXIT`.
fn handle_kill_in(t: &mut [MProc; NR_MPROCS], slot: usize) -> KillAction {
    let Some(e) = t.get_mut(slot) else {
        return KillAction::NotFound;
    };
    if e.flags & MF_IN_USE == 0 || e.flags & MF_DEAD != 0 {
        return KillAction::NotFound;
    }
    if e.flags & MF_PRIV_PROC != 0 {
        return KillAction::SkipPrivileged;
    }
    e.flags |= MF_DEAD;
    KillAction::Terminate
}

/// `UnsafeCell`-wrapped static table. See the module note for the
/// single-mutator invariant that makes the `Sync` impl sound.
#[repr(transparent)]
struct Table(UnsafeCell<[MProc; NR_MPROCS]>);

// SAFETY: PM is a single-threaded EL0 process with no interrupt handlers of
// its own; the table is only ever accessed from PM's straight-line SEF
// init + receive loop, so there is never concurrent access.
unsafe impl Sync for Table {}

static TABLE: Table = Table(UnsafeCell::new([MProc::EMPTY; NR_MPROCS]));

/// Seed the global table from the boot-image constants.
pub fn seed() {
    // SAFETY: single-mutator invariant (module note); no other reference into
    // the table is live during PM's straight-line init.
    let t = unsafe { &mut *TABLE.0.get() };
    seed_in(t);
}

/// Look up `(pid, ppid)` for `slot` in the global table.
pub fn getpid(slot: usize) -> Option<(i32, i32)> {
    // SAFETY: single-mutator invariant (module note); shared read, no live `&mut`.
    let t = unsafe { &*TABLE.0.get() };
    getpid_in(t, slot)
}

/// Decide (and record) the kill disposition for `slot` in the global table.
pub fn handle_kill(slot: usize) -> KillAction {
    // SAFETY: single-mutator invariant (module note); no other reference into
    // the table is live during PM's straight-line loop.
    let t = unsafe { &mut *TABLE.0.get() };
    handle_kill_in(t, slot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::com::{
        SCHED_PROC_NR, STUB_A_PROC_NR, STUB_D_PROC_NR, STUB_E_PROC_NR, VFS_PROC_NR, VM_PROC_NR,
    };

    fn seeded() -> [MProc; NR_MPROCS] {
        let mut t = [MProc::EMPTY; NR_MPROCS];
        seed_in(&mut t);
        t
    }

    #[test]
    fn seed_assigns_boot_pids() {
        let t = seeded();
        // PM pid 0, INIT pid 1, then slot order: VFS(1)=2, RS(2)=3 … SCHED(9)=10.
        assert_eq!(t[PM_PROC_NR.get() as usize].pid, 0);
        assert_eq!(t[INIT_PROC_NR.get() as usize].pid, 1);
        assert_eq!(t[VFS_PROC_NR.get() as usize].pid, 2);
        assert_eq!(t[RS_PROC_NR.get() as usize].pid, 3);
        assert_eq!(t[VM_PROC_NR.get() as usize].pid, 8);
        assert_eq!(t[SCHED_PROC_NR.get() as usize].pid, 10);
        for (slot, e) in t.iter().enumerate().take(NR_BOOT_PROCS) {
            assert_eq!(e.flags, MF_IN_USE | MF_PRIV_PROC, "slot {slot}");
        }
    }

    #[test]
    fn seed_stub_pids_continue() {
        let t = seeded();
        for (i, slot) in (NR_BOOT_PROCS..NR_BOOT_PROCS + NR_STUB_PROCS).enumerate() {
            assert_eq!(t[slot].pid, 11 + i as i32);
            assert_eq!(t[slot].flags, MF_IN_USE, "stubs are not PRIV_PROC");
            assert_eq!(t[slot].parent_slot, INIT_PROC_NR.get() as usize);
        }
    }

    #[test]
    fn seed_pids_are_unique() {
        let t = seeded();
        let mut seen = [false; 64];
        for e in t.iter().filter(|e| e.flags & MF_IN_USE != 0) {
            let pid = e.pid as usize;
            assert!(!seen[pid], "duplicate pid {pid}");
            seen[pid] = true;
        }
    }

    #[test]
    fn getpid_returns_pid_and_ppid() {
        let t = seeded();
        // PM is its own parent: (0, 0).
        assert_eq!(getpid_in(&t, PM_PROC_NR.get() as usize), Some((0, 0)));
        // VM's parent is RS (pid 3).
        assert_eq!(getpid_in(&t, VM_PROC_NR.get() as usize), Some((8, 3)));
        // Stubs are parented to INIT (pid 1).
        assert_eq!(getpid_in(&t, STUB_A_PROC_NR.get() as usize), Some((11, 1)));
        assert_eq!(getpid_in(&t, STUB_E_PROC_NR.get() as usize), Some((15, 1)));
    }

    #[test]
    fn getpid_unknown_slot_is_none() {
        let t = seeded();
        assert_eq!(getpid_in(&t, 16), None, "unseeded slot");
        assert_eq!(getpid_in(&t, NR_MPROCS), None, "out of range");
    }

    #[test]
    fn getpid_dead_slot_is_none() {
        let mut t = seeded();
        let d = STUB_D_PROC_NR.get() as usize;
        assert_eq!(handle_kill_in(&mut t, d), KillAction::Terminate);
        assert_eq!(getpid_in(&t, d), None, "terminated proc has no pid");
    }

    #[test]
    fn kill_user_proc_terminates_and_marks_dead() {
        let mut t = seeded();
        let d = STUB_D_PROC_NR.get() as usize;
        assert_eq!(handle_kill_in(&mut t, d), KillAction::Terminate);
        assert_ne!(t[d].flags & MF_DEAD, 0);
    }

    #[test]
    fn kill_priv_proc_is_skipped() {
        let mut t = seeded();
        let vm = VM_PROC_NR.get() as usize;
        assert_eq!(handle_kill_in(&mut t, vm), KillAction::SkipPrivileged);
        assert_eq!(t[vm].flags & MF_DEAD, 0, "server must not be marked dead");
    }

    #[test]
    fn kill_dead_or_unknown_is_notfound() {
        let mut t = seeded();
        let d = STUB_D_PROC_NR.get() as usize;
        assert_eq!(handle_kill_in(&mut t, d), KillAction::Terminate);
        // A second kill on the same (now dead) slot finds nothing to do.
        assert_eq!(handle_kill_in(&mut t, d), KillAction::NotFound);
        assert_eq!(handle_kill_in(&mut t, 20), KillAction::NotFound);
        assert_eq!(handle_kill_in(&mut t, NR_MPROCS), KillAction::NotFound);
    }
}
