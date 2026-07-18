// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Static process and privilege tables, plus the boot-image table that
//! drives initial population.
//!
//! Storage uses `UnsafeCell<[T; N]>` wrapped in a `Sync` newtype: slice 2.2
//! runs on one CPU with interrupts masked, so a `Mutex` would buy nothing
//! over `unsafe` accessors that document the single-threaded invariant.
//! Slice 2.4 (timer + preemption) will revisit this — at that point we'll
//! need a per-table lock or a percpu pattern, not before.

use core::cell::UnsafeCell;

use minixrs_kernel_shared::callnr::NR_KERN_CALLS_PHASE4;
use minixrs_kernel_shared::com::{
    ASYNCM, CLOCK, DS_PROC_NR, HARDWARE, IDLE, INIT_PROC_NR, MEM_PROC_NR, MFS_PROC_NR,
    NR_BOOT_PROCS, NR_PROCS, NR_SYS_PROCS, NR_TASKS, PFS_PROC_NR, PM_PROC_NR, RS_PROC_NR,
    SCHED_PROC_NR, SYSTEM, TTY_PROC_NR, VFS_PROC_NR, VM_PROC_NR, boot_endpoint,
};
use minixrs_kernel_shared::endpoint::{Endpoint, NONE, endpoint_proc};
use minixrs_kernel_shared::error::{EBADSRCDST, EDEADSRCDST};
use minixrs_kernel_shared::{PrivId, ProcNr};

use super::bitmap::set_sys_bit;
use super::flags::{
    BILLABLE, CSK_T, PREEMPTIBLE, ROOT_SYS_PROC, RTS_NO_PRIV, RTS_SLOT_FREE, SRV_T, SYS_PROC,
    TSK_T, USR_T, VM_SYS_PROC,
};
use super::priv_struct::{IPC_MAP_CHUNKS, K_CALL_MASK_CHUNKS, Priv};
use super::proc_struct::{PROC_NAME_LEN, Proc};

/// Total slots in the process table: kernel tasks plus user-process room.
pub const N_PROC_SLOTS: usize = NR_TASKS + NR_PROCS;

// ---------------------------------------------------------------------------
// Scheduling constants (slice 2.4 expands these into full bands).
// ---------------------------------------------------------------------------

/// Highest scheduler priority — reserved for kernel tasks.
pub const TASK_Q: u8 = 0;
/// Default priority for the root system process (RS).
pub const RS_Q: u8 = 4;
/// Default priority for the VM server.
pub const VM_Q: u8 = 4;
/// Default priority for ordinary boot servers.
pub const SRV_Q: u8 = 8;
/// Default priority for `init`.
pub const INIT_Q: u8 = 8;
/// Lowest priority — the idle task only.
pub const IDLE_Q: u8 = 15;

/// Default scheduling quantum for user-class servers (ms).
pub const SRV_QUANTUM_MS: u32 = 200;

// ---------------------------------------------------------------------------
// Static storage.
// ---------------------------------------------------------------------------

#[repr(transparent)]
struct ProcStorage(UnsafeCell<[Proc; N_PROC_SLOTS]>);
// SAFETY: slice 2.2 runs single-threaded with interrupts masked; concurrent
// access to PROC_TABLE is impossible. Future slices that enable preemption
// must wrap this in a proper lock before keeping this `unsafe impl`.
unsafe impl Sync for ProcStorage {}

#[repr(transparent)]
struct PrivStorage(UnsafeCell<[Priv; NR_SYS_PROCS]>);
// SAFETY: same single-threaded invariant as ProcStorage.
unsafe impl Sync for PrivStorage {}

static PROC_TABLE: ProcStorage =
    ProcStorage(UnsafeCell::new([const { Proc::EMPTY }; N_PROC_SLOTS]));
static PRIV_TABLE: PrivStorage =
    PrivStorage(UnsafeCell::new([const { Priv::EMPTY }; NR_SYS_PROCS]));

