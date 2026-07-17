// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Kernel-call numbers — the `m_type` values for `SENDREC`s addressed to the
//! `SYSTEM` task.
//!
//! Numbering convention follows MINIX 3 `include/minix/com.h` (`KERNEL_CALL`
//! base, contiguous offsets). Only the 14 calls needed by Phase 2 are
//! defined; more are added as later phases come online.

/// Base for kernel-call numbers. Matches MINIX 3 `KERNEL_CALL`.
pub const KERNEL_CALL: i32 = 0x600;

#[allow(clippy::identity_op)] // explicit `+ 0` keeps the table aligned visually
pub const SYS_GETINFO: i32 = KERNEL_CALL + 0;
pub const SYS_PRIVCTL: i32 = KERNEL_CALL + 1;
pub const SYS_FORK: i32 = KERNEL_CALL + 2;
pub const SYS_EXEC: i32 = KERNEL_CALL + 3;
pub const SYS_EXIT: i32 = KERNEL_CALL + 4;
pub const SYS_COPY: i32 = KERNEL_CALL + 5;
pub const SYS_SAFECOPY: i32 = KERNEL_CALL + 6;
pub const SYS_IRQCTL: i32 = KERNEL_CALL + 7;
pub const SYS_VMCTL: i32 = KERNEL_CALL + 8;
pub const SYS_SCHEDULE: i32 = KERNEL_CALL + 9;
pub const SYS_SETALARM: i32 = KERNEL_CALL + 10;
pub const SYS_TIMES: i32 = KERNEL_CALL + 11;
pub const SYS_DIAGCTL: i32 = KERNEL_CALL + 12;
pub const SYS_SETGRANT: i32 = KERNEL_CALL + 13;
/// Scheduler claim/release. A user-space scheduler (SCHED) calls this to take a
/// target proc under its management (`target.scheduler = caller`) or hand it
/// back to the kernel scheduler (`SCHEDCTL_FLAG_KERNEL`). Made real in slice 4.3
/// alongside `SYS_SCHEDULE`; payload layout mirrors `SYS_VMCTL` (flags in
/// `0..4`, target endpoint in `4..8`).
pub const SYS_SCHEDCTL: i32 = KERNEL_CALL + 14;
/// Raise a signal on a target proc (slice 4.5). Target endpoint in payload
/// `0..4` (i32), signal number in `4..8` (i32, `1..NSIG`). The kernel records
/// the signal in the target's pending bitmap (`cause_sig`) and notifies PM,
/// which drains via `SYS_GETKSIG` / `SYS_ENDKSIG`. This is the MINIX 3
/// non-PM-caller semantics (queue toward PM); PM's own direct-delivery branch
/// (`send_sig` to a system proc) is deferred until a consumer exists.
pub const SYS_KILL: i32 = KERNEL_CALL + 15;
/// PM → kernel: fetch the next proc with pending kernel signals (slice 4.5).
/// Reply payload: target endpoint in `0..4` (i32; `NONE` when nothing is
/// pending) and the pending-signal bitmap in `4..8` (u32). The kernel hands
/// the bitmap off (clears `Proc::sig_pending`) but leaves the target's
/// signal-pending RTS state set until `SYS_ENDKSIG` acknowledges it.
pub const SYS_GETKSIG: i32 = KERNEL_CALL + 16;
/// PM → kernel: signal processing for the target (payload `0..4`, i32) is
/// complete — clear its signal-pending RTS state (slice 4.5).
pub const SYS_ENDKSIG: i32 = KERNEL_CALL + 17;

/// `SYS_SCHEDCTL` flag: revert the target to kernel scheduling
/// (`target.scheduler = NONE`). Absent → the caller claims the target as its
/// own scheduler. Matches MINIX 3 `SCHEDCTL_FLAG_KERNEL` (`include/minix/com.h`).
pub const SCHEDCTL_FLAG_KERNEL: i32 = 1 << 0;

/// Number of kernel calls defined through Phase 4. Slice 4.3 made
/// `SYS_SCHEDULE` real and added `SYS_SCHEDCTL` (15); slice 4.5 adds the
/// signal trio `SYS_KILL` / `SYS_GETKSIG` / `SYS_ENDKSIG`, bringing the
/// count to 18.
pub const NR_KERN_CALLS_PHASE4: usize = 18;

