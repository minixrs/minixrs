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
}