/// Map a [`ProcNr`] to its index in the process table.
///
/// Kernel tasks (negative `nr`) land in slots `[0, NR_TASKS)`; user processes
/// land in `[NR_TASKS, N_PROC_SLOTS)`. Returns `None` if `nr` is outside the
/// allocated range.
pub const fn proc_index(nr: ProcNr) -> Option<usize> {
    let n = nr.get();
    let shifted = n + NR_TASKS as i32;
    if shifted < 0 || (shifted as usize) >= N_PROC_SLOTS {
        None
    } else {
        Some(shifted as usize)
    }
}

/// Resolve a user-supplied endpoint to its process-table index, verifying the
/// slot is allocated and its stored endpoint — generation included — matches.
/// MINIX 3 `kernel/system.c` `isokendpt`/`okendpt`.
///
/// Once slots recycle (slice 4.6: `SYS_EXIT` bumps the generation on free,
/// `SYS_FORK` reuses the slot), a bare `proc_index(endpoint_proc(e))` would
/// silently resolve a stale endpoint to the slot's *new* occupant. Every path
/// that translates an endpoint taken from a message or trap register must go
/// through here; kernel-originated deliveries that hold live `ProcNr`s
/// (`mini_pf_send`, `deliver_alarm`, …) don't.
///
/// Errors: `EBADSRCDST` for an out-of-range proc field, `EDEADSRCDST` for a
/// freed slot or generation mismatch.
pub fn okendpt(proc_table: &[Proc; N_PROC_SLOTS], e: Endpoint) -> Result<usize, i32> {
    let Some(idx) = proc_index(endpoint_proc(e)) else {
        return Err(EBADSRCDST);
    };
    let p = &proc_table[idx];
    if p.rts_flags.load(core::sync::atomic::Ordering::Relaxed) & RTS_SLOT_FREE != 0
        || p.endpoint != e
    {
        return Err(EDEADSRCDST);
    }
    Ok(idx)
}

/// Map a [`PrivId`] to its index in the privilege table.
pub const fn priv_index(id: PrivId) -> Option<usize> {
    let n = id.as_usize();
    if n >= NR_SYS_PROCS { None } else { Some(n) }
}

// ---------------------------------------------------------------------------
// Boot image.
// ---------------------------------------------------------------------------

struct BootEntry {
    nr: ProcNr,
    name: &'static [u8],
    priv_flags: u16,
    trap_mask: u16,
    priority: u8,
    quantum_ms: u32,
    runnable: bool,
}

const N_IMAGE: usize = NR_TASKS + NR_BOOT_PROCS;
const _: () = assert!(N_IMAGE == 16);