/// Size of the privilege-table kernel-call mask, in bits. Sized as a single
/// `u32` chunk (32 slots) to leave headroom past Phase 4's 15 calls while
/// keeping the bitmap a single word per privilege slot.
pub const NR_SYS_CALLS: usize = 32;

const _: () = assert!(NR_SYS_CALLS >= NR_KERN_CALLS_PHASE4);
const _: () = assert!(NR_SYS_CALLS.is_multiple_of(32));

// ---------------------------------------------------------------------------
// `SYS_GETINFO` request sub-types.
//
// `SYS_GETINFO` is a multi-purpose introspection call: the request sub-type
// in the first 4 bytes of the message payload selects what the kernel reports
// back. Numbering matches MINIX 3 `include/minix/sysinfo.h` so the same wire
// values can be reused once musl + servers land.
// ---------------------------------------------------------------------------

/// `SYS_GETINFO` request: return the caller's endpoint, priv flags, init
/// flags, and process name. The kernel writes the reply into the payload of
/// the request message in-place; on return `m_type == OK`.
pub const GET_WHOAMI: i32 = 12;

/// Length of the `name` field in the `GET_WHOAMI` reply payload. minix.rs uses
/// the kernel's own `PROC_NAME_LEN` here rather than MINIX 3's 44-byte field —
/// the name is only used for debug/log output and the kernel never stores more
/// than 16 bytes per slot.
pub const SYS_GETINFO_NAME_LEN: usize = 16;

// ---------------------------------------------------------------------------
// `SYS_PRIVCTL` subcodes.
//
// `SYS_PRIVCTL` (real as of slice 4.5) sets up a target proc's privilege
// slot. The target endpoint lives in payload `0..4` and the subcode in `4..8`
// (both i32, the same target-first convention as `SYS_SCHEDULE`). Numbers
// start at 1 so a zeroed payload is an obvious "invalid" (the `VMCTL_*`
// convention). Modeled on MINIX 3 `SYS_PRIV_SET_USER`; the system-proc
// variants (`SET_SYS`, range grants) arrive with RS service starts.
// ---------------------------------------------------------------------------

/// Point a frozen (`RTS_NO_PRIV`) target at the shared USER privilege slot
/// and release it. The USER slot carries `USR_T` traps, `ipc_to` = {PM}, and
/// an empty kernel-call mask — ordinary user processes make no kernel calls.
/// The 4.6 fork path leans on this to hand forked children a privilege.
pub const PRIVCTL_SET_USER: i32 = 1;

// ---------------------------------------------------------------------------
// `SYS_VMCTL` subcalls.
//
// `SYS_VMCTL` mediates all user-space page-table changes: the kernel owns the
// physical frame allocator and every unsafe PTE write, and VM (slice 3.4)
// drives policy by issuing these subcalls. The subcall selector lives in the
// first 4 bytes of the message payload (same convention as `GET_WHOAMI`); the
// target process is named by an endpoint in the next 4 bytes (`SELF` allowed).
// Numbers start at 1 so a zeroed payload (subcall 0) is an obvious "invalid".
// These are minix.rs-specific — MINIX 3's VMCTL subcall set differs because its
// frame allocator lives in VM, not the kernel.
// ---------------------------------------------------------------------------

/// Allocate a fresh zeroed frame and map it at `vaddr` in the target's
/// address space with the requested protection. The allocated PA is returned
/// in the reply payload. (The kernel allocates because the frame allocator is
/// kernel-side; VM supplies only `vaddr` + protection.)
pub const VMCTL_PT_MAP: i32 = 1;
/// Unmap `vaddr` in the target's address space and free the backing frame.
pub const VMCTL_PT_UNMAP: i32 = 2;
/// Clear the target's pending page fault and make it runnable again.
pub const VMCTL_CLEAR_PAGEFAULT: i32 = 3;
/// Read the target's recorded page-fault state (addr/flags/ip) into the reply.
/// Valid only while the target is blocked on a page fault.
pub const VMCTL_GET_PAGEFAULT: i32 = 4;
/// Inhibit scheduling of the target while VM mutates its address space.
pub const VMCTL_VMINHIBIT_SET: i32 = 5;
/// Release a prior `VMCTL_VMINHIBIT_SET`.
pub const VMCTL_VMINHIBIT_CLEAR: i32 = 6;

