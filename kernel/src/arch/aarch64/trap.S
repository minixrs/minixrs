/* SPDX-License-Identifier: BSD-3-Clause */
/* Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors */
/*
 * minix.rs aarch64 EL0 → EL1 trap entry + eret stub.
 *
 * Used for:
 *   - EL0 synchronous exceptions (vector slot 8). `vectors.S` branches here
 *     unconditionally for that slot; this stub saves the whole user context
 *     into CURRENT_PROC->regs -- x0..x30, SP_EL0, ELR_EL1, SPSR_EL1, the
 *     TPIDR_EL0 thread pointer (spec M18) and the FP/SIMD state, FPCR, FPSR and
 *     q0..q31 (spec M19) -- decodes ESR_EL1, and dispatches either `do_ipc`
 *     (EC = SVC64) or `do_page_fault` (every other EC).
 *   - Returning to EL0. `switch_to_user`, the SVC and page-fault tails here,
 *     and the IRQ tail in `interrupt.S` all branch to `el1_return_to_user`,
 *     which restores the same set from the `tpidr_el1`-pointed frame and erets.
 *
 * Calling convention:
 *   - TPIDR_EL1 always holds `&mut CURRENT_PROC->regs` while we are running
 *     a user context. `switch_to_user` sets it before the first eret; we
 *     never clear it.
 *   - Field offsets below must match `kernel/src/arch/aarch64/context.rs`.
 *     The .equ block is mirrored from the Rust REGS_*_OFFSET constants; the
 *     `const _: () = assert!(size_of::<ArchRegisterFrame> == 816)` and `offset_of!`
 *     asserts in context.rs catch drift.
 */

    .equ REGS_X_OFFSET,      0
    .equ REGS_SP_EL0_OFFSET, 248
    .equ REGS_ELR_OFFSET,    256
    .equ REGS_SPSR_OFFSET,   264
    .equ REGS_TPIDR_EL0_OFFSET, 272
    .equ REGS_FPCR_OFFSET,   280
    .equ REGS_FPSR_OFFSET,   288
    .equ REGS_Q_OFFSET,      304

    .equ ESR_EC_SVC64,       0x15

    .section .text.trap, "ax", %progbits

/*
 * el0_64_sync_entry — entry stub for vector slot 8 (EL0 AArch64 sync).
 *
 * On entry:
 *   - EL1h, SP = SP_EL1 (kernel stack) is live and usable.
 *   - All EL0 user regs x0..x30 hold caller values, unsaved.
 *   - TPIDR_EL1 = &mut CURRENT_PROC->regs.
 *   - ESR/ELR/SPSR_EL1 describe the trap.
 *
 * We spill x29/x30 to the kernel stack to free two scratch registers
 * (x29 will be the frame pointer; x30 is reloaded from the stack into a
 * scratch reg before being written into the frame). All other user regs
 * are saved directly into the frame via stp/str, then SP_EL0 / ELR_EL1 /
 * SPSR_EL1 follow. EC is decoded from ESR_EL1; SVC64 → do_ipc, anything
 * else → el0_sync_unexpected.
 */
    .global el0_64_sync_entry
    .type el0_64_sync_entry, %function