static IMAGE: [BootEntry; N_IMAGE] = [
    // --- Kernel tasks (always runnable) -----------------------------------
    BootEntry {
        nr: ASYNCM,
        name: b"asyncm",
        priv_flags: SYS_PROC,
        trap_mask: TSK_T,
        priority: TASK_Q,
        quantum_ms: 0,
        runnable: true,
    },
    BootEntry {
        nr: IDLE,
        name: b"idle",
        priv_flags: SYS_PROC | BILLABLE,
        trap_mask: TSK_T,
        priority: IDLE_Q,
        quantum_ms: 0,
        runnable: true,
    },
    BootEntry {
        nr: CLOCK,
        name: b"clock",
        priv_flags: SYS_PROC,
        trap_mask: CSK_T,
        priority: TASK_Q,
        quantum_ms: 0,
        runnable: true,
    },
    BootEntry {
        nr: SYSTEM,
        name: b"system",
        priv_flags: SYS_PROC,
        trap_mask: CSK_T,
        priority: TASK_Q,
        quantum_ms: 0,
        runnable: true,
    },
    BootEntry {
        nr: HARDWARE,
        name: b"hardware",
        priv_flags: SYS_PROC,
        trap_mask: TSK_T,
        priority: TASK_Q,
        quantum_ms: 0,
        runnable: true,
    },
    // --- Boot servers (blocked on RTS_NO_PRIV until slice 2.6 loads ELFs) -
    BootEntry {
        nr: PM_PROC_NR,
        name: b"pm",
        priv_flags: SYS_PROC | PREEMPTIBLE,
        trap_mask: SRV_T,
        priority: SRV_Q,
        quantum_ms: SRV_QUANTUM_MS,
        runnable: false,
    },
    BootEntry {
        nr: VFS_PROC_NR,
        name: b"vfs",
        priv_flags: SYS_PROC | PREEMPTIBLE,
        trap_mask: SRV_T,
        priority: SRV_Q,
        quantum_ms: SRV_QUANTUM_MS,
        runnable: false,
    },
    BootEntry {
        nr: RS_PROC_NR,
        name: b"rs",
        priv_flags: SYS_PROC | PREEMPTIBLE | ROOT_SYS_PROC,
        trap_mask: SRV_T,
        priority: RS_Q,
        quantum_ms: SRV_QUANTUM_MS,
        runnable: false,
    },
    BootEntry {
        nr: MEM_PROC_NR,
        name: b"memory",
        priv_flags: SYS_PROC | PREEMPTIBLE,
        trap_mask: SRV_T,
        priority: SRV_Q,
        quantum_ms: SRV_QUANTUM_MS,
        runnable: false,
    },
    BootEntry {
        nr: TTY_PROC_NR,
        name: b"tty",
        priv_flags: SYS_PROC | PREEMPTIBLE,
        trap_mask: SRV_T,
        priority: SRV_Q,
        quantum_ms: SRV_QUANTUM_MS,
        runnable: false,
    },
    BootEntry {
        nr: DS_PROC_NR,
        name: b"ds",
        priv_flags: SYS_PROC | PREEMPTIBLE,
        trap_mask: SRV_T,
        priority: SRV_Q,
        quantum_ms: SRV_QUANTUM_MS,
        runnable: false,
    },
    BootEntry {
        nr: MFS_PROC_NR,
        name: b"mfs",
        priv_flags: SYS_PROC | PREEMPTIBLE,
        trap_mask: SRV_T,
        priority: SRV_Q,
        quantum_ms: SRV_QUANTUM_MS,
        runnable: false,
    },
    BootEntry {
        nr: VM_PROC_NR,
        name: b"vm",
        priv_flags: SYS_PROC | PREEMPTIBLE | VM_SYS_PROC,
        trap_mask: SRV_T,
        priority: VM_Q,
        quantum_ms: SRV_QUANTUM_MS,
        runnable: false,
    },
    BootEntry {
        nr: PFS_PROC_NR,
        name: b"pfs",
        priv_flags: SYS_PROC | PREEMPTIBLE,
        trap_mask: SRV_T,
        priority: SRV_Q,
        quantum_ms: SRV_QUANTUM_MS,
        runnable: false,
    },
    BootEntry {
        nr: SCHED_PROC_NR,
        name: b"sched",
        priv_flags: SYS_PROC | PREEMPTIBLE,
        trap_mask: SRV_T,
        priority: SRV_Q,
        quantum_ms: SRV_QUANTUM_MS,
        runnable: false,
    },
    // init (PID 1) is an ordinary user process, not a system server (slice 4.8):
    // `USR_T` traps (SENDREC only), and `init_boot_image` points its proc slot at
    // the shared `USER_PRIV_ID` (ipc_to = {PM}, empty kernel-call mask) rather than
    // populating a dedicated server-grade priv slot. `priv_flags` still carries
    // `SYS_PROC` so the whole `IMAGE` range keeps uniform boot handling; the priv
    // grade is what the user-grade slot enforces.
    BootEntry {
        nr: INIT_PROC_NR,
        name: b"init",
        priv_flags: SYS_PROC | PREEMPTIBLE,
        trap_mask: USR_T,
        priority: INIT_Q,
        quantum_ms: SRV_QUANTUM_MS,
        runnable: false,
    },
];