/// Number of `SYS_VMCTL` subcalls. Locks the dispatch-match coverage in
/// `system::do_vmctl` via a const-assert.
pub const NR_VMCTL_SUBCALLS: usize = 6;

// `VMCTL_PT_MAP` protection bits (message payload, `vaddr`-adjacent word).
/// EL0 may write the mapped page.
pub const VMCTL_PROT_WRITE: i32 = 1 << 0;
/// EL0 may execute from the mapped page.
pub const VMCTL_PROT_EXEC: i32 = 1 << 1;

// ---------------------------------------------------------------------------
// PM (process manager) server request numbers — `m_type` values for messages
// addressed to the PM server (slice 4.5).
//
// Like the VM/DS/SEF/SCHED ranges these are *server IPC requests*, not kernel
// calls. SCHED's `0xF00` block is the last one below the IPC `NOTIFY_MESSAGE`
// marker (`0x1000`), so PM takes the free gap between the kernel-call range
// (`0x600..0x618`) and VM (`0xC00`). Numbering is minix.rs-specific — MINIX 3
// carries PM call numbers in `callnr.h`; those ABI numbers arrive with the
// musl wrappers in Phase 5.
// ---------------------------------------------------------------------------

/// Base for PM server request `m_type` values.
pub const PM_RQ_BASE: i32 = 0x700;

/// Client → PM: return the caller's process id. No request payload — the
/// kernel-stamped `m_source` names the caller. Reply: `m_type` *is* the pid
/// (MINIX convention: the result is the pid, >= 0; errors are negative, e.g.
/// `ESRCH` for a caller unknown to PM's mproc table), with the parent's pid
/// in payload `0..4` (i32) so `getppid` needs no second call.
pub const PM_GETPID: i32 = PM_RQ_BASE;

/// Number of PM server requests defined so far. Locks the PM server's
/// dispatch coverage the way `NR_DS_REQUESTS` locks the DS server.
pub const NR_PM_MSGS: usize = 1;

// The PM range sits strictly above the kernel-call range and strictly below
// VM's (and therefore every other) server request range and the NOTIFY marker.
const _: () = assert!(PM_RQ_BASE > KERNEL_CALL + (NR_KERN_CALLS_PHASE4 as i32 - 1));
const _: () = assert!(PM_RQ_BASE + (NR_PM_MSGS as i32 - 1) < VM_RQ_BASE);
const _: () = assert!(PM_RQ_BASE + (NR_PM_MSGS as i32 - 1) < crate::ipc_const::NOTIFY_MESSAGE);

// ---------------------------------------------------------------------------
// VM server request numbers — `m_type` values for messages addressed to VM.
//
// These are *server IPC requests*, not kernel calls, so they live in their own
// range distinct from `KERNEL_CALL` (`0x600`). The kernel originates
// `VM_PAGEFAULT` on a faulting process's behalf (slice 3.4); later slices add
// `VM_BRK` / `VM_MMAP` / `VM_MUNMAP`. Numbering is minix.rs-specific (MINIX 3's VM request set
// differs because its frame allocator lives in VM, not the kernel).
// ---------------------------------------------------------------------------

/// Base for VM server request `m_type` values.
pub const VM_RQ_BASE: i32 = 0xC00;

/// Kernel → VM: a process page-faulted. `m_source` identifies the faulting
/// process; the payload carries the fault address (`0..8`, u64) and fault
/// flags (`8..12`, u32). VM resolves it via `SYS_VMCTL(VMCTL_PT_MAP)` +
/// `SYS_VMCTL(VMCTL_CLEAR_PAGEFAULT)`.
pub const VM_PAGEFAULT: i32 = VM_RQ_BASE;

/// EL0 → VM: set the caller's program break to `new_break` (payload `0..8`,
/// u64). VM grows or creates the caller's heap region to `[HEAP_BASE,
/// new_break)`; pages fault in lazily on first touch (no eager mapping). The
/// reply carries `m_type = OK` and the resulting break in payload `0..8`, or a
/// negative error in `m_type`. (slice 3.5)
pub const VM_BRK: i32 = VM_RQ_BASE + 1;

