// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! A driver's claim on one interrupt line (`SYS_IRQCTL`), shared by every device
//! driver (slice 6.2, spec T9).
//!
//! [`IrqLine`] owns nothing beyond the kernel's hook id and the notify bit the
//! claim was made under. The "drain, acknowledge at the device, then re-arm"
//! order of I12 stays the caller's, because only the caller knows what
//! acknowledging its device means. There is no `Drop`: a boot driver never
//! releases its line, and the kernel's release-on-exit (I8) is the real cleanup.

use minixrs_kernel_shared::callnr::{IRQ_ENABLE, IRQ_SETPOLICY};
use minixrs_kernel_shared::error::{EINVAL, OK};
use minixrs_server_rt::sys_irqctl;

/// One claimed interrupt line: the hook id the kernel answered with and the bit
/// index the line's notifications carry in the pending word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrqLine {
    hook: i32,
    notify_id: u8,
}

impl IrqLine {
    /// Claim `intid` under `notify_id` (`IRQ_SETPOLICY`, policy 0). A repeat
    /// claim by the same owner under the same `notify_id` replaces the hook in
    /// place and answers the same id (I6).
    ///
    /// `notify_id` is a bit index into a `u32`, so it must be `0..=31`; anything
    /// else is `Err(EINVAL)` without a trap. A kernel refusal comes back as its
    /// negative errno.
    pub fn new(intid: u32, notify_id: u8) -> Result<Self, i32> {
        if notify_id > 31 {
            return Err(EINVAL);
        }
        let Ok(vector) = i32::try_from(intid) else {
            return Err(EINVAL);
        };
        let (rc, hook) = sys_irqctl(IRQ_SETPOLICY, vector, 0, i32::from(notify_id));
        if rc != OK {
            return Err(rc);
        }
        Ok(Self { hook, notify_id })
    }

    /// Re-arm the line (`IRQ_ENABLE`); the kernel leaves it masked after a
    /// delivery until this is called. Returns `OK` or a negative errno.
    pub fn rearm(&self) -> i32 {
        sys_irqctl(IRQ_ENABLE, 0, 0, self.hook).0
    }

    /// Whether this line's bit is set in a `HARDWARE` notification's pending
    /// word `pending`. Other lines' bits are ignored.
    pub const fn fired(&self, pending: u32) -> bool {
        pending & (1u32 << self.notify_id) != 0
    }

    /// The kernel's hook id for this line.
    pub const fn hook_id(&self) -> i32 {
        self.hook
    }

    #[cfg(test)]
    const fn for_test(hook: i32, notify_id: u8) -> Self {
        Self { hook, notify_id }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fired_checks_only_its_own_bit() {
        let line = IrqLine::for_test(1, 3);
        assert!(line.fired(0b1000));
        assert!(!line.fired(0b0111));
        assert!(!line.fired(0));
        assert_eq!(line.hook_id(), 1);
    }

    #[test]
    fn new_rejects_out_of_range_notify_id_without_a_trap() {
        assert_eq!(IrqLine::new(33, 32), Err(EINVAL));
        assert_eq!(IrqLine::new(33, 255), Err(EINVAL));
    }

    #[test]
    fn top_bit_is_addressable() {
        let line = IrqLine::for_test(2, 31);
        assert!(line.fired(1 << 31));
        assert!(!line.fired(!(1 << 31)));
    }
}
