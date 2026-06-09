// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
// REGS_*_OFFSET constants are the canonical source for the mirrored `.equ`
// directives in trap.S; they're consumed by assembly, not by Rust callers,
// so the dead-code lint would otherwise fire. The size + align asserts at
// the bottom of this file are the active guards.
#![allow(dead_code)]

//! Process register frame — saved on entry to EL1 and restored on `eret`.
//!
//! Slice 2.2 defined the layout; slice 2.3 wires up the SVC entry path that
//! actually populates and restores it. The field order matches what an EL0
//! → EL1 trap saves (general-purpose registers, then SP_EL0 and the two EL1
//! system registers that carry the return state). Field offsets are mirrored
//! into `trap.S` via `.equ` directives; the `assert!(size_of == 272)` canary
//! below catches accidental drift.

/// Saved register state for a user-mode process, captured at EL1 exception
/// entry. Layout matches the load/store sequence in `trap.S`.
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

// ----- Offsets mirrored into trap.S -----------------------------------------
//
// trap.S declares matching `.equ REGS_*_OFFSET, …` lines. If you change the
// layout, update both sides; the `size_of` assert below catches drift in
// the trailing fields.

/// Offset of `x[0]` within [`ArchRegisterFrame`].
pub const REGS_X_OFFSET: usize = 0;
/// Offset of `sp_el0` within [`ArchRegisterFrame`].
pub const REGS_SP_EL0_OFFSET: usize = 31 * 8;
/// Offset of `elr_el1` within [`ArchRegisterFrame`].
pub const REGS_ELR_OFFSET: usize = 31 * 8 + 8;
/// Offset of `spsr_el1` within [`ArchRegisterFrame`].
pub const REGS_SPSR_OFFSET: usize = 31 * 8 + 16;

const _: () = assert!(core::mem::size_of::<ArchRegisterFrame>() == 32 * 8 + 16);
const _: () = assert!(core::mem::align_of::<ArchRegisterFrame>() == 16);
