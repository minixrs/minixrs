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

/// Number of kernel calls reserved for Phase 2.
pub const NR_KERN_CALLS_PHASE2: usize = 14;

/// Size of the privilege-table kernel-call mask, in bits. Sized as a single
/// `u32` chunk (32 slots) to leave headroom past Phase 2's 14 calls while
/// keeping the bitmap a single word per privilege slot.
pub const NR_SYS_CALLS: usize = 32;

const _: () = assert!(NR_SYS_CALLS >= NR_KERN_CALLS_PHASE2);
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
        assert_eq!(calls.len(), NR_KERN_CALLS_PHASE2);
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
}
