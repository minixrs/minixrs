//! Process register frame — saved on entry to EL1 and restored on `eret`.
//!
//! Slice 2.2 defines the layout; slice 2.3 wires up the SVC entry path that
//! actually populates and restores it. The field order matches what an EL0
//! → EL1 trap will need to save (general-purpose registers, then the three
//! special EL1 system registers). Fields are `pub` so the trap stub can
//! access them by name from assembly via `offset_of!` once it exists.

/// Saved register state for a user-mode process, captured at EL1 exception
/// entry. Layout mirrors what `vectors.S` will eventually push on entry to
/// the SVC handler.
#[repr(C, align(16))]
#[derive(Copy, Clone, Debug)]
pub struct ArchRegisterFrame {
    /// `x0`–`x30` general-purpose registers (31 slots).
    pub x: [u64; 31],
    /// `SP_EL0` — user-mode stack pointer at trap entry.
    pub sp_el0: u64,
    /// `ELR_EL1` — return address (the user instruction following the SVC).
    pub elr_el1: u64,
    /// `SPSR_EL1` — saved processor state to restore on `eret`.
    pub spsr_el1: u64,
}

impl ArchRegisterFrame {
    /// All-zero initializer — used for empty process-table slots.
    pub const EMPTY: Self = Self {
        x: [0; 31],
        sp_el0: 0,
        elr_el1: 0,
        spsr_el1: 0,
    };
}
