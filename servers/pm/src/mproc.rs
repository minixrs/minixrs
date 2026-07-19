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
    INIT_PROC_NR, NR_BOOT_PROCS, NR_STUB_PROCS, PM_PROC_NR, RS_PROC_NR, boot_endpoint,
};
use minixrs_kernel_shared::endpoint::{Endpoint, ProcNr};

/// Table capacity. Slots `0..NR_BOOT_PROCS + NR_STUB_PROCS` (= 15 since stub E's
/// slice-4.8 retirement) are seeded at init; the pool above that
/// ([`FORK_POOL_BASE`]`..NR_MPROCS`) is where 4.6's fork allocates children.
pub const NR_MPROCS: usize = 32;

/// First slot fork may allocate. Boot servers + demo stubs own `[0, 15)`; forked
/// children land in `[FORK_POOL_BASE, NR_MPROCS)`. A child's slot index is also
/// its kernel proc number, so this range must stay within the kernel proc table
/// and within VM's `MAX_CLIENTS` region-table cap (both hold).
pub const FORK_POOL_BASE: usize = NR_BOOT_PROCS + NR_STUB_PROCS;

/// Entry holds a live process.
pub const MF_IN_USE: u8 = 1 << 0;
/// Boot system process — the kill path refuses to terminate it.
pub const MF_PRIV_PROC: u8 = 1 << 1;
/// Terminated (a zombie): the process is gone at the kernel level (`SYS_EXIT`
/// ran) but its `mproc` slot is retained — holding the exit status — until its
/// parent `wait()`s and reaps it. Set by both the exit path ([`set_zombie_in`],
/// with a real status) and the kill path ([`handle_kill_in`]).
pub const MF_DEAD: u8 = 1 << 2;
/// This process is a parent blocked in `wait()` with no reapable child yet;
/// when a child exits, [`set_zombie_in`]'s caller wakes it directly.
pub const MF_WAITING: u8 = 1 << 3;

/// One `mproc` entry. The slot index *is* the kernel proc number, but once slots
/// recycle (4.6 fork/exit bump the endpoint generation) `boot_endpoint(slot)` no
/// longer identifies the occupant, so the generation-aware endpoint is stored
/// explicitly.
#[derive(Clone, Copy)]
pub struct MProc {
    pub pid: i32,
    pub parent_slot: usize,
    /// Generation-aware endpoint of this process (from `SYS_FORK` for children,
    /// `boot_endpoint(slot)` for seeded boot/stub procs).
    pub endpoint: Endpoint,
    /// Encoded exit status stored while the process is a zombie (`MF_DEAD`).
    pub exit_status: i32,
    pub flags: u8,
}

impl MProc {
    pub const EMPTY: Self = Self {
        pid: 0,
        parent_slot: 0,
        endpoint: 0,
        exit_status: 0,
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

    // Every seeded proc is at generation 0, so its endpoint is `boot_endpoint`
    // of its slot (= kernel proc number). Forked children later store the
    // generation-aware endpoint `SYS_FORK` hands back.
    let ep = |slot: usize| boot_endpoint(ProcNr::new(slot as i32));

    // PM: pid 0, its own parent (MINIX `pm_init` patches itself the same way).
    t[pm] = MProc {
        pid: 0,
        parent_slot: pm,
        endpoint: ep(pm),
        exit_status: 0,
        flags: MF_IN_USE | MF_PRIV_PROC,
    };
    // INIT: pid 1. A real boot process as of slice 4.8 (the kernel loader makes
    // it runnable); it drives fork/exec/wait through PM. `MF_PRIV_PROC` keeps it
    // unkillable (correct for PID 1) — that flag gates only the kill path, not
    // fork/wait/getpid, so PM still serves it as an ordinary client. It also
    // parents the demo stubs (pids advance from it below).
    t[init] = MProc {
        pid: 1,
        parent_slot: rs,
        endpoint: ep(init),
        exit_status: 0,
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
            endpoint: ep(slot),
            exit_status: 0,
            flags: MF_IN_USE | MF_PRIV_PROC,
        };
        next_pid += 1;
    }