// ---------------------------------------------------------------------------
// Access helpers. All return raw pointers — callers carry the single-threaded
// invariant.
// ---------------------------------------------------------------------------

/// SAFETY: caller must ensure no other reference into `PROC_TABLE` exists.
pub(crate) unsafe fn proc_slot_mut(nr: ProcNr) -> Option<&'static mut Proc> {
    let idx = proc_index(nr)?;
    // SAFETY: `idx` is in-range; single-threaded boot context.
    let arr = unsafe { &mut *PROC_TABLE.0.get() };
    Some(&mut arr[idx])
}

/// SAFETY: caller must ensure no exclusive reference into `PROC_TABLE` exists.
pub(crate) unsafe fn proc_table_ref() -> &'static [Proc; N_PROC_SLOTS] {
    // SAFETY: single-threaded boot context.
    unsafe { &*PROC_TABLE.0.get() }
}

/// Borrow `PROC_TABLE` mutably as a slice. Used by `ipc::do_ipc` to hand a
/// single `&mut [Proc]` down through every IPC primitive — avoiding the
/// two-`&mut`-from-one-`UnsafeCell` hazard that arises if each primitive
/// re-borrows individual slots via `proc_slot_mut`.
///
/// SAFETY: caller must hold the single-threaded-boot / IRQ-masked
/// invariant and must not hold any other reference into `PROC_TABLE`
/// while the returned borrow is live.
pub(crate) unsafe fn proc_table_mut_slice() -> &'static mut [Proc; N_PROC_SLOTS] {
    // SAFETY: forwarded — caller's invariants.
    unsafe { &mut *PROC_TABLE.0.get() }
}

/// SAFETY: caller must ensure no other reference into `PRIV_TABLE` exists.
pub(crate) unsafe fn priv_slot_mut(id: PrivId) -> Option<&'static mut Priv> {
    let idx = priv_index(id)?;
    // SAFETY: `idx` is in-range; single-threaded boot context.
    let arr = unsafe { &mut *PRIV_TABLE.0.get() };
    Some(&mut arr[idx])
}

/// SAFETY: caller must ensure no exclusive reference into `PRIV_TABLE` exists.
pub(crate) unsafe fn priv_table_ref() -> &'static [Priv; NR_SYS_PROCS] {
    // SAFETY: single-threaded boot context.
    unsafe { &*PRIV_TABLE.0.get() }
}

/// Borrow `PRIV_TABLE` mutably as a slice. Companion of
/// [`proc_table_mut_slice`] — `ipc::do_ipc` materializes both at once and
/// passes them down to the per-primitive handlers.
///
/// SAFETY: caller must hold the single-threaded-boot / IRQ-masked
/// invariant and must not hold any other reference into `PRIV_TABLE`
/// while the returned borrow is live.
pub(crate) unsafe fn priv_table_mut_slice() -> &'static mut [Priv; NR_SYS_PROCS] {
    // SAFETY: forwarded — caller's invariants.
    unsafe { &mut *PRIV_TABLE.0.get() }
}

// ---------------------------------------------------------------------------
// Boot-time population.
// ---------------------------------------------------------------------------

/// Populate `PROC_TABLE` and `PRIV_TABLE` from `IMAGE`.
///
/// Called exactly once from `kmain` during boot. Slice 2.4's scheduler will
/// assume one-shot init; double-calling would clobber any RTS bits the
/// scheduler has set since.
pub fn init() {
    init_empty_slots();
    init_boot_image();
    populate_user_priv();
}

/// Shared privilege slot for ordinary user processes (slice 4.5) — MINIX 3's
/// single user priv (`USER_PRIV_ID`), minus the dynamic id allocation. The
/// boot image occupies priv slots `[0, 16)` and the demo stubs are
/// kernel-installed at 16..=19 (`arch::aarch64::userland`); 20 is the next
/// free slot. `SYS_PRIVCTL(PRIVCTL_SET_USER)` points a frozen target here,
/// and the 4.6 fork path hands every forked child this same slot so fork
/// can't exhaust the 64-slot `PRIV_TABLE`.
pub(crate) const USER_PRIV_ID: PrivId = PrivId::new(20);

