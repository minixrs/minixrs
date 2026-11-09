// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_IRQCTL` request decoding and the IRQ hook table (slice 6.1, design
//! I1/I2/I6/I10).
//!
//! Pure logic over the ABI shape in [`crate::callnr`], kept here so it is
//! host-testable; the kernel's handler owns the GIC and the privilege slots.

use crate::callnr::{IRQ_DISABLE, IRQ_ENABLE, IRQ_RMPOLICY, IRQ_SETPOLICY};
use crate::endpoint::{self, Endpoint};
use crate::error::{EBUSY, EINVAL, ENOSPC, EPERM};

/// Capacity of the kernel's IRQ hook table (I6).
pub const NR_IRQ_HOOKS: usize = 16;
/// First claimable GIC INTID; INTIDs below are SGIs and PPIs (I3).
pub const FIRST_SPI: u32 = 32;
/// Highest `notify_id`: it is a bit index into a `u32` pending word.
pub const NOTIFY_ID_MAX: i32 = 31;

/// A decoded, range-checked `SYS_IRQCTL` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrqRequest {
    /// Claim `vector` (a raw SPI INTID), notifying with bit `notify_id`.
    SetPolicy { vector: u32, notify_id: u8 },
    /// Release the hook. `hook_id` is unchecked; the table range-checks it.
    RmPolicy { hook_id: i32 },
    /// Unmask the hook's line. `hook_id` is unchecked.
    Enable { hook_id: i32 },
    /// Mask the hook's line. `hook_id` is unchecked.
    Disable { hook_id: i32 },
}

/// Decode the four payload words of a `SYS_IRQCTL` request. Errors are
/// negative errnos, ready to be returned as the kernel-call result.
///
/// `id` is `notify_id` for `IRQ_SETPOLICY` (range-checked as a signed value)
/// and `hook_id` for the rest. `policy` must be 0 (I10).
pub fn decode(
    request: i32,
    vector: i32,
    policy: i32,
    id: i32,
    nr_intids: u32,
) -> Result<IrqRequest, i32> {
    match request {
        IRQ_SETPOLICY => {
            let v = i64::from(vector);
            if v < i64::from(FIRST_SPI) || v >= i64::from(nr_intids) {
                return Err(EINVAL);
            }
            if policy != 0 || !(0..=NOTIFY_ID_MAX).contains(&id) {
                return Err(EINVAL);
            }
            Ok(IrqRequest::SetPolicy {
                vector: vector as u32,
                notify_id: id as u8,
            })
        }
        IRQ_RMPOLICY => Ok(IrqRequest::RmPolicy { hook_id: id }),
        IRQ_ENABLE => Ok(IrqRequest::Enable { hook_id: id }),
        IRQ_DISABLE => Ok(IrqRequest::Disable { hook_id: id }),
        _ => Err(EINVAL),
    }
}

/// One slot of the hook table. `owner == endpoint::NONE` means free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrqHook {
    pub owner: Endpoint,
    pub intid: u32,
    pub notify_id: u8,
}

impl IrqHook {
    const FREE: IrqHook = IrqHook {
        owner: endpoint::NONE,
        intid: 0,
        notify_id: 0,
    };

    const fn is_free(&self) -> bool {
        self.owner == endpoint::NONE
    }
}

/// Result of a successful [`HookTable::set_policy`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Claim {
    /// 1-based hook id (`index + 1`).
    pub hook_id: i32,
    /// `Some(old)` when a replacement moved the hook off INTID `old`.
    pub moved_from: Option<u32>,
}

/// The fixed-size IRQ hook table (I6): a linear scan over 16 slots.
pub struct HookTable {
    hooks: [IrqHook; NR_IRQ_HOOKS],
}

impl Default for HookTable {
    fn default() -> Self {
        Self::new()
    }
}

impl HookTable {
    pub const fn new() -> Self {
        Self {
            hooks: [IrqHook::FREE; NR_IRQ_HOOKS],
        }
    }

    /// Claim `intid` for `owner` under `notify_id` (I6). A hook with the same
    /// `(owner, notify_id)` is replaced in place; any other hook on `intid` is
    /// `EBUSY`; otherwise the first free slot, or `ENOSPC`. Nothing changes on
    /// error.
    pub fn set_policy(&mut self, owner: Endpoint, intid: u32, notify_id: u8) -> Result<Claim, i32> {
        let candidate = self
            .hooks
            .iter()
            .position(|h| !h.is_free() && h.owner == owner && h.notify_id == notify_id);
        let busy = self
            .hooks
            .iter()
            .enumerate()
            .any(|(i, h)| !h.is_free() && h.intid == intid && Some(i) != candidate);
        if busy {
            return Err(EBUSY);
        }
        let (idx, moved_from) = match candidate {
            Some(i) => {
                let old = self.hooks[i].intid;
                (i, (old != intid).then_some(old))
            }
            None => (
                self.hooks.iter().position(IrqHook::is_free).ok_or(ENOSPC)?,
                None,
            ),
        };
        self.hooks[idx] = IrqHook {
            owner,
            intid,
            notify_id,
        };
        Ok(Claim {
            hook_id: idx as i32 + 1,
            moved_from,
        })
    }