/// EL0 → VM: anonymous mmap. The caller requests `len` bytes (payload `0..8`,
/// u64); VM page-aligns the length, picks a free VA from the caller's mmap bump
/// arena, records an `Mmap` region, and replies with the chosen base address in
/// payload `0..8` and `m_type = OK`. Like `mmap(NULL, len, …)`: VM chooses the
/// address. Pages fault in lazily on first touch (no eager mapping). On failure
/// the negative error is in `m_type` (`EINVAL` for a zero or overflowing
/// length, `ENOMEM` when no region slot is free). (slice 3.6)
pub const VM_MMAP: i32 = VM_RQ_BASE + 2;

/// EL0 → VM: unmap a prior mmap. The caller passes the base address (payload
/// `0..8`, u64) and length (payload `8..16`, u64). VM page-aligns the range,
/// drops the matching `Mmap` region, and unmaps each backing page via
/// `SYS_VMCTL(VMCTL_PT_UNMAP)` (a never-faulted page returns a harmless
/// `EINVAL` from the kernel, which VM ignores). The reply carries
/// `m_type = OK`, or `EINVAL` in `m_type` if no `Mmap` region matches the base
/// address. (slice 3.6)
pub const VM_MUNMAP: i32 = VM_RQ_BASE + 3;

// ---------------------------------------------------------------------------
// DS (Data Store) server request numbers — `m_type` values for messages
// addressed to the DS server.
//
// DS is a name→endpoint registry: every server publishes its own endpoint at
// init (slice 4.2) so others can look each other up without hard-coding boot
// proc numbers. These are *server IPC requests* like the VM range, so they live
// in their own range, distinct from `KERNEL_CALL` (`0x600`), the VM request
// range (`VM_RQ_BASE = 0xC00`), and the SEF control range (`SEF_RQ_BASE =
// 0xD00`), and stay below the IPC `NOTIFY_MESSAGE` marker (`0x1000`) so neither
// a server's `m_type` dispatcher nor the SEF classifier can ever misroute.
//
// The key (a NUL-padded server name) travels inline in the request payload
// (`0..SYS_GETINFO_NAME_LEN`); no grants / cross-AS copy are needed because the
// kernel copies the whole 96-byte payload on delivery. An endpoint value rides
// in payload `16..20` (i32, native-endian) only on `DS_RETRIEVE` replies;
// `DS_PUBLISH` registers the caller's kernel-stamped `m_source` (a process can
// only publish itself), so no endpoint is sent in a publish request. Numbering
// is minix.rs-specific.
// ---------------------------------------------------------------------------

/// Base for DS server request `m_type` values.
pub const DS_RQ_BASE: i32 = 0xE00;

/// Server → DS: publish the *caller's own* endpoint under the key in payload
/// `0..SYS_GETINFO_NAME_LEN`. DS records the caller's kernel-stamped `m_source`,
/// not a value from the payload, so a process can only publish itself and can
/// never spoof another server's endpoint. Re-publishing the same key updates the
/// stored endpoint. Reply `m_type = OK`, or `EINVAL` (empty key) / `ENOMEM`
/// (registry full).
pub const DS_PUBLISH: i32 = DS_RQ_BASE;

/// Client → DS: look up the endpoint for the key in payload `0..NAME_LEN`.
/// Reply `m_type = OK` with the endpoint in payload `16..20` (i32), or
/// `ESRCH` if the key is not registered.
pub const DS_RETRIEVE: i32 = DS_RQ_BASE + 1;

/// Client → DS: test whether the key in payload `0..NAME_LEN` is registered.
/// Reply `m_type = OK` with a status in payload `16..20` (i32: 1 = present,
/// 0 = absent) — absence is a status, not an error, so a `CHECK` never aborts
/// the caller's SENDREC.
pub const DS_CHECK: i32 = DS_RQ_BASE + 2;

/// Number of DS server requests defined so far. Locks the dispatch-match
/// coverage in the DS server the way `NR_VMCTL_SUBCALLS` locks `do_vmctl`.
pub const NR_DS_REQUESTS: usize = 3;

// The DS range sits strictly above the SEF range (0xD00..0xD01) so a server's
// `m_type` dispatcher and the SEF classifier can never collide, and stays
// below the NOTIFY marker.
const _: () = assert!(DS_RQ_BASE > SEF_RQ_BASE + (NR_SEF_MSGS as i32 - 1));
const _: () = assert!(DS_RQ_BASE + (NR_DS_REQUESTS as i32 - 1) < crate::ipc_const::NOTIFY_MESSAGE);

