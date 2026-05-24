//! User-memory IPC message copies.
//!
//! Each `mini_send` / `mini_receive` / `mini_notify` handler accepts a
//! user-supplied VA in `frame.x[2]`. The kernel reads the outgoing
//! [`Message`] out of that buffer when the caller blocks, and writes the
//! incoming `Message` back to that buffer when the kernel resumes the
//! caller (the `MF_DELIVERMSG` flush).
//!
//! Slice 2.5's MMU is a single hand-rolled TTBR0 set up by
//! `arch::aarch64::userland_bootstrap`: one shared code page (now two —
//! one per stub) and per-task stack pages all under `USER_VA_TOP`. Because
//! the same TTBR0 is live across every EL0 task in this slice, reading
//! and writing user-mode VAs from EL1 just walks the active TTBR0; no
//! switch is needed.
//!
//! We do a coarse bounds check and let the hardware fault if a stub ever
//! passes a pointer into an unmapped page. Slice 2.5's stubs always pass
//! a stack-resident buffer, so this is unreachable in practice; phase 3's
//! VM server replaces this entire helper with a fault-recovering
//! `copy_from_user` once per-process page tables exist.

use minix4_kernel_shared::error::EFAULT;
use minix4_kernel_shared::message::Message;

/// One past the highest user VA reachable through TTBR0. Limine programs
/// `TCR_EL1.T0SZ = 16`, giving a 48-bit user VA space (`[0, 2^48)`); the
/// kernel's HHDM lives in the very high half via TTBR1 and is unreachable
/// from this range.
pub const USER_VA_TOP: u64 = 1 << 48;

/// Bounds check for a user pointer of length `len` starting at `va`.
///
/// Rejects:
///   - NULL or sub-alignment-of-`Message` addresses (a NULL pointer or an
///     unaligned `read_volatile` is UB even before the access faults).
///   - Pointers whose range extends past `USER_VA_TOP`.
#[inline]
fn user_va_ok(va: u64, len: usize) -> bool {
    if va < core::mem::align_of::<Message>() as u64 {
        return false;
    }
    if va % (core::mem::align_of::<Message>() as u64) != 0 {
        return false;
    }
    match va.checked_add(len as u64) {
        Some(end) => end <= USER_VA_TOP,
        None => false,
    }
}

/// Copy a [`Message`] out of user memory at `va`.
///
/// SAFETY: walks the currently-active TTBR0. We assume the same single-
/// threaded SVC/IRQ-masked invariant the rest of the IPC code relies on,
/// so no concurrent unmap can race the read. If `va` is in range but
/// unmapped, the EL1 access faults and we panic via the slice-2.4
/// `el0_sync_unexpected` path — acceptable until phase 3 adds a real
/// fault-recovering copy.
pub(crate) fn copy_msg_from_user(va: u64) -> Result<Message, i32> {
    if !user_va_ok(va, core::mem::size_of::<Message>()) {
        return Err(EFAULT);
    }
    // SAFETY: bounds-checked above; `Message` alignment confirmed; under
    // the single-threaded SVC invariant no concurrent writer exists.
    let m = unsafe { core::ptr::read_volatile(va as *const Message) };
    Ok(m)
}

/// Write a [`Message`] into user memory at `va`.
///
/// See [`copy_msg_from_user`] for the SAFETY discussion.
pub(crate) fn copy_msg_to_user(va: u64, m: &Message) -> Result<(), i32> {
    if !user_va_ok(va, core::mem::size_of::<Message>()) {
        return Err(EFAULT);
    }
    // SAFETY: bounds-checked above; same single-threaded invariant.
    unsafe {
        core::ptr::write_volatile(va as *mut Message, *m);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // 8 MiB — where the slice's stub stacks live.
        assert!(user_va_ok(0x80_0000, core::mem::size_of::<Message>()));
    }
}