    fn slot(&self, owner: Endpoint, hook_id: i32) -> Result<usize, i32> {
        if !(1..=NR_IRQ_HOOKS as i32).contains(&hook_id) {
            return Err(EINVAL);
        }
        let idx = (hook_id - 1) as usize;
        let h = &self.hooks[idx];
        if h.is_free() {
            return Err(EINVAL);
        }
        if h.owner != owner {
            return Err(EPERM);
        }
        Ok(idx)
    }

    /// The hook `hook_id` if `owner` holds it. `EINVAL` for an id out of
    /// `1..=NR_IRQ_HOOKS` or a free slot; `EPERM` for someone else's.
    pub fn owned(&self, owner: Endpoint, hook_id: i32) -> Result<IrqHook, i32> {
        self.slot(owner, hook_id).map(|i| self.hooks[i])
    }

    /// Free `owner`'s hook `hook_id`, returning it. Same errors as [`owned`].
    ///
    /// [`owned`]: HookTable::owned
    pub fn remove(&mut self, owner: Endpoint, hook_id: i32) -> Result<IrqHook, i32> {
        let i = self.slot(owner, hook_id)?;
        let h = self.hooks[i];
        self.hooks[i] = IrqHook::FREE;
        Ok(h)
    }

    /// The hook claiming `intid`, if any.
    pub fn by_intid(&self, intid: u32) -> Option<IrqHook> {
        self.hooks
            .iter()
            .find(|h| !h.is_free() && h.intid == intid)
            .copied()
    }