const _: () = assert!((USER_PRIV_ID.get() as usize) < NR_SYS_PROCS);

/// Populate the shared USER priv slot (slice 4.5).
///
/// `USR_T` traps (SENDREC only), `ipc_to` open to PM alone, and an *empty*
/// kernel-call mask — ordinary user processes make no kernel calls (MINIX 3
/// `table.c` gives the user template a `{0}` call mask). `sig_mgr` is PM:
/// MINIX makes PM the signal manager for user processes, while the boot
/// slots keep RS (the system-process manager) from `populate_priv`.
///
/// Also opens the reverse PM → USER `ipc_to` bit so PM can reply to a user
/// proc's SENDREC — `init_boot_image` fills SRV_T bitmaps only for the active
/// boot slots `[0, n_active)`, the same gap `install_stub_d_priv` closes for
/// VM → D (as a second, sequential borrow).
fn populate_user_priv() {
    let pm_priv_id = {
        // SAFETY: read-only snapshot; no live `&mut Proc` here.
        let table = unsafe { proc_table_ref() };
        let idx = proc_index(PM_PROC_NR).expect("PM in proc table");
        table[idx].priv_id.expect("PM priv populated by proc::init")
    };

    {
        // SAFETY: priv index < NR_SYS_PROCS (const-asserted); single-threaded
        // boot context; no overlapping reference into PRIV_TABLE held.
        let pr = unsafe { priv_slot_mut(USER_PRIV_ID) }.expect("USER priv slot in range");
        pr.id = USER_PRIV_ID;
        // Shared among every USER-priv proc — no single owning proc.
        pr.proc_nr = None;
        pr.flags = PREEMPTIBLE | BILLABLE;
        pr.trap_mask = USR_T;
        pr.ipc_to.fill(0);
        set_sys_bit(&mut pr.ipc_to, pm_priv_id);
        pr.k_call_mask.fill(0);
        pr.notify_pending.fill(0);
        pr.asyn_pending.fill(0);
        pr.sig_mgr = boot_endpoint(PM_PROC_NR);
    }

    // Open PM → USER. Separate borrow: the USER slot's `&mut Priv` above has
    // been dropped.
    {
        // SAFETY: priv index in-range; no overlapping reference held.
        let pm_pr = unsafe { priv_slot_mut(pm_priv_id) }.expect("PM priv slot in range");
        set_sys_bit(&mut pm_pr.ipc_to, USER_PRIV_ID);
    }
}

fn init_empty_slots() {
    // Give every slot a valid (nr, endpoint) so that index-based traversal
    // never sees a stale zero `nr` after `Proc::EMPTY` initialization.
    for i in 0..N_PROC_SLOTS {
        let nr = ProcNr::new(i as i32 - NR_TASKS as i32);
        // SAFETY: index in-range; single-threaded boot context; no other
        // references into PROC_TABLE exist at this point in `init`.
        let p = unsafe { proc_slot_mut(nr) }.expect("proc index in range");
        p.nr = nr;
        p.endpoint = boot_endpoint(nr);
        // EMPTY left rts_flags = RTS_SLOT_FREE — leave it that way.
    }

    for i in 0..NR_SYS_PROCS {
        // SAFETY: index in-range; single-threaded boot context.
        let pr = unsafe { priv_slot_mut(PrivId::new(i as u16)) }.expect("priv index in range");
        pr.id = PrivId::new(i as u16);
    }
}

