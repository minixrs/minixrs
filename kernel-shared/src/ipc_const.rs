//! IPC primitive numbers — values that go in `r1`/`x16` on the IPC trap.
//!
//! Matches MINIX 3 `include/minix/ipcconst.h`. The numbering is shared
//! between the kernel's `do_ipc()` dispatch and the user-space stub.

/// Blocking send. Caller blocks until the receiver accepts the message.
pub const SEND: i32 = 1;
/// Blocking receive. Caller blocks until a message arrives.
pub const RECEIVE: i32 = 2;
/// Atomic send + receive (the common case for client-server calls).
pub const SENDREC: i32 = 3;
/// Non-blocking notification — sets a bit in the receiver's notify bitmap.
pub const NOTIFY: i32 = 4;
/// Non-blocking send — returns `ENOTREADY` if the receiver is not waiting.
pub const SENDNB: i32 = 5;
/// Asynchronous send — the kernel walks the caller's async-send table.
pub const SENDA: i32 = 16;

/// Kernel-info introspection trap (separate from `SYS_GETINFO`); reserved
/// but unused in Phase 2.
pub const MINIX_KERNINFO: i32 = 32;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_primitives_distinct() {
        let all = [SEND, RECEIVE, SENDREC, NOTIFY, SENDNB, SENDA, MINIX_KERNINFO];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "duplicate primitive number {a}");
            }
        }
    }

    #[test]
    fn primitive_numbers_match_minix3() {
        // Values pinned by the MINIX 3 ABI — these must not drift.
        assert_eq!(SEND, 1);
        assert_eq!(RECEIVE, 2);
        assert_eq!(SENDREC, 3);
        assert_eq!(NOTIFY, 4);
        assert_eq!(SENDNB, 5);
        assert_eq!(SENDA, 16);
    }
}
