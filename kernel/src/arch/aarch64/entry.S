/*
 * MINIX 4 aarch64 kernel entry point.
 *
 * Limine has already:
 *   - placed the CPU in EL1 with the MMU enabled and translation tables set up,
 *   - allocated a stack and pointed SP at it,
 *   - masked interrupts (DAIF all set),
 *   - mapped this kernel image at its link address (higher half),
 *   - filled in the Limine response pointers for our requests.
 *
 * All we need to do is establish a frame-pointer sentinel and branch into
 * `kmain`. We don't return; if `kmain` ever does, we spin in WFE.
 */

    .section .text.boot, "ax", %progbits
    .global _start
    .type _start, %function
_start:
    mov     x29, xzr            // frame-pointer sentinel for stack unwinders
    mov     x30, xzr            // clear link register so a backtrace stops here
    bl      kmain
1:  wfe
    b       1b
    .size _start, . - _start
