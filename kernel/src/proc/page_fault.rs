// `PageFaultState`'s fields are write-only in slice 3.2 (the handler records
// them); slice 3.3's `SYS_VMCTL(VMCTL_GET_PAGEFAULT)` is the first reader.
#![allow(dead_code)]

//! Page-fault bookkeeping carried on each [`Proc`](super::Proc).
//!
//! Slice 3.2 introduces the on-demand paging mechanism: an EL0 translation
//! or permission fault blocks the faulting proc on `RTS_PAGEFAULT`, stashes
//! the fault details in [`PageFaultState`], and (for now) lets the kernel
//! resolve heap-window faults inline. These types are arch-neutral on
//! purpose — slice 3.3's `SYS_VMCTL(VMCTL_GET_PAGEFAULT)` reads
//! [`PageFaultState`] back out to the (eventual) VM server, and slice 3.5
//! moves [`HeapWindow`] ownership into VM's per-proc region table.
//!
//! The arch entry point that fills these in lives in
//! `arch::aarch64::exception::do_page_fault`; this module is just the data.

/// Fault was a write access (aarch64 data abort with `ISS.WnR == 1`).
pub const PFF_WRITE: u32 = 1 << 0;
/// Fault was an instruction fetch (aarch64 `EC == 0x20`).
pub const PFF_INSTR: u32 = 1 << 1;
/// Fault was a permission fault (FSC `0x0D..=0x0F`) rather than a
/// translation fault. A permission fault on an already-mapped page means
/// the PTE exists but the access mode is disallowed.
pub const PFF_PERMISSION: u32 = 1 << 2;

/// Recorded details of the fault a proc is currently blocked on.
///
/// Only meaningful while the owning proc has `RTS_PAGEFAULT` set; cleared
/// back to [`PageFaultState::EMPTY`] once the fault is resolved.
#[derive(Copy, Clone)]
pub struct PageFaultState {
    /// Faulting virtual address (aarch64 `FAR_EL1`).
    pub addr: u64,
    /// Classification bits: `PFF_WRITE | PFF_INSTR | PFF_PERMISSION`.
    pub flags: u32,
    /// Instruction pointer at the fault (aarch64 `ELR_EL1`).
    pub ip: u64,
}

impl PageFaultState {
    pub const EMPTY: Self = Self { addr: 0, flags: 0, ip: 0 };
}

/// Half-open virtual-address range `[start, end)` the kernel will resolve
/// on-demand for a proc in slice 3.2 (the kernel-as-VM stand-in). An empty
/// window (`end == 0`) means "no kernel-resolved heap" — faults there fall
/// through to the halt path. Slice 3.4 hands this responsibility to the VM
/// server and the window becomes one of VM's tracked regions.
#[derive(Copy, Clone)]
pub struct HeapWindow {
    pub start: u64,
    pub end: u64,
}

impl HeapWindow {
    pub const EMPTY: Self = Self { start: 0, end: 0 };

    /// True iff `addr` lies within a non-empty window.
    pub fn contains(&self, addr: u64) -> bool {
        self.end != 0 && addr >= self.start && addr < self.end
    }
}