// ---------------------------------------------------------------------------
// SEF (System Event Framework) control message numbers — `m_type` values the
// server runtime (`server-rt`) intercepts before handing traffic to a server.
//
// These live in their own range distinct from `KERNEL_CALL` (`0x600`) and the
// VM request range (`VM_RQ_BASE = 0xC00`), and stay below the IPC
// `NOTIFY_MESSAGE` marker (`0x1000`), so neither a server's `m_type`
// dispatcher nor the SEF classifier can ever misroute. Numbering is
// minix.rs-specific (MINIX 3 carries these inside `lib/libsys/sef.c` request
// types rather than `com.h`).
//
// The RS heartbeat ("ping") deliberately gets NO number here: it is delivered
// as a NOTIFY, so it arrives with `m_type == NOTIFY_MESSAGE` and is keyed on
// `m_source == RS` instead (see `server-rt`'s `classify`). Do not add a
// `SEF_PING` — there is no payload room in a NOTIFY to carry one anyway.
// ---------------------------------------------------------------------------

/// Base for SEF control message `m_type` values.
pub const SEF_RQ_BASE: i32 = 0xD00;

/// RS → server: run the registered fresh-init callback. (Re-init / live-update
/// variants are deferred past Phase 4.)
pub const SEF_INIT: i32 = SEF_RQ_BASE;

/// PM/RS → server: deliver a signal. The signal number is in payload `0..4`
/// (i32, native-endian); `server-rt` dispatches it to the registered signal
/// handler.
pub const SEF_SIGNAL: i32 = SEF_RQ_BASE + 1;

/// Number of SEF control messages defined so far. Locks the classifier's
/// coverage in `server-rt` the way `NR_VMCTL_SUBCALLS` locks `do_vmctl`.
pub const NR_SEF_MSGS: usize = 2;

// The SEF range sits strictly above the VM request range (0xC00..0xC03) so a
// server's `m_type` dispatcher and the SEF classifier can never collide.
const _: () = assert!(SEF_RQ_BASE > VM_RQ_BASE + 3);
const _: () = assert!(SEF_RQ_BASE < crate::ipc_const::NOTIFY_MESSAGE);

// ---------------------------------------------------------------------------
// SCHED (scheduler) server request numbers — `m_type` values for messages
// addressed to the user-space SCHED server (slice 4.3).
//
// `SCHEDULING_NO_QUANTUM` is kernel-originated: when a SCHED-scheduled proc
// exhausts its quantum, the kernel sends it (with `m_source` = the preempted
// proc, so SCHED knows which proc to reschedule), exactly as it originates
// `VM_PAGEFAULT` for a faulter. The other three are PM/RS → SCHED requests
// (claim/release/renice a managed proc). Like the VM/DS ranges these are
// *server IPC requests*, not kernel calls, so they live in their own range
// distinct from `KERNEL_CALL` (`0x600`), VM (`0xC00`), SEF (`0xD00`), and DS
// (`0xE00`), and stay below the IPC `NOTIFY_MESSAGE` marker (`0x1000`) so the
// SEF classifier (which returns `Application` for them) can never misroute.
// Numbering is minix.rs-specific (MINIX 3 carries `SCHEDULING_*` in `com.h`).
// ---------------------------------------------------------------------------

/// Base for SCHED server request `m_type` values.
pub const SCHED_RQ_BASE: i32 = 0xF00;

/// Kernel → SCHED: the proc identified by `m_source` used up its full quantum.
/// SCHED applies its policy and re-admits the proc via `SYS_SCHEDULE`. Carries
/// no payload — `m_source` is the whole request.
pub const SCHEDULING_NO_QUANTUM: i32 = SCHED_RQ_BASE;

/// PM/RS → SCHED: start scheduling a proc. The target endpoint is in payload
/// `0..4` (i32), the initial priority in `4..8` (i32), and the quantum (ms) in
/// `8..12` (i32). SCHED claims the target via `SYS_SCHEDCTL` and assigns the
/// initial priority/quantum via `SYS_SCHEDULE`. (Driven by PM/RS from slice 4.5+.)
pub const SCHEDULING_START: i32 = SCHED_RQ_BASE + 1;

/// PM/RS → SCHED: stop scheduling the target (payload `0..4`, i32). SCHED hands
/// it back to the kernel scheduler via `SYS_SCHEDCTL(SCHEDCTL_FLAG_KERNEL)`.
pub const SCHEDULING_STOP: i32 = SCHED_RQ_BASE + 2;