fn init_boot_image() {
    // Active priv-slot count after population — the IPC bitmap for SRV_T
    // entries needs to enumerate every active slot.
    let n_active = N_IMAGE as u16;

    for (slot, entry) in IMAGE.iter().enumerate() {
        if entry.nr == INIT_PROC_NR {
            // init is PID 1 — an ordinary user process. Point its proc slot at the
            // shared USER priv (`USER_PRIV_ID`: `USR_T`, ipc_to = {PM}, empty
            // k_call_mask, sig_mgr = PM), the same slot every forked child uses.
            // `populate_user_priv()` (called right after this loop) fills that slot
            // and opens the PM → USER reverse edge, so no dedicated server-grade
            // priv slot is populated for init. Its would-be slot stays free.
            populate_proc(USER_PRIV_ID, entry);
            continue;
        }
        let priv_id = PrivId::new(slot as u16);
        populate_priv(priv_id, entry, n_active);
        populate_proc(priv_id, entry);
    }
}

fn populate_priv(id: PrivId, entry: &BootEntry, n_active: u16) {
    // SAFETY: priv index < NR_SYS_PROCS; single-threaded boot context; no
    // overlapping reference into PRIV_TABLE held across the call.
    let pr = unsafe { priv_slot_mut(id) }.expect("priv slot in range");
    pr.id = id;
    pr.proc_nr = Some(entry.nr);
    pr.flags = entry.priv_flags;
    pr.trap_mask = entry.trap_mask;

    if entry.trap_mask == SRV_T {
        // SRV_T privs can send to every active slot. Set bits [0, n_active).
        fill_bits(&mut pr.ipc_to, n_active as usize);
        // SRV_T privs can issue every kernel call defined so far. This bound
        // tracks the highest `SYS_*` number; slice 4.3 widened it to admit
        // `SYS_SCHEDULE` / `SYS_SCHEDCTL` so SCHED may issue them.
        fill_bits(&mut pr.k_call_mask, NR_KERN_CALLS_PHASE4);
    }
    // Kernel-task slots leave ipc_to and k_call_mask zeroed.

    pr.sig_mgr = boot_endpoint(RS_PROC_NR);
    pr.notify_pending.fill(0);
    pr.asyn_pending.fill(0);
}

fn populate_proc(priv_id: PrivId, entry: &BootEntry) {
    // SAFETY: proc index in-range; single-threaded boot context; no
    // overlapping reference into PROC_TABLE held across the call.
    let p = unsafe { proc_slot_mut(entry.nr) }.expect("proc slot in range");
    p.priv_id = Some(priv_id);
    p.priority = entry.priority;
    p.quantum_ms = entry.quantum_ms;
    p.quantum_left = entry.quantum_ms as u64;
    p.endpoint = boot_endpoint(entry.nr);
    p.nr = entry.nr;
    // Boot procs start kernel-scheduled; a user-space scheduler claims them
    // later via `SYS_SCHEDCTL` (slice 4.3). `Proc::EMPTY` already zeroes this to
    // `NONE`, but set it explicitly so a future slot-reuse path can't inherit a
    // stale scheduler endpoint.
    p.scheduler = NONE;

    // Copy name into the fixed-width field (truncates silently at PROC_NAME_LEN).
    let n = core::cmp::min(entry.name.len(), PROC_NAME_LEN - 1);
    p.name[..n].copy_from_slice(&entry.name[..n]);
    p.name[n..].fill(0);

    let rts = if entry.runnable { 0 } else { RTS_NO_PRIV };
    p.rts_flags
        .store(rts, core::sync::atomic::Ordering::Relaxed);
}

/// Set the lowest `n` bits in a `u32` bitmap.
fn fill_bits(map: &mut [u32], n: usize) {
    for i in 0..n {
        let word = i / 32;
        let bit = i % 32;
        if word < map.len() {
            map[word] |= 1 << bit;
        }
    }
}

// Compile-time sanity that the bitmap sizing came out as expected.
const _: () = assert!(IPC_MAP_CHUNKS == 2);
const _: () = assert!(K_CALL_MASK_CHUNKS == 1);

/// Number of slots in `PROC_TABLE` (for the dump helper).
pub const fn n_proc_slots() -> usize {
    N_PROC_SLOTS
}

/// Number of slots in `IMAGE` (for the dump helper).
pub const fn n_image_slots() -> usize {
    N_IMAGE
}
