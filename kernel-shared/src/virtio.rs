// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! virtio-mmio probe constants and the pure slot-assignment logic (slice 6.2, T4).
//!
//! The kernel reads three registers from each of the platform's transport slots
//! and hands the triples to [`assign`], which decides which slot (if any) each
//! virtio driver gets. The decision is pure and lives here so it is host-tested;
//! the register reads and the platform facts (base, stride, INTID) stay in the
//! kernel's arch module.
//!
//! The rules, in the order [`assign`] applies them to each present device:
//!
//! 1. A 4 KiB page holding more than one *present* device assigns none of them
//!    ([`SlotVerdict::SharedPage`]), whether or not a driver exists for them: a
//!    driver must reach one device's registers and no neighbour's.
//! 2. A lone device with an unknown id is [`SlotVerdict::NoDriver`].
//! 3. A device whose driver has no boot module is [`SlotVerdict::NoModule`] and
//!    does not consume the driver.
//! 4. Among the rest, the lowest slot wins a `DeviceID`; later ones are
//!    [`SlotVerdict::Duplicate`]. The winner is [`SlotVerdict::Assigned`].
//!
//! `Version` is never judged here: refusing a legacy transport is the driver's
//! job.

use crate::com::{VBLK_PROC_NR, VCON_PROC_NR, VINPUT_PROC_NR, VNET_PROC_NR};
use crate::endpoint::ProcNr;

/// `MagicValue` register contents of a virtio-mmio transport ("virt").
pub const VIRTIO_MMIO_MAGIC: u32 = 0x7472_6976;
/// Offset of `MagicValue`.
pub const VIRTIO_MMIO_MAGIC_OFF: usize = 0x000;
/// Offset of `Version`.
pub const VIRTIO_MMIO_VERSION_OFF: usize = 0x004;
/// Offset of `DeviceID`.
pub const VIRTIO_MMIO_DEVICE_ID_OFF: usize = 0x008;

/// virtio `DeviceID` of a network card.
pub const VIRTIO_ID_NET: u32 = 1;
/// virtio `DeviceID` of a block device.
pub const VIRTIO_ID_BLOCK: u32 = 2;
/// virtio `DeviceID` of a console.
pub const VIRTIO_ID_CONSOLE: u32 = 3;
/// virtio `DeviceID` of an input device.
pub const VIRTIO_ID_INPUT: u32 = 18;

/// Number of virtio driver slots in the boot image.
pub const NR_VIRTIO_DRIVERS: usize = 4;

/// One virtio driver slot: its proc number, the device it serves, and its name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VirtioDriver {
    pub nr: ProcNr,
    pub device_id: u32,
    pub name: &'static str,
}

/// Index order is slot order: vblk, vcon, vinput, vnet.
pub const VIRTIO_DRIVERS: [VirtioDriver; NR_VIRTIO_DRIVERS] = [
    VirtioDriver {
        nr: VBLK_PROC_NR,
        device_id: VIRTIO_ID_BLOCK,
        name: "vblk",
    },
    VirtioDriver {
        nr: VCON_PROC_NR,
        device_id: VIRTIO_ID_CONSOLE,
        name: "vcon",
    },
    VirtioDriver {
        nr: VINPUT_PROC_NR,
        device_id: VIRTIO_ID_INPUT,
        name: "vinput",
    },
    VirtioDriver {
        nr: VNET_PROC_NR,
        device_id: VIRTIO_ID_NET,
        name: "vnet",
    },
];

/// Index into [`VIRTIO_DRIVERS`] of the driver with proc number `nr`, if any.
pub const fn virtio_driver_index(nr: ProcNr) -> Option<usize> {
    let mut i = 0;
    while i < NR_VIRTIO_DRIVERS {
        if VIRTIO_DRIVERS[i].nr.get() == nr.get() {
            return Some(i);
        }
        i += 1;
    }
    None
}

