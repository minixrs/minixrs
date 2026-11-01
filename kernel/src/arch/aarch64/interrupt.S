/* SPDX-License-Identifier: BSD-3-Clause */
/* Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors */
/*
 * minix.rs aarch64 EL0 → EL1 IRQ entry stub.
 *
 * Wired in `vectors.S` vector slot 9 (`el0_64_irq`) — the IRQ vector for an
 * exception taken from a Lower EL using AArch64. Mirrors the save/restore
 * idiom from `el0_64_sync_entry` (slice 2.3): the whole user state — GPRs,
 * return state, TPIDR_EL0 and FP/SIMD — goes into the
 * `tpidr_el1`-pointed `ArchRegisterFrame`, the Rust dispatcher runs with
 * the frame pointer in x0, then the tail returns to whichever EL0 context
 * `tpidr_el1` still points at — possibly a *different* proc if `do_irq`
 * rescheduled.
 *
 * On entry:
 *   - EL1h, SP = SP_EL1 (kernel stack).
 *   - All EL0 user regs x0..x30 hold caller values, unsaved.
 *   - TPIDR_EL1 = &mut CURRENT_PROC->regs.
 *   - PSTATE.DAIF: I bit set (IRQs masked while we handle this one).
 *   - ELR_EL1 / SPSR_EL1 describe the interrupted EL0 context.
 *
 * Field offsets must match `kernel/src/arch/aarch64/context.rs`. See the
 * matching .equ block in `trap.S`; we duplicate them rather than #include
 * to keep each .S file self-contained for the cc-crate-style build.
 */

    .equ REGS_X_OFFSET,      0
    .equ REGS_SP_EL0_OFFSET, 248
    .equ REGS_ELR_OFFSET,    256
    .equ REGS_SPSR_OFFSET,   264
    .equ REGS_TPIDR_EL0_OFFSET, 272
    .equ REGS_FPCR_OFFSET,   280
    .equ REGS_FPSR_OFFSET,   288
    .equ REGS_Q_OFFSET,      304

    .section .text.interrupt, "ax", %progbits

/*
 * el0_64_irq_entry — entry stub for vector slot 9 (EL0 AArch64 IRQ).
 *
 * Same calling convention as el0_64_sync_entry: spill the user's x29/x30 to
 * the kernel stack to free two scratch registers, load the frame pointer
 * from TPIDR_EL1 into x29, save x0..x28 directly, then recover x29/x30 from
 * the kernel stack and store into the frame. Then save SP_EL0, ELR_EL1,
 * SPSR_EL1, TPIDR_EL0 (spec M18) and FPCR, FPSR and q0..q31 (spec M19) --
 * the same set, in the same order, as the sync entry -- call do_irq, and
 * branch to el1_return_to_user, which restores all of it.
 */
    .global el0_64_irq_entry
    .type el0_64_irq_entry, %function
el0_64_irq_entry:
    // Free x29/x30 for scratch use.
    stp     x29, x30, [sp, #-16]!

    // x29 = &frame.
    mrs     x29, tpidr_el1

    // Save x0..x28 into the frame.
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

    // Pop the user's x29/x30 off the kernel stack and store into the frame.
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

    // do_irq(&mut frame). May update tpidr_el1 if a context switch happens.
    mov     x0, x29
    bl      do_irq

    // Tail-call the shared eret stub from trap.S. It re-reads tpidr_el1, so
    // a reschedule inside do_irq is picked up here automatically.
    b       el1_return_to_user
    .size el0_64_irq_entry, . - el0_64_irq_entry
