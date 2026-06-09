/* SPDX-License-Identifier: BSD-3-Clause */
/* Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors */
/*
 * minix.rs aarch64 kernel entry point.
 *
 * Limine has already:
 *   - placed the CPU in EL1 with the MMU enabled and translation tables set up,
 *   - allocated a stack and pointed SP at it,
 *   - masked interrupts (DAIF all set),
 *   - mapped this kernel image at its link address (higher half),
 *   - filled in the Limine response pointers for our requests.
 *
 * All we need to do is establish a frame-pointer sentinel, switch to
 * EL1h on a primed SP_EL1, and branch into `kmain`. Limine hands us
 * SPSel=0 (EL1t, kernel using SP_EL0) with SP_EL1 left unset — fine for
 * kmain itself, but the moment EL0 takes a synchronous trap the CPU
 * forces SPSel=1 and the vector handler runs on SP_EL1. We do the
 * transition the only way that survives on the QEMU-virt CPU we tested
 * against (a bare `msr sp_el1, xN` from EL1t silently locks the core):
 *
 *   mov x9, sp        ; x9 = Limine's boot SP (currently SP_EL0)
 *   msr SPSel, #1     ; switch active SP register to SP_EL1
 *   mov sp, x9        ; write x9 into SP_EL1 via `mov sp` while SPSel=1
 *
 * After this kmain runs at EL1h on Limine's boot stack and any EL0 trap
 * lands on the same stack. Sharing is safe because the kernel runs to
 * completion before erets to EL0 and EL0 is quiesced during trap
 * handling. Slice 2.4 will need a dedicated trap stack once IRQ
 * preemption can fire mid-kernel.
 *
 * We don't return; if `kmain` ever does, we spin in WFE.
 */

    .section .text.boot, "ax", %progbits
    .global _start
    .type _start, %function
_start:
    mov     x29, xzr            // frame-pointer sentinel for stack unwinders
    mov     x30, xzr            // clear link register so a backtrace stops here

    mov     x9, sp
    msr     SPSel, #1
    mov     sp, x9

    bl      kmain
1:  wfe
    b       1b
    .size _start, . - _start
