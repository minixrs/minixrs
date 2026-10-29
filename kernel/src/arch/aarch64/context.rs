// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
// REGS_*_OFFSET constants are the canonical source for the mirrored `.equ`
// directives in trap.S and interrupt.S; they're consumed by assembly, not by
// Rust callers, so the dead-code lint would otherwise fire. The `offset_of!`,
// size and align asserts at the bottom of this file are the active guards.
#![allow(dead_code)]

//! Process register frame — saved on entry to EL1 and restored on `eret`.
//!
//! Slice 2.2 defined the layout; slice 2.3 wires up the SVC entry path that
//! actually populates and restores it. The field order matches what an EL0
//! → EL1 trap saves: the general-purpose registers, SP_EL0 and the two EL1
//! system registers that carry the return state, then TPIDR_EL0 (spec M18)
//! and the FP/SIMD state, FPCR, FPSR and q0–q31 (spec M19). Field offsets
//! are mirrored into `trap.S` and `interrupt.S` via `.equ` directives. Each
//! mirrored offset has its own `offset_of!` assert below, so a field moving
//! is caught wherever it sits. The `size_of` assert pins only the total: it
//! fails when a field is added or resized — the prompt to update the stubs —
//! but cannot say which offset moved.

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
    /// `TPIDR_EL0` — the user thread pointer (musl's `__set_thread_area` is
    /// `msr tpidr_el0`). Saved by both entry stubs, restored by
    /// `el1_return_to_user`; without it every C process shares one thread
    /// pointer (spec M18).
    pub tpidr_el0: u64,
    /// `FPCR` — the floating-point control register (rounding mode, flush
    /// to zero, default NaN). Zero, the architectural reset value, for a
    /// fresh image. Saved by both entry stubs, restored by
    /// `el1_return_to_user` (spec M19).
    pub fpcr: u64,
    /// `FPSR` — the floating-point status register (cumulative exception
    /// flags, saturation). Saved and restored alongside `fpcr`.
    pub fpsr: u64,
    /// `q0`–`q31` — the full 128-bit FP/SIMD register file. Saved eagerly by
    /// both entry stubs before any kernel Rust runs (the kernel target
    /// enables NEON, so kernel code itself uses `q` registers) and reloaded
    /// by `el1_return_to_user` just before `eret`. Without it every EL0
    /// process's vector registers are whatever the last runner left
    /// (spec M19). `u128` is 16-aligned, so with the frame's `align(16)`
    /// every `q` slot sits at its natural 16-byte alignment.
    pub q: [u128; 32],
}

impl ArchRegisterFrame {
    /// All-zero initializer — used for empty process-table slots.
    pub const EMPTY: Self = Self {
        x: [0; 31],
        sp_el0: 0,
        elr_el1: 0,
        spsr_el1: 0,
        tpidr_el0: 0,
        fpcr: 0,
        fpsr: 0,
        q: [0; 32],
    };
}

// ----- Offsets mirrored into trap.S and interrupt.S -------------------------
//
// trap.S and interrupt.S each declare matching `.equ REGS_*_OFFSET, …` lines.
// If you change the layout, update all three. The asserts below catch drift
// on the Rust side only — a field's `offset_of!` against its constant, the
// `size_of` against the total — and nothing checks the `.equ` copies, so the
// stubs must be edited by hand to match.

/// Offset of `x[0]` within [`ArchRegisterFrame`].
pub const REGS_X_OFFSET: usize = 0;
/// Offset of `sp_el0` within [`ArchRegisterFrame`].
pub const REGS_SP_EL0_OFFSET: usize = 31 * 8;
/// Offset of `elr_el1` within [`ArchRegisterFrame`].
pub const REGS_ELR_OFFSET: usize = 31 * 8 + 8;
/// Offset of `spsr_el1` within [`ArchRegisterFrame`].
pub const REGS_SPSR_OFFSET: usize = 31 * 8 + 16;
/// Offset of `tpidr_el0` within [`ArchRegisterFrame`].
pub const REGS_TPIDR_EL0_OFFSET: usize = 31 * 8 + 24;
/// Offset of `fpcr` within [`ArchRegisterFrame`].
pub const REGS_FPCR_OFFSET: usize = 31 * 8 + 32;
/// Offset of `fpsr` within [`ArchRegisterFrame`].
pub const REGS_FPSR_OFFSET: usize = 31 * 8 + 40;
/// Offset of `q[0]` within [`ArchRegisterFrame`]: `fpsr` ends at 296, and
/// `u128`'s 16-byte alignment rounds that up to 304 (8 bytes of padding).
pub const REGS_Q_OFFSET: usize = 304;

// 37 u64 slots = 296 bytes, padded to 304, + 32 * 16 bytes of `q` = 816 (a
// multiple of the 16-byte alignment, so no tail padding).
const _: () = assert!(core::mem::size_of::<ArchRegisterFrame>() == 816);
const _: () = assert!(core::mem::offset_of!(ArchRegisterFrame, x) == REGS_X_OFFSET);
const _: () = assert!(core::mem::offset_of!(ArchRegisterFrame, sp_el0) == REGS_SP_EL0_OFFSET);
const _: () = assert!(core::mem::offset_of!(ArchRegisterFrame, elr_el1) == REGS_ELR_OFFSET);
const _: () = assert!(core::mem::offset_of!(ArchRegisterFrame, spsr_el1) == REGS_SPSR_OFFSET);
const _: () = assert!(core::mem::offset_of!(ArchRegisterFrame, tpidr_el0) == REGS_TPIDR_EL0_OFFSET);
const _: () = assert!(core::mem::offset_of!(ArchRegisterFrame, fpcr) == REGS_FPCR_OFFSET);
const _: () = assert!(core::mem::offset_of!(ArchRegisterFrame, fpsr) == REGS_FPSR_OFFSET);
const _: () = assert!(core::mem::offset_of!(ArchRegisterFrame, q) == REGS_Q_OFFSET);
// The `q` save area must be 16-aligned, so every `q` slot the stubs' `stp`/
// `ldp` pairs touch is naturally aligned — what SCTLR_EL1.A strict alignment
// checking would demand, should it ever be turned on.
const _: () = assert!(REGS_Q_OFFSET.is_multiple_of(16));
// `stp`/`ldp` of `q` pairs take a signed 7-bit immediate scaled by 16 (max
// +1008), so the last pair (q30/q31) must stay reachable from the frame base.
const _: () = assert!(REGS_Q_OFFSET + 30 * 16 <= 1008);
// `stp`/`ldp` of the x-register pair (fpcr, fpsr) reach at most +504.
const _: () = assert!(REGS_FPCR_OFFSET + 8 == REGS_FPSR_OFFSET && REGS_FPCR_OFFSET <= 504);
const _: () = assert!(core::mem::align_of::<ArchRegisterFrame>() == 16);
