/*
 * MINIX 4 aarch64 exception vector table.
 *
 * Layout per ARMv8 ARM D1.10: 16 entries of 128 bytes each, table aligned
 * to 2 KiB. Four groups of four entries:
 *   - Current EL with SP_EL0
 *   - Current EL with SP_ELx
 *   - Lower EL using AArch64
 *   - Lower EL using AArch32
 * Each group has: Synchronous, IRQ, FIQ, SError.
 *
 * Phase 1 policy: every exception is fatal. Each entry saves the integer
 * register file plus the relevant system registers onto SP and calls into
 * `exception_entry`, which panics. We don't expect any exceptions yet --
 * the kernel just prints a banner and halts -- so this exists solely to
 * make any inadvertent fault legible instead of silently rebooting.
 *
 * Frame layout (matches ExceptionFrame in exception.rs):
 *   offset  field
 *   0x000   x0..x30 (31 * 8 bytes = 0xF8)
 *   0x0F8   SPSR_EL1
 *   0x100   ELR_EL1
 *   0x108   ESR_EL1
 *   0x110   FAR_EL1
 *   0x118   pad
 * Total: 0x120 bytes.
 */

    .macro EXC_ENTRY name, kind
    .global \name
    .type \name, %function
\name:
    sub     sp, sp, #0x120
    stp     x0,  x1,  [sp, #0x000]
    stp     x2,  x3,  [sp, #0x010]
    stp     x4,  x5,  [sp, #0x020]
    stp     x6,  x7,  [sp, #0x030]
    stp     x8,  x9,  [sp, #0x040]
    stp     x10, x11, [sp, #0x050]
    stp     x12, x13, [sp, #0x060]
    stp     x14, x15, [sp, #0x070]
    stp     x16, x17, [sp, #0x080]
    stp     x18, x19, [sp, #0x090]
    stp     x20, x21, [sp, #0x0A0]
    stp     x22, x23, [sp, #0x0B0]
    stp     x24, x25, [sp, #0x0C0]
    stp     x26, x27, [sp, #0x0D0]
    stp     x28, x29, [sp, #0x0E0]
    str     x30,      [sp, #0x0F0]
    mrs     x9,  spsr_el1
    mrs     x10, elr_el1
    mrs     x11, esr_el1
    mrs     x12, far_el1
    stp     x9,  x10, [sp, #0x0F8]
    stp     x11, x12, [sp, #0x108]
    mov     x0, sp                  // arg 0: pointer to frame
    mov     x1, #\kind              // arg 1: vector index 0..15
    bl      exception_entry
1:  wfe
    b       1b
    .size \name, . - \name
    .endm

    .section .text.vectors, "ax", %progbits
    .balign 0x800
    .global _vector_table
_vector_table:
    // Current EL with SP_EL0
    EXC_ENTRY el1t_sync,    0
    .balign 0x80
    EXC_ENTRY el1t_irq,     1
    .balign 0x80
    EXC_ENTRY el1t_fiq,     2
    .balign 0x80
    EXC_ENTRY el1t_serror,  3
    // Current EL with SP_ELx
    .balign 0x80
    EXC_ENTRY el1h_sync,    4
    .balign 0x80
    EXC_ENTRY el1h_irq,     5
    .balign 0x80
    EXC_ENTRY el1h_fiq,     6
    .balign 0x80
    EXC_ENTRY el1h_serror,  7
    // Lower EL using AArch64
    //
    // Slot 8 (synchronous EL0 trap) is the SVC entry point — slice 2.3
    // wires it to `el0_64_sync_entry` in `trap.S` instead of the generic
    // panic macro so EL0 IPC traps land in the SVC handler rather than
    // dumping registers and crashing. The .balign before the next entry
    // keeps the per-vector 128-byte spacing intact.
    .balign 0x80
    .global el0_64_sync
    .type el0_64_sync, %function
el0_64_sync:
    b       el0_64_sync_entry
    .size el0_64_sync, . - el0_64_sync
    .balign 0x80
    EXC_ENTRY el0_64_irq,     9
    .balign 0x80
    EXC_ENTRY el0_64_fiq,    10
    .balign 0x80
    EXC_ENTRY el0_64_serror, 11
    // Lower EL using AArch32
    .balign 0x80
    EXC_ENTRY el0_32_sync,   12
    .balign 0x80
    EXC_ENTRY el0_32_irq,    13
    .balign 0x80
    EXC_ENTRY el0_32_fiq,    14
    .balign 0x80
    EXC_ENTRY el0_32_serror, 15
    .size _vector_table, . - _vector_table
