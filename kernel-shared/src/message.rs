// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Fixed-size IPC message.
//!
//! The 104-byte layout matches MINIX 3's `sizeof(message) == 104` assertion
//! for `__x86_64__` (see `include/minix/ipc.h`). Both x86_64 and aarch64 ports
//! of minix.rs adopt the same size so that the message is ABI-portable.
//!
//! Note: MINIX 3 only ever shipped as a 32-bit OS (i386, and 32-bit ARM). The
//! `__x86_64__` layout we anchor on exists in the MINIX 3 source tree, but a
//! working 64-bit MINIX 3 was never an upstream release — it was a personal
//! prototype by this project's author. We treat the 104-byte layout as an ABI
//! *reference*, not a shipped 64-bit MINIX 3.
//!
//! The struct is explicitly 8-aligned. MINIX 3's `message` is a union over
//! sub-structs containing `uint64_t` fields, so its native alignment is 8.
//! minix.rs expresses `payload` as `[u8; 96]` for now, but future typed
//! accessor structs (slice 2.5) will overlay payload regions that contain
//! `u64` fields. Forcing the message itself to 8-align guarantees those
//! overlays are 8-aligned relative to the message base — required for
//! strict-alignment loads on aarch64.
//!
//! Typed access — MINIX 3 had a union over many `mess_*` payload variants
//! (`m1i1`, `m2l1`, etc.). minix.rs will eventually expose strongly-typed
//! accessor structs per call (e.g., `as_vfs_read()`), but those live in the
//! servers/userland crates so the kernel doesn't pull in every protocol
//! definition. For now, `payload` is a raw byte array that callers
//! interpret per-call.

use crate::endpoint::Endpoint;

/// 104-byte fixed-size IPC message. Layout matches MINIX 3 x86_64.
#[repr(C, align(8))]
#[derive(Copy, Clone, Debug)]
pub struct Message {
    /// Endpoint of the sender (set by the kernel on delivery).
    pub m_source: Endpoint,
    /// Call number on send; result code on reply.
    pub m_type: i32,
    /// Raw payload — interpreted per-call by typed accessors.
    pub payload: [u8; 96],
}

// Compile-time guarantee that the layout matches MINIX 3's x86_64 message.
const _: () = assert!(core::mem::size_of::<Message>() == 104);
const _: () = assert!(core::mem::align_of::<Message>() == 8);

/// One past the highest user VA reachable through TTBR0. Limine programs
/// `TCR_EL1.T0SZ = 16`, giving a 48-bit user VA space (`[0, 2^48)`); the
/// kernel's HHDM lives in the very high half via TTBR1 and is unreachable
/// from this range.
pub const USER_VA_TOP: u64 = 1 << 48;

/// Bounds check for a user pointer of length `len` starting at `va`.
///
/// Rejects:
///   - NULL or sub-alignment-of-[`Message`] addresses (a NULL pointer or an
///     unaligned `read_volatile` is UB even before the access faults).
///   - Pointers whose range extends past [`USER_VA_TOP`].
///
/// A pure predicate over the message ABI shape, so it lives here (and is
/// host-testable) rather than in the kernel's copy helpers.
#[inline]
pub fn user_va_ok(va: u64, len: usize) -> bool {
    if va < core::mem::align_of::<Message>() as u64 {
        return false;
    }
    if !va.is_multiple_of(core::mem::align_of::<Message>() as u64) {
        return false;
    }
    match va.checked_add(len as u64) {
        Some(end) => end <= USER_VA_TOP,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_is_104_bytes() {
        assert_eq!(core::mem::size_of::<Message>(), 104);
    }

    #[test]
    fn message_align_is_8() {
        assert_eq!(core::mem::align_of::<Message>(), 8);
    }

    #[test]
    fn message_payload_offset_is_8() {
        let m = Message {
            m_source: 0,
            m_type: 0,
            payload: [0; 96],
        };
        let base = &m as *const _ as usize;
        let payload = m.payload.as_ptr() as usize;
        assert_eq!(payload - base, 8);
    }

    #[test]
    fn user_va_ok_rejects_null() {
        assert!(!user_va_ok(0, core::mem::size_of::<Message>()));
    }

    #[test]
    fn user_va_ok_rejects_unaligned() {
        assert!(!user_va_ok(0x10001, core::mem::size_of::<Message>()));
    }

    #[test]
    fn user_va_ok_rejects_at_top() {
        assert!(!user_va_ok(USER_VA_TOP, core::mem::size_of::<Message>()));
    }

    #[test]
    fn user_va_ok_rejects_overflow() {
        assert!(!user_va_ok(u64::MAX - 7, core::mem::size_of::<Message>()));
    }

    #[test]
    fn user_va_ok_accepts_aligned_in_range() {
        // 8 MiB — where the boot stubs' stacks live.
        assert!(user_va_ok(0x80_0000, core::mem::size_of::<Message>()));
    }
}