/// PM/RS → SCHED: change the target's nice value. Target endpoint in payload
/// `0..4` (i32), new priority in `4..8` (i32). SCHED records it and applies it
/// via `SYS_SCHEDULE`.
pub const SCHEDULING_SET_NICE: i32 = SCHED_RQ_BASE + 3;

/// Number of SCHED server requests defined so far. Locks the dispatch-match
/// coverage in the SCHED server the way `NR_DS_REQUESTS` locks the DS server.
pub const NR_SCHED_MSGS: usize = 4;

// The SCHED range sits strictly above the DS range (0xE00..0xE02) and below the
// NOTIFY marker, so neither a server's `m_type` dispatcher nor the SEF
// classifier can ever collide with it.
const _: () = assert!(SCHED_RQ_BASE > DS_RQ_BASE + (NR_DS_REQUESTS as i32 - 1));
const _: () =
    assert!(SCHED_RQ_BASE + (NR_SCHED_MSGS as i32 - 1) < crate::ipc_const::NOTIFY_MESSAGE);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_are_contiguous_from_base() {
        let calls = [
            SYS_GETINFO,
            SYS_PRIVCTL,
            SYS_FORK,
            SYS_EXEC,
            SYS_EXIT,
            SYS_COPY,
            SYS_SAFECOPY,
            SYS_IRQCTL,
            SYS_VMCTL,
            SYS_SCHEDULE,
            SYS_SETALARM,
            SYS_TIMES,
            SYS_DIAGCTL,
            SYS_SETGRANT,
            SYS_SCHEDCTL,
            SYS_KILL,
            SYS_GETKSIG,
            SYS_ENDKSIG,
        ];
        for (i, call) in calls.iter().enumerate() {
            assert_eq!(*call, KERNEL_CALL + i as i32);
        }
        assert_eq!(calls.len(), NR_KERN_CALLS_PHASE4);
    }

    #[test]
    fn kernel_call_base_matches_minix3() {
        assert_eq!(KERNEL_CALL, 0x600);
    }

    #[test]
    fn get_whoami_matches_minix3() {
        // Pinned by MINIX 3 include/minix/sysinfo.h; servers / musl wrappers
        // built later in the project depend on this value.
        assert_eq!(GET_WHOAMI, 12);
    }

    #[test]
    fn vmctl_subcalls_are_contiguous_from_one() {
        // Subcall 0 is reserved as "invalid" (a zeroed payload). The six
        // real subcalls are 1..=6 and distinct; `NR_VMCTL_SUBCALLS` locks
        // the dispatch coverage in `system::do_vmctl`.
        let subcalls = [
            VMCTL_PT_MAP,
            VMCTL_PT_UNMAP,
            VMCTL_CLEAR_PAGEFAULT,
            VMCTL_GET_PAGEFAULT,
            VMCTL_VMINHIBIT_SET,
            VMCTL_VMINHIBIT_CLEAR,
        ];
        for (i, sc) in subcalls.iter().enumerate() {
            assert_eq!(*sc, 1 + i as i32);
        }
        assert_eq!(subcalls.len(), NR_VMCTL_SUBCALLS);
    }

    #[test]
    fn vm_pagefault_distinct_from_kernel_calls_and_notify() {
        // VM requests must not collide with the KERNEL_CALL range, the IPC
        // NOTIFY_MESSAGE marker, or any SYS_* number — a server dispatcher
        // keys on m_type and a collision would misroute.
        assert_eq!(VM_PAGEFAULT, VM_RQ_BASE);
        assert!(VM_PAGEFAULT > KERNEL_CALL + NR_KERN_CALLS_PHASE4 as i32);
        assert_ne!(VM_PAGEFAULT, crate::ipc_const::NOTIFY_MESSAGE);
    }

    #[test]
    fn vm_brk_follows_pagefault_in_request_range() {
        // VM_BRK is the second VM server request, contiguous after VM_PAGEFAULT.
        // It must stay distinct from the page-fault request, the KERNEL_CALL
        // range, and the NOTIFY marker so VM's m_type dispatcher can't misroute.
        assert_eq!(VM_BRK, VM_RQ_BASE + 1);
        assert_ne!(VM_BRK, VM_PAGEFAULT);
        assert!(VM_BRK > KERNEL_CALL + NR_KERN_CALLS_PHASE4 as i32);
        assert_ne!(VM_BRK, crate::ipc_const::NOTIFY_MESSAGE);
    }

    #[test]
    fn vm_mmap_follows_brk_in_request_range() {
        // VM_MMAP is the third VM server request, contiguous after VM_BRK.
        assert_eq!(VM_MMAP, VM_RQ_BASE + 2);
        assert_ne!(VM_MMAP, VM_PAGEFAULT);
        assert_ne!(VM_MMAP, VM_BRK);
        assert!(VM_MMAP > KERNEL_CALL + NR_KERN_CALLS_PHASE4 as i32);
        assert_ne!(VM_MMAP, crate::ipc_const::NOTIFY_MESSAGE);
    }

    #[test]
    fn vm_munmap_follows_mmap_in_request_range() {
        // VM_MUNMAP is the fourth VM server request, contiguous after VM_MMAP.
        // Each VM request must stay distinct from the others, the KERNEL_CALL
        // range, and the NOTIFY marker so VM's m_type dispatcher can't misroute.
        assert_eq!(VM_MUNMAP, VM_RQ_BASE + 3);
        assert_ne!(VM_MUNMAP, VM_MMAP);
        assert_ne!(VM_MUNMAP, VM_BRK);
        assert_ne!(VM_MUNMAP, VM_PAGEFAULT);
        assert!(VM_MUNMAP > KERNEL_CALL + NR_KERN_CALLS_PHASE4 as i32);
        assert_ne!(VM_MUNMAP, crate::ipc_const::NOTIFY_MESSAGE);
    }

    #[test]
    fn ds_requests_contiguous_from_base() {
        // DS requests are contiguous from DS_RQ_BASE; NR_DS_REQUESTS locks the
        // DS server's dispatch coverage.
        let reqs = [DS_PUBLISH, DS_RETRIEVE, DS_CHECK];
        for (i, r) in reqs.iter().enumerate() {
            assert_eq!(*r, DS_RQ_BASE + i as i32);
        }
        assert_eq!(reqs.len(), NR_DS_REQUESTS);
    }

    #[test]
    fn ds_requests_distinct_from_other_ranges() {
        // Each DS request must stay distinct from the VM request range, the SEF
        // control range, and the KERNEL_CALL range, and below NOTIFY_MESSAGE —
        // so a server's m_type dispatcher and the SEF classifier never collide.
        for r in [DS_PUBLISH, DS_RETRIEVE, DS_CHECK] {
            for vm in [VM_PAGEFAULT, VM_BRK, VM_MMAP, VM_MUNMAP] {
                assert_ne!(r, vm);
            }
            assert_ne!(r, SEF_INIT);
            assert_ne!(r, SEF_SIGNAL);
            assert!(r > SEF_RQ_BASE + (NR_SEF_MSGS as i32 - 1));
            assert!(r > KERNEL_CALL + NR_KERN_CALLS_PHASE4 as i32);
            assert_ne!(r, crate::ipc_const::NOTIFY_MESSAGE);
            assert!(r < crate::ipc_const::NOTIFY_MESSAGE);
        }
    }

    #[test]
    fn sef_msgs_contiguous_from_base() {
        // SEF control messages are contiguous from SEF_RQ_BASE; NR_SEF_MSGS
        // locks `server-rt`'s classifier coverage.
        let msgs = [SEF_INIT, SEF_SIGNAL];
        for (i, m) in msgs.iter().enumerate() {
            assert_eq!(*m, SEF_RQ_BASE + i as i32);
        }
        assert_eq!(msgs.len(), NR_SEF_MSGS);
    }

    #[test]
    fn sched_msgs_contiguous_from_base() {
        // SCHED requests are contiguous from SCHED_RQ_BASE; NR_SCHED_MSGS locks
        // the SCHED server's dispatch coverage.
        let msgs = [
            SCHEDULING_NO_QUANTUM,
            SCHEDULING_START,
            SCHEDULING_STOP,
            SCHEDULING_SET_NICE,
        ];
        for (i, m) in msgs.iter().enumerate() {
            assert_eq!(*m, SCHED_RQ_BASE + i as i32);
        }
        assert_eq!(msgs.len(), NR_SCHED_MSGS);
    }

    #[test]
    fn sched_msgs_distinct_from_other_ranges() {
        // Each SCHED request must stay distinct from the VM/DS/SEF request
        // ranges and the KERNEL_CALL range, and below NOTIFY_MESSAGE — so a
        // server's m_type dispatcher and the SEF classifier never collide.
        for m in [
            SCHEDULING_NO_QUANTUM,
            SCHEDULING_START,
            SCHEDULING_STOP,
            SCHEDULING_SET_NICE,
        ] {
            for other in [
                VM_PAGEFAULT,
                VM_BRK,
                VM_MMAP,
                VM_MUNMAP,
                DS_PUBLISH,
                DS_RETRIEVE,
                DS_CHECK,
                SEF_INIT,
                SEF_SIGNAL,
            ] {
                assert_ne!(m, other);
            }
            assert!(m > DS_RQ_BASE + (NR_DS_REQUESTS as i32 - 1));
            assert!(m > KERNEL_CALL + NR_KERN_CALLS_PHASE4 as i32);
            assert_ne!(m, crate::ipc_const::NOTIFY_MESSAGE);
            assert!(m < crate::ipc_const::NOTIFY_MESSAGE);
        }
    }

    #[test]
    fn endksig_is_last_kernel_call() {
        // The slice-4.5 signal trio extends the Phase-4 call set; the count
        // must cover it.
        assert_eq!(SYS_KILL, KERNEL_CALL + 15);
        assert_eq!(SYS_GETKSIG, KERNEL_CALL + 16);
        assert_eq!(SYS_ENDKSIG, KERNEL_CALL + 17);
        assert_eq!(NR_KERN_CALLS_PHASE4, 18);
    }

    #[test]
    fn privctl_set_user_is_nonzero() {
        // Subcode 0 is reserved as "invalid" (a zeroed payload), the VMCTL
        // convention.
        assert_eq!(PRIVCTL_SET_USER, 1);
    }

    #[test]
    fn pm_msgs_contiguous_from_base() {
        // PM requests are contiguous from PM_RQ_BASE; NR_PM_MSGS locks the PM
        // server's dispatch coverage.
        let msgs = [PM_GETPID];
        for (i, m) in msgs.iter().enumerate() {
            assert_eq!(*m, PM_RQ_BASE + i as i32);
        }
        assert_eq!(msgs.len(), NR_PM_MSGS);
    }

    #[test]
    fn pm_msgs_distinct_from_other_ranges() {
        // Each PM request must stay distinct from the VM/DS/SEF/SCHED request
        // ranges and the KERNEL_CALL range, and below NOTIFY_MESSAGE — so a
        // server's m_type dispatcher and the SEF classifier never collide.
        for m in [PM_GETPID] {
            for other in [
                VM_PAGEFAULT,
                VM_BRK,
                VM_MMAP,
                VM_MUNMAP,
                DS_PUBLISH,
                DS_RETRIEVE,
                DS_CHECK,
                SEF_INIT,
                SEF_SIGNAL,
                SCHEDULING_NO_QUANTUM,
                SCHEDULING_START,
                SCHEDULING_STOP,
                SCHEDULING_SET_NICE,
            ] {
                assert_ne!(m, other);
            }
            assert!(m > KERNEL_CALL + NR_KERN_CALLS_PHASE4 as i32 - 1);
            assert!(m < VM_RQ_BASE);
            assert_ne!(m, crate::ipc_const::NOTIFY_MESSAGE);
            assert!(m < crate::ipc_const::NOTIFY_MESSAGE);
        }
    }

    #[test]
    fn sef_msgs_distinct_from_vm_kernel_and_notify_ranges() {
        // Each SEF control message must stay distinct from the VM request
        // range, the KERNEL_CALL range, and the NOTIFY marker — and below
        // NOTIFY_MESSAGE — so a server's m_type dispatcher and the SEF
        // classifier can never collide. (The base-vs-VM-range ordering is
        // additionally locked by a module-level const-assert.)
        for m in [SEF_INIT, SEF_SIGNAL] {
            assert_ne!(m, VM_PAGEFAULT);
            assert_ne!(m, VM_BRK);
            assert_ne!(m, VM_MMAP);
            assert_ne!(m, VM_MUNMAP);
            assert!(m > KERNEL_CALL + NR_KERN_CALLS_PHASE4 as i32);
            assert_ne!(m, crate::ipc_const::NOTIFY_MESSAGE);
            assert!(m < crate::ipc_const::NOTIFY_MESSAGE);
        }
    }
}