el0_64_sync_entry:
    // Free x29/x30 for use as scratch (frame pointer + temporary).
    stp     x29, x30, [sp, #-16]!

    // x29 = &frame.
    mrs     x29, tpidr_el1

    // Save x0..x28 into the frame. x29 (frame ptr) cannot be its own base
    // and destination simultaneously, so we save the user's x29/x30 below.
    stp     x0,  x1,  [x29, #REGS_X_OFFSET + 0x00]
    stp     x2,  x3,  [x29, #REGS_X_OFFSET + 0x10]
    stp     x4,  x5,  [x29, #REGS_X_OFFSET + 0x20]
    stp     x6,  x7,  [x29, #REGS_X_OFFSET + 0x30]
    stp     x8,  x9,  [x29, #REGS_X_OFFSET + 0x40]
    stp     x10, x11, [x29, #REGS_X_OFFSET + 0x50]
    stp     x12, x13, [x29, #REGS_X_OFFSET + 0x60]
    stp     x14, x15, [x29, #REGS_X_OFFSET + 0x70]
    stp     x16, x17, [x29, #REGS_X_OFFSET + 0x80]
    stp     x18, x19, [x29, #REGS_X_OFFSET + 0x90]
    stp     x20, x21, [x29, #REGS_X_OFFSET + 0xA0]
    stp     x22, x23, [x29, #REGS_X_OFFSET + 0xB0]
    stp     x24, x25, [x29, #REGS_X_OFFSET + 0xC0]
    stp     x26, x27, [x29, #REGS_X_OFFSET + 0xD0]
    str     x28,      [x29, #REGS_X_OFFSET + 0xE0]

    // Pop the user's x29 and x30 off the kernel stack into scratch regs
    // and store them into the frame.
    ldp     x16, x17, [sp], #16
    stp     x16, x17, [x29, #REGS_X_OFFSET + 0xE8]

    // Save SP_EL0, ELR_EL1, SPSR_EL1.
    mrs     x9,  sp_el0
    mrs     x10, elr_el1
    mrs     x11, spsr_el1
    stp     x9,  x10, [x29, #REGS_SP_EL0_OFFSET]
    str     x11,      [x29, #REGS_SPSR_OFFSET]

    // Save TPIDR_EL0 (the user thread pointer). x13 is free: every GPR is
    // already in the frame.
    mrs     x13, tpidr_el0
    str     x13,      [x29, #REGS_TPIDR_EL0_OFFSET]

    // Save FPCR/FPSR and q0..q31 (spec M19) before any Rust runs: the kernel
    // target enables NEON, so kernel code itself clobbers `q` registers.
    // x13/x14 are free for the same reason as above. These EL1 FP accesses
    // do not trap: the kernel never writes CPACR_EL1, and FPEN arrives as
    // 0b11 from UEFI/Limine — the only encoding under which both the kernel's
    // own NEON code (EL1) and the servers' (EL0) run, which they do. The
    // QEMU virt machine has no EL2/EL3, so no CPTR_EL2/EL3 trap applies.
    // REGS_Q_OFFSET is 16-aligned and the last pair's +0x1E0 stays within
    // stp's +1008 reach (both asserted in context.rs).
    mrs     x13, fpcr
    mrs     x14, fpsr
    stp     x13, x14, [x29, #REGS_FPCR_OFFSET]
    stp     q0,  q1,  [x29, #REGS_Q_OFFSET + 0x000]
    stp     q2,  q3,  [x29, #REGS_Q_OFFSET + 0x020]
    stp     q4,  q5,  [x29, #REGS_Q_OFFSET + 0x040]
    stp     q6,  q7,  [x29, #REGS_Q_OFFSET + 0x060]
    stp     q8,  q9,  [x29, #REGS_Q_OFFSET + 0x080]
    stp     q10, q11, [x29, #REGS_Q_OFFSET + 0x0A0]
    stp     q12, q13, [x29, #REGS_Q_OFFSET + 0x0C0]
    stp     q14, q15, [x29, #REGS_Q_OFFSET + 0x0E0]
    stp     q16, q17, [x29, #REGS_Q_OFFSET + 0x100]
    stp     q18, q19, [x29, #REGS_Q_OFFSET + 0x120]
    stp     q20, q21, [x29, #REGS_Q_OFFSET + 0x140]
    stp     q22, q23, [x29, #REGS_Q_OFFSET + 0x160]
    stp     q24, q25, [x29, #REGS_Q_OFFSET + 0x180]
    stp     q26, q27, [x29, #REGS_Q_OFFSET + 0x1A0]
    stp     q28, q29, [x29, #REGS_Q_OFFSET + 0x1C0]
    stp     q30, q31, [x29, #REGS_Q_OFFSET + 0x1E0]

    // Decode ESR_EL1 EC and dispatch.
    mrs     x12, esr_el1
    lsr     x13, x12, #26
    cmp     x13, #ESR_EC_SVC64
    b.ne    1f

    // SVC64 — call do_ipc(&mut frame), then run the slice-2.5 SVC tail
    // (schedule the next runnable proc + flush MF_DELIVERMSG into its
    // user buffer). The tail may transparently swap TPIDR_EL1 to a
    // different proc; the subsequent `el1_return_to_user` reads the new
    // TPIDR_EL1 and restores that frame.
    mov     x0, x29
    bl      do_ipc
    bl      el1_svc_tail
    b       el1_return_to_user

1:  // Non-SVC EL0 sync exception — route through the page-fault handler.
    // It resolves heap-window faults and returns; unrecoverable faults
    // panic inside (el0_sync_unexpected is -> !). The tail mirrors the
    // SVC path: schedule the next runnable proc (the just-unblocked
    // faulting proc, or another), then eret into it.
    // Args: do_page_fault(esr, elr, far).
    mov     x0, x12              // esr_el1
    mrs     x1, elr_el1
    mrs     x2, far_el1
    bl      do_page_fault
    bl      el1_svc_tail
    b       el1_return_to_user
    .size el0_64_sync_entry, . - el0_64_sync_entry

/*
 * el1_return_to_user — restore TPIDR_EL1-pointed frame and eret.
 *
 * Used by:
 *   - proc::sched::switch_to_user (first entry into EL0).
 *   - SVC tail (after do_ipc returns).
 *   - Page-fault tail (after do_page_fault returns).
 *   - IRQ tail in `interrupt.S` (after do_irq returns).
 *
 * Restores the system registers, TPIDR_EL0, FPCR/FPSR and q0..q31, then the
 * GPRs. Every path leaves TPIDR_EL1 pointing at the frame -- possibly a
 * different proc's than the one that trapped, if the tail rescheduled. x9
 * stays the base throughout; x8 and x9 are restored last (otherwise loading
 * into x9 would clobber the base).
 */
    .global el1_return_to_user
    .type el1_return_to_user, %function
el1_return_to_user:
    mrs     x9, tpidr_el1

    // System registers first.
    ldp     x10, x11, [x9, #REGS_SP_EL0_OFFSET]   // sp_el0, elr_el1
    ldr     x12,      [x9, #REGS_SPSR_OFFSET]
    msr     sp_el0,   x10
    msr     elr_el1,  x11
    msr     spsr_el1, x12
    ldr     x13,      [x9, #REGS_TPIDR_EL0_OFFSET]
    msr     tpidr_el0, x13

    // FP/SIMD state (spec M19). Nothing but this stub runs between here and
    // eret, so the reloaded q registers reach EL0 untouched. x13/x14 are
    // reloaded by the GPR ldp below.
    ldp     x13, x14, [x9, #REGS_FPCR_OFFSET]
    msr     fpcr,     x13
    msr     fpsr,     x14
    ldp     q0,  q1,  [x9, #REGS_Q_OFFSET + 0x000]
    ldp     q2,  q3,  [x9, #REGS_Q_OFFSET + 0x020]
    ldp     q4,  q5,  [x9, #REGS_Q_OFFSET + 0x040]
    ldp     q6,  q7,  [x9, #REGS_Q_OFFSET + 0x060]
    ldp     q8,  q9,  [x9, #REGS_Q_OFFSET + 0x080]
    ldp     q10, q11, [x9, #REGS_Q_OFFSET + 0x0A0]
    ldp     q12, q13, [x9, #REGS_Q_OFFSET + 0x0C0]
    ldp     q14, q15, [x9, #REGS_Q_OFFSET + 0x0E0]
    ldp     q16, q17, [x9, #REGS_Q_OFFSET + 0x100]
    ldp     q18, q19, [x9, #REGS_Q_OFFSET + 0x120]
    ldp     q20, q21, [x9, #REGS_Q_OFFSET + 0x140]
    ldp     q22, q23, [x9, #REGS_Q_OFFSET + 0x160]
    ldp     q24, q25, [x9, #REGS_Q_OFFSET + 0x180]
    ldp     q26, q27, [x9, #REGS_Q_OFFSET + 0x1A0]
    ldp     q28, q29, [x9, #REGS_Q_OFFSET + 0x1C0]
    ldp     q30, q31, [x9, #REGS_Q_OFFSET + 0x1E0]

    // GPRs. Skip x8/x9 here — they'd clobber the base mid-restore.
    ldp     x0,  x1,  [x9, #REGS_X_OFFSET + 0x00]
    ldp     x2,  x3,  [x9, #REGS_X_OFFSET + 0x10]
    ldp     x4,  x5,  [x9, #REGS_X_OFFSET + 0x20]
    ldp     x6,  x7,  [x9, #REGS_X_OFFSET + 0x30]
    ldp     x10, x11, [x9, #REGS_X_OFFSET + 0x50]
    ldp     x12, x13, [x9, #REGS_X_OFFSET + 0x60]
    ldp     x14, x15, [x9, #REGS_X_OFFSET + 0x70]
    ldp     x16, x17, [x9, #REGS_X_OFFSET + 0x80]
    ldp     x18, x19, [x9, #REGS_X_OFFSET + 0x90]
    ldp     x20, x21, [x9, #REGS_X_OFFSET + 0xA0]
    ldp     x22, x23, [x9, #REGS_X_OFFSET + 0xB0]
    ldp     x24, x25, [x9, #REGS_X_OFFSET + 0xC0]
    ldp     x26, x27, [x9, #REGS_X_OFFSET + 0xD0]
    ldp     x28, x29, [x9, #REGS_X_OFFSET + 0xE0]
    ldr     x30,      [x9, #REGS_X_OFFSET + 0xF0]

    // Finally x8 and x9 — load x9 last so the base survives until needed.
    ldr     x8,       [x9, #REGS_X_OFFSET + 0x40]
    ldr     x9,       [x9, #REGS_X_OFFSET + 0x48]

    eret
    .size el1_return_to_user, . - el1_return_to_user
