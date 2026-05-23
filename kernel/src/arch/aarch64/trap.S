/*
 * MINIX 4 aarch64 EL0 → EL1 trap entry + eret stub.
 *
 * Used for:
 *   - EL0 SVC traps (vector slot 8). `vectors.S` branches here unconditionally
 *     for that slot; this stub saves user state into CURRENT_PROC->regs,
 *     decodes ESR_EL1, and dispatches either `do_ipc` (EC = SVC64) or the
 *     unexpected-sync panic helper.
 *   - Returning to EL0. `switch_to_user` and the SVC tail both branch to
 *     `el1_return_to_user`, which restores `tpidr_el1`-pointed regs and erets.
 *
 * Calling convention:
 *   - TPIDR_EL1 always holds `&mut CURRENT_PROC->regs` while we are running
 *     a user context. `switch_to_user` sets it before the first eret; we
 *     never clear it.
 *   - Field offsets below must match `kernel/src/arch/aarch64/context.rs`.
 *     The .equ block is mirrored from the Rust REGS_*_OFFSET constants; the
 *     `const _: () = assert!(size_of::<ArchRegisterFrame> == 272)` canary in
 *     context.rs catches drift in the trailing fields.
 */

    .equ REGS_X_OFFSET,      0
    .equ REGS_SP_EL0_OFFSET, 248
    .equ REGS_ELR_OFFSET,    256
    .equ REGS_SPSR_OFFSET,   264

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

    // Decode ESR_EL1 EC and dispatch.
    mrs     x12, esr_el1
    lsr     x13, x12, #26
    cmp     x13, #ESR_EC_SVC64
    b.ne    1f

    // SVC64 — call do_ipc(&mut frame).
    mov     x0, x29
    bl      do_ipc
    b       el1_return_to_user

1:  // Non-SVC EL0 sync exception — diagnose and panic.
    // Args: el0_sync_unexpected(esr, elr, far).
    mov     x0, x12              // esr_el1
    mrs     x1, elr_el1
    mrs     x2, far_el1
    bl      el0_sync_unexpected
    // el0_sync_unexpected is -> !; if it ever returns, halt.
2:  wfe
    b       2b
    .size el0_64_sync_entry, . - el0_64_sync_entry

/*
 * el1_return_to_user — restore TPIDR_EL1-pointed frame and eret.
 *
 * Used by:
 *   - proc::sched::switch_to_user (first entry into EL0).
 *   - SVC tail (after do_ipc returns).
 *
 * Both paths leave TPIDR_EL1 pointing at the frame. x9 stays the base
 * throughout; x8 and x9 are restored last (otherwise loading into x9
 * would clobber the base).
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
