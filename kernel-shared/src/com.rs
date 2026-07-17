// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Process counts and well-known endpoints.
//!
//! Kernel tasks (`ASYNCM`, `IDLE`, `CLOCK`, `SYSTEM`, `HARDWARE`) live at
//! negative process numbers; system servers and other user-space processes
//! live at non-negative process numbers in the order they boot. The set of
//! kernel tasks follows MINIX 3 `include/minix/com.h`; the user-space proc
//! numbering does not — see the boot-image section below.

use crate::endpoint::{ENDPOINT_SLOT_TOP, Endpoint, ProcNr, make_endpoint};

/// Number of kernel tasks (slots with negative proc-numbers).
pub const NR_TASKS: usize = 5;
/// Number of user-process slots in the process table.
pub const NR_PROCS: usize = 1024;
/// Number of slots in the privilege table — system (privileged) processes.
pub const NR_SYS_PROCS: usize = 64;

// NR_PROCS must not grow into the sentinel range (ANY/NONE/SELF live at
// ENDPOINT_SLOT_TOP, -1, -2). Raising NR_PROCS past this bound would alias
// a real endpoint with a sentinel.
const _: () = assert!(NR_PROCS < (ENDPOINT_SLOT_TOP.get() as usize) - 2);

// ---------------------------------------------------------------------------
// Kernel tasks — negative proc-numbers.
// ---------------------------------------------------------------------------

/// Asynchronous-message driver (kernel pseudo-task).
pub const ASYNCM: ProcNr = ProcNr::new(-5);
/// Idle task (runs when nothing else is runnable).
pub const IDLE: ProcNr = ProcNr::new(-4);
/// Clock task (kernel pseudo-task for timer-driven work).
pub const CLOCK: ProcNr = ProcNr::new(-3);
/// System task (handles `SYS_*` kernel calls).
pub const SYSTEM: ProcNr = ProcNr::new(-2);
/// Hardware-interrupt pseudo-task (source endpoint for IRQ notifications).
pub const HARDWARE: ProcNr = ProcNr::new(-1);

// ---------------------------------------------------------------------------
// Well-known user-space servers — generation 0 at boot.
//
// minix.rs boot image: renumbered contiguously from 0. Differs from MINIX
// 3, which scatters slots (no slot 7) and places SCHED at 4. LOG is not
// statically slotted in minix.rs — RS spawns it on demand.
// ---------------------------------------------------------------------------

pub const PM_PROC_NR: ProcNr = ProcNr::new(0);
pub const VFS_PROC_NR: ProcNr = ProcNr::new(1);
pub const RS_PROC_NR: ProcNr = ProcNr::new(2);
pub const MEM_PROC_NR: ProcNr = ProcNr::new(3);
pub const TTY_PROC_NR: ProcNr = ProcNr::new(4);
pub const DS_PROC_NR: ProcNr = ProcNr::new(5);
pub const MFS_PROC_NR: ProcNr = ProcNr::new(6);
pub const VM_PROC_NR: ProcNr = ProcNr::new(7);
pub const PFS_PROC_NR: ProcNr = ProcNr::new(8);
pub const SCHED_PROC_NR: ProcNr = ProcNr::new(9);
pub const INIT_PROC_NR: ProcNr = ProcNr::new(10);

/// One past the highest boot-server proc-number (`init` is the last slot
/// allocated from the boot image).
pub const NR_BOOT_PROCS: usize = (INIT_PROC_NR.get() as usize) + 1;

// Compile-time guarantees that the proc tables can hold every boot process.
const _: () = assert!(NR_PROCS >= NR_BOOT_PROCS);
const _: () = assert!(NR_SYS_PROCS >= NR_BOOT_PROCS);

// ---------------------------------------------------------------------------
// Phase-4 demo stubs — hand-installed EL0 programs just past the boot image.
//
// Slices 2.x-4.x install five tiny assembly stubs (kernel
// `arch::aarch64::userland`) as live exercises for IPC, kernel calls, VM, and
// the PM signal path. PM's mproc table (slice 4.5) seeds them as user
// processes, so their proc numbers are shared here rather than kept
// kernel-private. The whole range is retired in slice 4.8 when init + real
// processes take over as the live exercise.
// ---------------------------------------------------------------------------

