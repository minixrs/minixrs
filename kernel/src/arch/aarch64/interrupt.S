/*
 * MINIX 4 aarch64 EL0 → EL1 IRQ entry stub.
 *
 * Wired in `vectors.S` vector slot 9 (`el0_64_irq`) — the IRQ vector for an
 * exception taken from a Lower EL using AArch64. Mirrors the save/restore
 * idiom from `el0_64_sync_entry` (slice 2.3): user state goes into the
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

    .section .text.interrupt, "ax", %progbits

/*
 * el0_64_irq_entry — entry stub for vector slot 9 (EL0 AArch64 IRQ).
 *
 * Same calling convention as el0_64_sync_entry: spill the user's x29/x30 to
 * the kernel stack to free two scratch registers, load the frame pointer
 * from TPIDR_EL1 into x29, save x0..x28 directly, then recover x29/x30 from
 * the kernel stack and store into the frame. Finally save SP_EL0, ELR_EL1,
 * SPSR_EL1 and tail-call do_irq.
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

    // do_irq(&mut frame). May update tpidr_el1 if a context switch happens.
    mov     x0, x29
    bl      do_irq

    // Tail-call the shared eret stub from trap.S. It re-reads tpidr_el1, so
    // a reschedule inside do_irq is picked up here automatically.
    b       el1_return_to_user
    .size el0_64_irq_entry, . - el0_64_irq_entry
