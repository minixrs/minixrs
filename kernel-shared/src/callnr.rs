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

/// Number of kernel calls defined through Phase 3. Slice 3.3 adds no new
/// `SYS_*` (it fills in `SYS_VMCTL`'s subcalls instead), so the count is
/// unchanged from Phase 2 — the rename just gives `system/mod.rs`'s
/// arm-coverage const-assert a phase-appropriate name as Phase 3 lands.
pub const NR_KERN_CALLS_PHASE3: usize = 14;

/// Size of the privilege-table kernel-call mask, in bits. Sized as a single
/// `u32` chunk (32 slots) to leave headroom past Phase 2's 14 calls while
/// keeping the bitmap a single word per privilege slot.
pub const NR_SYS_CALLS: usize = 32;

const _: () = assert!(NR_SYS_CALLS >= NR_KERN_CALLS_PHASE3);
const _: () = assert!(NR_SYS_CALLS % 32 == 0);

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

/// Length of the `name` field in the `GET_WHOAMI` reply payload. MINIX 4 uses
/// the kernel's own `PROC_NAME_LEN` here rather than MINIX 3's 44-byte field —
/// the name is only used for debug/log output and the kernel never stores more
/// than 16 bytes per slot.
pub const SYS_GETINFO_NAME_LEN: usize = 16;

// ---------------------------------------------------------------------------
// `SYS_VMCTL` subcalls.
//
// `SYS_VMCTL` mediates all user-space page-table changes: the kernel owns the
// physical frame allocator and every unsafe PTE write, and VM (slice 3.4)
// drives policy by issuing these subcalls. The subcall selector lives in the
// first 4 bytes of the message payload (same convention as `GET_WHOAMI`); the
// target process is named by an endpoint in the next 4 bytes (`SELF` allowed).
// Numbers start at 1 so a zeroed payload (subcall 0) is an obvious "invalid".
// These are MINIX 4-specific — MINIX 3's VMCTL subcall set differs because its
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
// VM server request numbers — `m_type` values for messages addressed to VM.
//
// These are *server IPC requests*, not kernel calls, so they live in their own
// range distinct from `KERNEL_CALL` (`0x600`). The kernel originates
// `VM_PAGEFAULT` on a faulting process's behalf (slice 3.4); later slices add
// `VM_BRK` / `VM_MMAP`. Numbering is MINIX 4-specific (MINIX 3's VM request set
// differs because its frame allocator lives in VM, not the kernel).
// ---------------------------------------------------------------------------

/// Base for VM server request `m_type` values.
pub const VM_RQ_BASE: i32 = 0xC00;

/// Kernel → VM: a process page-faulted. `m_source` identifies the faulting
/// process; the payload carries the fault address (`0..8`, u64) and fault
/// flags (`8..12`, u32). VM resolves it via `SYS_VMCTL(VMCTL_PT_MAP)` +
/// `SYS_VMCTL(VMCTL_CLEAR_PAGEFAULT)`.
pub const VM_PAGEFAULT: i32 = VM_RQ_BASE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_are_contiguous_from_base() {
        let calls = [
            SYS_GETINFO, SYS_PRIVCTL, SYS_FORK, SYS_EXEC, SYS_EXIT,
            SYS_COPY, SYS_SAFECOPY, SYS_IRQCTL, SYS_VMCTL, SYS_SCHEDULE,
            SYS_SETALARM, SYS_TIMES, SYS_DIAGCTL, SYS_SETGRANT,
        ];
        for (i, call) in calls.iter().enumerate() {
            assert_eq!(*call, KERNEL_CALL + i as i32);
        }
        assert_eq!(calls.len(), NR_KERN_CALLS_PHASE3);
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
        assert!(VM_PAGEFAULT > KERNEL_CALL + NR_KERN_CALLS_PHASE3 as i32);
        assert_ne!(VM_PAGEFAULT, crate::ipc_const::NOTIFY_MESSAGE);
    }
}