/// Stub A — SENDREC ping loop to stub B.
pub const STUB_A_PROC_NR: ProcNr = ProcNr::new(11);
/// Stub B — RECEIVE/SEND echo peer of stub A.
pub const STUB_B_PROC_NR: ProcNr = ProcNr::new(12);
/// Stub C — `SYS_GETINFO(GET_WHOAMI)` kernel-call loop (SCHED-delegated).
pub const STUB_C_PROC_NR: ProcNr = ProcNr::new(13);
/// Stub D — brk/mmap/munmap VM client; deliberately faults out-of-region
/// after its munmap (slice 4.5) to exercise the SIGSEGV → PM kill path.
pub const STUB_D_PROC_NR: ProcNr = ProcNr::new(14);
/// Stub E — built frozen (`RTS_NO_PRIV`, no priv slot); PM unfreezes it via
/// `SYS_PRIVCTL(PRIVCTL_SET_USER)` and it then loops `SENDREC PM_GETPID`.
pub const STUB_E_PROC_NR: ProcNr = ProcNr::new(15);

/// Number of demo stubs.
pub const NR_STUB_PROCS: usize = 5;

// Stubs sit contiguously just past the boot image, well inside the proc table.
const _: () = assert!(STUB_A_PROC_NR.get() as usize == NR_BOOT_PROCS);
const _: () = assert!(STUB_E_PROC_NR.get() as usize == NR_BOOT_PROCS + NR_STUB_PROCS - 1);
const _: () = assert!((STUB_E_PROC_NR.get() as usize) < NR_PROCS);

/// Build a boot-time endpoint (generation 0) for a task or server.
pub const fn boot_endpoint(p: ProcNr) -> Endpoint {
    make_endpoint(0, p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::{endpoint_gen, endpoint_proc};

    #[test]
    fn task_endpoints_have_negative_procs() {
        for p in [ASYNCM, IDLE, CLOCK, SYSTEM, HARDWARE] {
            assert!(p.is_task(), "task proc {p} should be negative");
        }
    }

    #[test]
    fn server_endpoints_distinct_and_nonnegative() {
        let servers = [
            PM_PROC_NR,
            VFS_PROC_NR,
            RS_PROC_NR,
            MEM_PROC_NR,
            TTY_PROC_NR,
            DS_PROC_NR,
            MFS_PROC_NR,
            VM_PROC_NR,
            PFS_PROC_NR,
            SCHED_PROC_NR,
            INIT_PROC_NR,
        ];
        for &p in &servers {
            assert!(p.get() >= 0, "server proc {p} should be non-negative");
        }
        let mut seen = [false; NR_BOOT_PROCS];
        for &p in &servers {
            let i = p.get() as usize;
            assert!(!seen[i], "duplicate proc_nr {p}");
            seen[i] = true;
        }
    }

    #[test]
    fn boot_endpoint_roundtrips_for_tasks_and_servers() {
        for p in [SYSTEM, IDLE, CLOCK, PM_PROC_NR, VFS_PROC_NR, INIT_PROC_NR] {
            let e = boot_endpoint(p);
            assert_eq!(endpoint_gen(e), 0);
            assert_eq!(endpoint_proc(e), p);
        }
    }

    #[test]
    fn stub_procs_contiguous_past_boot_image() {
        // The demo stubs occupy the five slots just past the boot image, in
        // order and without duplicates; PM's mproc seeding depends on this.
        let stubs = [
            STUB_A_PROC_NR,
            STUB_B_PROC_NR,
            STUB_C_PROC_NR,
            STUB_D_PROC_NR,
            STUB_E_PROC_NR,
        ];
        for (i, s) in stubs.iter().enumerate() {
            assert_eq!(s.get() as usize, NR_BOOT_PROCS + i);
        }
        assert_eq!(stubs.len(), NR_STUB_PROCS);
    }

    #[test]
    fn nr_boot_procs_covers_init() {
        // `INIT_PROC_NR.get() as usize` is constant so the comparison is also
        // checked statically via `const _: () = assert!(...)` below; this
        // test keeps the invariant visible at test-discovery time.
        const { assert!(NR_BOOT_PROCS > INIT_PROC_NR.get() as usize) };
    }
}