const fn driver_for_device(device_id: u32) -> Option<usize> {
    let mut i = 0;
    while i < NR_VIRTIO_DRIVERS {
        if VIRTIO_DRIVERS[i].device_id == device_id {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// The three registers the probe reads from one transport slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SlotProbe {
    pub magic: u32,
    pub version: u32,
    pub device_id: u32,
}

impl SlotProbe {
    /// A device is present when the magic is right and `DeviceID` is non-zero. An
    /// empty QEMU slot reads good magic, `Version` 2 and `DeviceID` 0.
    pub const fn present(&self) -> bool {
        self.magic == VIRTIO_MMIO_MAGIC && self.device_id != 0
    }
}

/// The probe's decision for one slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotVerdict {
    /// No device.
    Empty,
    /// Assigned to `VIRTIO_DRIVERS[d]`.
    Assigned(usize),
    /// Its 4 KiB page holds another present device; nothing in the page is assigned.
    SharedPage,
    /// Alone in its page, but no driver serves this `DeviceID`.
    NoDriver,
    /// Belongs to `VIRTIO_DRIVERS[d]`, which has no module in the boot archive.
    NoModule(usize),
    /// `VIRTIO_DRIVERS[d]` already went to a lower slot.
    Duplicate(usize),
}

/// Decide each slot's fate. `assigned[d]` is the slot index given to
/// `VIRTIO_DRIVERS[d]`. `slots_per_page` is how many consecutive slots share one
/// 4 KiB page.
pub fn assign<const N: usize>(
    slots: &[SlotProbe; N],
    slots_per_page: usize,
    has_module: [bool; NR_VIRTIO_DRIVERS],
) -> ([SlotVerdict; N], [Option<usize>; NR_VIRTIO_DRIVERS]) {
    let mut verdicts = [SlotVerdict::Empty; N];
    let mut assigned = [None; NR_VIRTIO_DRIVERS];
    let per_page = slots_per_page.max(1);

    for (i, slot) in slots.iter().enumerate() {
        if !slot.present() {
            continue;
        }
        let page_start = i - i % per_page;
        let page_end = (page_start + per_page).min(N);
        let present_in_page = slots[page_start..page_end]
            .iter()
            .filter(|s| s.present())
            .count();
        verdicts[i] = if present_in_page > 1 {
            SlotVerdict::SharedPage
        } else {
            match driver_for_device(slot.device_id) {
                None => SlotVerdict::NoDriver,
                Some(d) if !has_module[d] => SlotVerdict::NoModule(d),
                Some(d) if assigned[d].is_some() => SlotVerdict::Duplicate(d),
                Some(d) => {
                    assigned[d] = Some(i);
                    SlotVerdict::Assigned(d)
                }
            }
        };
    }
    (verdicts, assigned)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: usize = 8;
    const EMPTY: SlotProbe = SlotProbe {
        magic: VIRTIO_MMIO_MAGIC,
        version: 2,
        device_id: 0,
    };
    const ALL: [bool; NR_VIRTIO_DRIVERS] = [true; NR_VIRTIO_DRIVERS];

    fn dev(id: u32) -> SlotProbe {
        SlotProbe {
            magic: VIRTIO_MMIO_MAGIC,
            version: 2,
            device_id: id,
        }
    }

    fn bus(devs: &[(usize, u32)]) -> [SlotProbe; 32] {
        let mut b = [EMPTY; 32];
        for &(s, id) in devs {
            b[s] = dev(id);
        }
        b
    }

    #[test]
    fn empty_bus_assigns_nothing() {
        let (v, a) = assign(&bus(&[]), PAGE, ALL);
        assert_eq!(v, [SlotVerdict::Empty; 32]);
        assert_eq!(a, [None; 4]);
        let zero = SlotProbe {
            magic: 0,
            version: 0,
            device_id: 0,
        };
        let (v, a) = assign(&[zero; 32], PAGE, ALL);
        assert_eq!(v, [SlotVerdict::Empty; 32]);
        assert_eq!(a, [None; 4]);
    }

    #[test]
    fn one_block_device_goes_to_vblk() {
        let (v, a) = assign(&bus(&[(0, VIRTIO_ID_BLOCK)]), PAGE, ALL);
        assert_eq!(v[0], SlotVerdict::Assigned(0));
        assert_eq!(a, [Some(0), None, None, None]);
    }

    #[test]
    fn bad_magic_is_empty_even_with_an_id() {
        let mut b = bus(&[]);
        b[3] = SlotProbe {
            magic: 0x1234,
            version: 2,
            device_id: VIRTIO_ID_BLOCK,
        };
        let (v, a) = assign(&b, PAGE, ALL);
        assert_eq!(v[3], SlotVerdict::Empty);
        assert_eq!(a, [None; 4]);
    }

    #[test]
    fn two_devices_in_one_page_are_both_refused() {
        let (v, a) = assign(&bus(&[(0, 2), (1, 3)]), PAGE, ALL);
        assert_eq!(v[0], SlotVerdict::SharedPage);
        assert_eq!(v[1], SlotVerdict::SharedPage);
        assert_eq!(a, [None; 4]);
    }

    #[test]
    fn a_driverless_neighbour_also_refuses_the_page() {
        let (v, a) = assign(&bus(&[(0, 2), (7, 4)]), PAGE, ALL);
        assert_eq!(v[0], SlotVerdict::SharedPage);
        assert_eq!(v[7], SlotVerdict::SharedPage);
        assert_eq!(a, [None; 4]);
    }

    #[test]
    fn devices_in_different_pages_are_independent() {
        let (v, a) = assign(&bus(&[(0, 2), (8, 3)]), PAGE, ALL);
        assert_eq!(v[0], SlotVerdict::Assigned(0));
        assert_eq!(v[8], SlotVerdict::Assigned(1));
        assert_eq!(a, [Some(0), Some(8), None, None]);
    }

    #[test]
    fn the_lowest_slot_wins_a_device_id() {
        let (v, a) = assign(&bus(&[(0, 2), (8, 2)]), PAGE, ALL);
        assert_eq!(v[0], SlotVerdict::Assigned(0));
        assert_eq!(v[8], SlotVerdict::Duplicate(0));
        assert_eq!(a[0], Some(0));
    }

    #[test]
    fn a_refused_device_does_not_block_a_later_duplicate() {
        let (v, a) = assign(&bus(&[(0, 2), (1, 2), (8, 2)]), PAGE, ALL);
        assert_eq!(v[0], SlotVerdict::SharedPage);
        assert_eq!(v[1], SlotVerdict::SharedPage);
        assert_eq!(v[8], SlotVerdict::Assigned(0));
        assert_eq!(a[0], Some(8));
    }

    #[test]
    fn no_module_means_no_assignment() {
        let mut m = ALL;
        m[1] = false;
        let (v, a) = assign(&bus(&[(8, VIRTIO_ID_CONSOLE)]), PAGE, m);
        assert_eq!(v[8], SlotVerdict::NoModule(1));
        assert_eq!(a, [None; 4]);
    }

    #[test]
    fn no_module_does_not_consume_the_driver() {
        // Same id twice, no module: both NoModule, neither Duplicate.
        let mut m = ALL;
        m[0] = false;
        let (v, a) = assign(&bus(&[(0, 2), (8, 2)]), PAGE, m);
        assert_eq!(v[0], SlotVerdict::NoModule(0));
        assert_eq!(v[8], SlotVerdict::NoModule(0));
        assert_eq!(a, [None; 4]);
    }

    #[test]
    fn a_no_module_device_sharing_a_page_still_refuses_both() {
        // Rule 1 (shared page) outranks rule 3 (no module): the moduled device and
        // its module-less page-mate are both SharedPage, and nothing is assigned.
        let mut m = ALL;
        m[1] = false; // no module for DeviceID 3's driver
        let (v, a) = assign(&bus(&[(0, 2), (1, 3)]), PAGE, m);
        assert_eq!(v[0], SlotVerdict::SharedPage);
        assert_eq!(v[1], SlotVerdict::SharedPage);
        assert_eq!(a, [None; 4]);
    }

    #[test]
    fn an_unknown_device_alone_in_its_page_is_no_driver() {
        let (v, a) = assign(&bus(&[(16, 4)]), PAGE, ALL);
        assert_eq!(v[16], SlotVerdict::NoDriver);
        assert_eq!(a, [None; 4]);
    }

    #[test]
    fn legacy_version_does_not_change_the_verdict() {
        let mut b = bus(&[]);
        b[0] = SlotProbe {
            magic: VIRTIO_MMIO_MAGIC,
            version: 1,
            device_id: VIRTIO_ID_BLOCK,
        };
        let (v, _) = assign(&b, PAGE, ALL);
        assert_eq!(v[0], SlotVerdict::Assigned(0));
    }

    #[test]
    fn driver_table_matches_the_com_slots() {
        assert_eq!(virtio_driver_index(VBLK_PROC_NR), Some(0));
        assert_eq!(virtio_driver_index(VCON_PROC_NR), Some(1));
        assert_eq!(virtio_driver_index(VINPUT_PROC_NR), Some(2));
        assert_eq!(virtio_driver_index(VNET_PROC_NR), Some(3));
        assert_eq!(virtio_driver_index(ProcNr::new(0)), None);
    }
}
