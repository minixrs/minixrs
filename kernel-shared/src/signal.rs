// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Signal numbers (slice 4.5).
//!
//! Only the signals the kernel and PM currently raise are defined; the full
//! set arrives with the musl wrappers in Phase 5. Values are the POSIX
//! numbers, matching MINIX 3 `sys/sys/signal.h`, so the wire format is
//! already ABI-correct.

/// Kill (cannot be caught or ignored).
pub const SIGKILL: i32 = 9;
/// Invalid memory reference — raised by VM via `SYS_KILL` when a page fault
/// lands outside every region of the faulter (slice 4.5).
pub const SIGSEGV: i32 = 11;
/// Software termination signal.
pub const SIGTERM: i32 = 15;

/// One past the highest valid signal number; valid signals are `1..NSIG`.
/// Sized so a pending-signal set fits one `u32` bitmap (`Proc::sig_pending`).
pub const NSIG: usize = 32;

const _: () = assert!(NSIG <= 32);
const _: () = assert!(SIGKILL > 0 && (SIGKILL as usize) < NSIG);
const _: () = assert!(SIGSEGV > 0 && (SIGSEGV as usize) < NSIG);
const _: () = assert!(SIGTERM > 0 && (SIGTERM as usize) < NSIG);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_numbers_match_posix() {
        // Pinned by MINIX 3 sys/sys/signal.h (POSIX numbering); the musl
        // wrappers built in Phase 5 depend on these values.
        assert_eq!(SIGKILL, 9);
        assert_eq!(SIGSEGV, 11);
        assert_eq!(SIGTERM, 15);
    }

    #[test]
    fn signals_fit_pending_bitmap() {
        assert_eq!(NSIG, 32);
        for s in [SIGKILL, SIGSEGV, SIGTERM] {
            assert!(s >= 1 && (s as usize) < NSIG);
        }
    }
}