    /// Free every hook of `owner`, calling `mask(intid)` for each. Returns the
    /// number freed.
    pub fn release_owner(&mut self, owner: Endpoint, mut mask: impl FnMut(u32)) -> usize {
        let mut n = 0;
        for h in self.hooks.iter_mut() {
            if !h.is_free() && h.owner == owner {
                mask(h.intid);
                *h = IrqHook::FREE;
                n += 1;
            }
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::callnr::{IRQ_DISABLE, IRQ_ENABLE, IRQ_REENABLE, IRQ_RMPOLICY, IRQ_SETPOLICY};
    use crate::error::{EBUSY, EINVAL, ENOSPC, EPERM};

    extern crate std;
    use std::vec::Vec;

    const N: u32 = 288;

    #[test]
    fn decode_rejects_unknown_request() {
        assert_eq!(decode(5, 33, 0, 0, N), Err(EINVAL));
        assert_eq!(decode(0, 33, 0, 0, N), Err(EINVAL));
    }

    #[test]
    fn decode_fails_closed_before_gic_init() {
        // nr_intids == 0 until the GIC is probed: every vector must be refused.
        assert_eq!(decode(IRQ_SETPOLICY, 33, 0, 0, 0), Err(EINVAL));
    }

    #[test]
    fn decode_rejects_sgi_ppi_and_out_of_range() {
        for v in [0, 27, 31, 288, -1] {
            assert_eq!(decode(IRQ_SETPOLICY, v, 0, 0, N), Err(EINVAL), "vector {v}");
        }
        assert!(decode(IRQ_SETPOLICY, 32, 0, 0, N).is_ok());
        assert!(decode(IRQ_SETPOLICY, 287, 0, 0, N).is_ok());
    }

    #[test]
    fn decode_rejects_any_policy_bit() {
        for p in [IRQ_REENABLE, 0x100, 0x400] {
            assert_eq!(
                decode(IRQ_SETPOLICY, 33, p, 0, N),
                Err(EINVAL),
                "policy {p}"
            );
        }
    }

    #[test]
    fn decode_notify_id_range_is_signed() {
        assert_eq!(decode(IRQ_SETPOLICY, 33, 0, -1, N), Err(EINVAL));
        assert_eq!(decode(IRQ_SETPOLICY, 33, 0, 32, N), Err(EINVAL));
        assert_eq!(
            decode(IRQ_SETPOLICY, 33, 0, 0, N),
            Ok(IrqRequest::SetPolicy {
                vector: 33,
                notify_id: 0
            })
        );
        assert_eq!(
            decode(IRQ_SETPOLICY, 33, 0, 31, N),
            Ok(IrqRequest::SetPolicy {
                vector: 33,
                notify_id: 31
            })
        );
    }

    #[test]
    fn decode_passes_hook_id_through() {
        assert_eq!(
            decode(IRQ_ENABLE, 0, 0, 0, N),
            Ok(IrqRequest::Enable { hook_id: 0 })
        );
        assert_eq!(
            decode(IRQ_DISABLE, 0, 0, -7, N),
            Ok(IrqRequest::Disable { hook_id: -7 })
        );
        assert_eq!(
            decode(IRQ_RMPOLICY, 0, 0, 99, N),
            Ok(IrqRequest::RmPolicy { hook_id: 99 })
        );
    }

    #[test]
    fn claim_is_one_based() {
        let mut t = HookTable::new();
        let c = t.set_policy(10, 33, 0).unwrap();
        assert_eq!(
            c,
            Claim {
                hook_id: 1,
                moved_from: None
            }
        );
    }

    #[test]
    fn second_owner_on_claimed_line_is_ebusy() {
        let mut t = HookTable::new();
        t.set_policy(10, 33, 0).unwrap();
        assert_eq!(t.set_policy(11, 33, 0), Err(EBUSY));
    }

    #[test]
    fn same_owner_second_notify_id_is_ebusy() {
        let mut t = HookTable::new();
        t.set_policy(10, 33, 0).unwrap();
        assert_eq!(t.set_policy(10, 33, 1), Err(EBUSY));
    }

    #[test]
    fn same_owner_same_notify_id_replaces() {
        let mut t = HookTable::new();
        let a = t.set_policy(10, 33, 0).unwrap();
        let b = t.set_policy(10, 33, 0).unwrap();
        assert_eq!(a.hook_id, b.hook_id);
        assert_eq!(b.moved_from, None);
        let c = t.set_policy(10, 40, 0).unwrap();
        assert_eq!(c.hook_id, a.hook_id);
        assert_eq!(c.moved_from, Some(33));
        assert_eq!(t.by_intid(33), None);
        assert_eq!(t.by_intid(40).unwrap().owner, 10);
    }

    #[test]
    fn replace_that_hits_busy_line_leaves_old_hook() {
        let mut t = HookTable::new();
        t.set_policy(10, 33, 0).unwrap();
        t.set_policy(11, 34, 0).unwrap();
        assert_eq!(t.set_policy(10, 34, 0), Err(EBUSY));
        assert_eq!(t.by_intid(33).unwrap().owner, 10);
        assert_eq!(t.by_intid(34).unwrap().owner, 11);
    }

    #[test]
    fn full_table_is_enospc() {
        let mut t = HookTable::new();
        for i in 0..NR_IRQ_HOOKS as u32 {
            t.set_policy(10, 32 + i, (i % 32) as u8).unwrap();
        }
        assert_eq!(t.set_policy(10, 32 + NR_IRQ_HOOKS as u32, 31), Err(ENOSPC));
    }

    #[test]
    fn owned_checks_range_free_and_owner() {
        let mut t = HookTable::new();
        let c = t.set_policy(10, 33, 3).unwrap();
        assert_eq!(t.owned(10, 0), Err(EINVAL));
        assert_eq!(t.owned(10, NR_IRQ_HOOKS as i32 + 1), Err(EINVAL));
        assert_eq!(t.owned(10, 2), Err(EINVAL));
        assert_eq!(t.owned(11, c.hook_id), Err(EPERM));
        let h = t.owned(10, c.hook_id).unwrap();
        assert_eq!((h.owner, h.intid, h.notify_id), (10, 33, 3));
        assert_eq!(t.remove(11, c.hook_id), Err(EPERM));
        assert_eq!(t.remove(10, c.hook_id).unwrap().intid, 33);
        assert_eq!(t.owned(10, c.hook_id), Err(EINVAL));
    }

    #[test]
    fn release_owner_masks_and_frees_only_that_owner() {
        let mut t = HookTable::new();
        t.set_policy(10, 33, 0).unwrap();
        t.set_policy(10, 34, 1).unwrap();
        t.set_policy(11, 35, 0).unwrap();
        let mut masked = Vec::new();
        let n = t.release_owner(10, |i| masked.push(i));
        masked.sort();
        assert_eq!(masked, [33, 34]);
        assert_eq!(n, 2);
        assert_eq!(t.by_intid(33), None);
        assert_eq!(t.by_intid(34), None);
        assert_eq!(t.by_intid(35).unwrap().owner, 11);
    }
}