    // Demo stubs: ordinary user processes, parented to INIT, pids continuing
    // past the servers (slots 11..=15 land on pids 11..=15).
    for (slot, e) in t
        .iter_mut()
        .enumerate()
        .skip(NR_BOOT_PROCS)
        .take(NR_STUB_PROCS)
    {
        *e = MProc {
            pid: next_pid,
            parent_slot: init,
            endpoint: ep(slot),
            exit_status: 0,
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

// ---------------------------------------------------------------------------
// Fork / exit / wait table management (slice 4.6b). All pure `*_in` helpers.
// ---------------------------------------------------------------------------

/// True if `slot` holds a live (in-use, not-yet-reaped) process.
fn in_use_in(t: &[MProc; NR_MPROCS], slot: usize) -> bool {
    t.get(slot).is_some_and(|e| e.flags & MF_IN_USE != 0)
}

/// The parent slot of `slot`, or `None` if the slot is not in use.
fn parent_of_in(t: &[MProc; NR_MPROCS], slot: usize) -> Option<usize> {
    let e = t.get(slot)?;
    (e.flags & MF_IN_USE != 0).then_some(e.parent_slot)
}

/// The stored endpoint of `slot`, or `None` if the slot is not in use.
fn endpoint_of_in(t: &[MProc; NR_MPROCS], slot: usize) -> Option<Endpoint> {
    let e = t.get(slot)?;
    (e.flags & MF_IN_USE != 0).then_some(e.endpoint)
}

/// First free slot in the fork pool `[FORK_POOL_BASE, NR_MPROCS)`, or `None`
/// when the table is full (PM replies `EAGAIN`). MINIX `do_fork` scans `mproc`
/// the same way; the slot index becomes the child's kernel proc number.
fn alloc_slot_in(t: &[MProc; NR_MPROCS]) -> Option<usize> {
    (FORK_POOL_BASE..NR_MPROCS).find(|&slot| t[slot].flags == 0)
}

/// Next pid to assign: one past the highest pid any current entry holds (live
/// or zombie), so a pid is never reused while its predecessor is still around.
/// Pids climb monotonically as long as the high-water entry persists; after the
/// whole fork pool drains they may repeat — harmless, since the endpoint
/// generation (not the pid) is what guards slot recycling.
fn alloc_pid_in(t: &[MProc; NR_MPROCS]) -> i32 {
    let max = t
        .iter()
        .filter(|e| e.flags != 0)
        .map(|e| e.pid)
        .max()
        .unwrap_or(0);
    max + 1
}

/// Populate a freshly forked child at `slot`, returning its assigned pid. The
/// caller passes the generation-aware endpoint `SYS_FORK` handed back.
fn set_child_in(
    t: &mut [MProc; NR_MPROCS],
    slot: usize,
    parent_slot: usize,
    endpoint: Endpoint,
) -> i32 {
    let pid = alloc_pid_in(t);
    t[slot] = MProc {
        pid,
        parent_slot,
        endpoint,
        exit_status: 0,
        flags: MF_IN_USE,
    };
    pid
}

/// Mark `slot` a zombie (terminated, awaiting reap), recording `status`.
fn set_zombie_in(t: &mut [MProc; NR_MPROCS], slot: usize, status: i32) {
    if let Some(e) = t.get_mut(slot) {
        e.exit_status = status;
        e.flags |= MF_DEAD;
    }
}

/// Set or clear `slot`'s `MF_WAITING` (parent blocked in `wait()`).
fn set_waiting_in(t: &mut [MProc; NR_MPROCS], slot: usize, waiting: bool) {
    if let Some(e) = t.get_mut(slot) {
        if waiting {
            e.flags |= MF_WAITING;
        } else {
            e.flags &= !MF_WAITING;
        }
    }
}

/// True if `slot` is a parent blocked in `wait()`.
fn is_waiting_in(t: &[MProc; NR_MPROCS], slot: usize) -> bool {
    t.get(slot).is_some_and(|e| e.flags & MF_WAITING != 0)
}

/// First zombie child of `parent_slot`, as `(slot, pid, exit_status)`, or `None`.
fn find_zombie_child_in(t: &[MProc; NR_MPROCS], parent_slot: usize) -> Option<(usize, i32, i32)> {
    t.iter().enumerate().find_map(|(slot, e)| {
        (e.flags & MF_IN_USE != 0 && e.flags & MF_DEAD != 0 && e.parent_slot == parent_slot)
            .then_some((slot, e.pid, e.exit_status))
    })
}

/// True if `parent_slot` has at least one still-live (non-zombie) child.
fn has_live_child_in(t: &[MProc; NR_MPROCS], parent_slot: usize) -> bool {
    t.iter().enumerate().any(|(slot, e)| {
        slot != parent_slot
            && e.flags & MF_IN_USE != 0
            && e.flags & MF_DEAD == 0
            && e.parent_slot == parent_slot
    })
}

/// Release `slot` back to the free pool (fork rollback, or reaping a zombie).
fn cleanup_in(t: &mut [MProc; NR_MPROCS], slot: usize) {
    if let Some(e) = t.get_mut(slot) {
        *e = MProc::EMPTY;
    }
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

// Fork / exit / wait wrappers over the global table. Each is the only `unsafe`
// touching the static; the logic lives in the host-tested `*_in` helpers above.
// SAFETY (all): single-mutator invariant (module note); PM's straight-line loop
// never holds another live reference into the table across these calls.

/// True if `slot` holds a live process.
pub fn in_use(slot: usize) -> bool {
    in_use_in(unsafe { &*TABLE.0.get() }, slot)
}

/// The parent slot of `slot`, or `None` if not in use.
pub fn parent_of(slot: usize) -> Option<usize> {
    parent_of_in(unsafe { &*TABLE.0.get() }, slot)
}

/// The stored endpoint of `slot`, or `None` if not in use.
pub fn endpoint_of(slot: usize) -> Option<Endpoint> {
    endpoint_of_in(unsafe { &*TABLE.0.get() }, slot)
}

/// Allocate a free fork-pool slot, or `None` if the table is full.
pub fn alloc_slot() -> Option<usize> {
    alloc_slot_in(unsafe { &*TABLE.0.get() })
}

/// Populate a forked child at `slot`; returns its assigned pid.
pub fn set_child(slot: usize, parent_slot: usize, endpoint: Endpoint) -> i32 {
    set_child_in(unsafe { &mut *TABLE.0.get() }, slot, parent_slot, endpoint)
}

/// Mark `slot` a zombie carrying `status`.
pub fn set_zombie(slot: usize, status: i32) {
    set_zombie_in(unsafe { &mut *TABLE.0.get() }, slot, status)
}

/// Set or clear `slot`'s waiting flag.
pub fn set_waiting(slot: usize, waiting: bool) {
    set_waiting_in(unsafe { &mut *TABLE.0.get() }, slot, waiting)
}

/// True if `slot` is a parent blocked in `wait()`.
pub fn is_waiting(slot: usize) -> bool {
    is_waiting_in(unsafe { &*TABLE.0.get() }, slot)
}

/// First zombie child of `parent_slot`, as `(slot, pid, exit_status)`.
pub fn find_zombie_child(parent_slot: usize) -> Option<(usize, i32, i32)> {
    find_zombie_child_in(unsafe { &*TABLE.0.get() }, parent_slot)
}

/// True if `parent_slot` has at least one still-live child.
pub fn has_live_child(parent_slot: usize) -> bool {
    has_live_child_in(unsafe { &*TABLE.0.get() }, parent_slot)
}

/// Release `slot` back to the free pool.
pub fn cleanup(slot: usize) {
    cleanup_in(unsafe { &mut *TABLE.0.get() }, slot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::com::{
        SCHED_PROC_NR, STUB_A_PROC_NR, STUB_D_PROC_NR, VFS_PROC_NR, VM_PROC_NR,
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
        assert_eq!(getpid_in(&t, STUB_D_PROC_NR.get() as usize), Some((14, 1)));
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

    // --- Fork / exit / wait helpers (slice 4.6b) ---------------------------

    #[test]
    fn seed_stores_boot_endpoints() {
        let t = seeded();
        // Seeded procs are generation 0, so endpoint == boot_endpoint(slot).
        let e = STUB_D_PROC_NR.get() as usize;
        assert_eq!(t[e].endpoint, boot_endpoint(ProcNr::new(e as i32)));
        assert_eq!(endpoint_of_in(&t, e), Some(t[e].endpoint));
    }

    #[test]
    fn alloc_slot_returns_first_free_fork_pool_slot() {
        let mut t = seeded();
        // Seeded slots stop at FORK_POOL_BASE (15); the first free slot is it.
        assert_eq!(alloc_slot_in(&t), Some(FORK_POOL_BASE));
        // Occupy it, and the next free slot moves up by one.
        set_child_in(
            &mut t,
            FORK_POOL_BASE,
            STUB_D_PROC_NR.get() as usize,
            0x1234,
        );
        assert_eq!(alloc_slot_in(&t), Some(FORK_POOL_BASE + 1));
    }

    #[test]
    fn alloc_slot_none_when_pool_full() {
        let mut t = seeded();
        for slot in FORK_POOL_BASE..NR_MPROCS {
            set_child_in(&mut t, slot, 0, 0x1000 + slot as i32);
        }
        assert_eq!(alloc_slot_in(&t), None);
    }

    #[test]
    fn set_child_assigns_monotonic_pid_and_records_fields() {
        let mut t = seeded();
        let parent = STUB_D_PROC_NR.get() as usize; // pid 14 — a childless seeded proc
        let slot = alloc_slot_in(&t).unwrap();
        let pid = set_child_in(&mut t, slot, parent, 0xABCD);
        // Highest seeded pid is 14 (stub D) → child gets 15.
        assert_eq!(pid, 15);
        assert!(in_use_in(&t, slot));
        assert_eq!(parent_of_in(&t, slot), Some(parent));
        assert_eq!(endpoint_of_in(&t, slot), Some(0xABCD));
        // A live child is getpid-visible: (child pid, parent pid).
        assert_eq!(getpid_in(&t, slot), Some((15, 14)));
    }

    #[test]
    fn zombie_child_is_found_and_reaped() {
        let mut t = seeded();
        let parent = STUB_D_PROC_NR.get() as usize; // childless seeded proc
        let slot = alloc_slot_in(&t).unwrap();
        let pid = set_child_in(&mut t, slot, parent, 0xABCD);

        // Before exit: a live child, no zombie.
        assert!(has_live_child_in(&t, parent));
        assert_eq!(find_zombie_child_in(&t, parent), None);

        set_zombie_in(&mut t, slot, 0x0500);
        // Now a zombie, no longer counted as a live child.
        assert!(!has_live_child_in(&t, parent));
        assert_eq!(find_zombie_child_in(&t, parent), Some((slot, pid, 0x0500)));

        // Reap frees the slot.
        cleanup_in(&mut t, slot);
        assert_eq!(find_zombie_child_in(&t, parent), None);
        assert!(!in_use_in(&t, slot));
        assert_eq!(alloc_slot_in(&t), Some(slot)); // slot is reusable again
    }

    #[test]
    fn waiting_flag_round_trips() {
        let mut t = seeded();
        let parent = STUB_D_PROC_NR.get() as usize;
        assert!(!is_waiting_in(&t, parent));
        set_waiting_in(&mut t, parent, true);
        assert!(is_waiting_in(&t, parent));
        set_waiting_in(&mut t, parent, false);
        assert!(!is_waiting_in(&t, parent));
    }

    #[test]
    fn has_live_child_is_false_without_children() {
        let t = seeded();
        // Stub A has no children (parents are INIT-parented, none point at A).
        assert!(!has_live_child_in(&t, STUB_A_PROC_NR.get() as usize));
    }

    #[test]
    fn out_of_range_slots_are_inert() {
        let mut t = seeded();
        assert!(!in_use_in(&t, NR_MPROCS));
        assert_eq!(parent_of_in(&t, NR_MPROCS), None);
        assert_eq!(endpoint_of_in(&t, NR_MPROCS), None);
        // Mutators on an out-of-range slot are no-ops, not panics.
        set_zombie_in(&mut t, NR_MPROCS, 0);
        set_waiting_in(&mut t, NR_MPROCS, true);
        cleanup_in(&mut t, NR_MPROCS);
    }
}
