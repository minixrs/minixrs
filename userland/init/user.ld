/*
 * minix.rs user-space linker script (aarch64 EL0 init — PID 1, slice 4.8).
 *
 * Identical to the worker/server `user.ld`: a static ET_EXEC ELF the kernel's
 * minimal loader (`kernel/src/boot_image/elf.rs`) maps segment-by-segment into a
 * fresh per-process AddrSpace. Every PT_LOAD is page-aligned (vaddr *and* file
 * offset); no dynamic sections / relocations.
 *
 * Load base 0x0010_0000 (1 MiB): the same base every boot module uses — init
 * runs in its own per-process TTBR0, so the shared base never collides. The
 * stack is NOT part of the ELF: the boot loader allocates a stack frame and sets
 * SP_EL0 before eret.
 */

OUTPUT_FORMAT(elf64-littleaarch64)
OUTPUT_ARCH(aarch64)
ENTRY(_start)

PHDRS {
    text   PT_LOAD FLAGS(5); /* R+X */
    rodata PT_LOAD FLAGS(4); /* R   */
    data   PT_LOAD FLAGS(6); /* RW  */
}

SECTIONS {
    . = 0x00100000;

    .text : ALIGN(4K) {
        KEEP(*(.text._start))
        *(.text .text.*)
    } :text

    . = ALIGN(4K);
    .rodata : ALIGN(4K) {
        *(.rodata .rodata.*)
    } :rodata

    . = ALIGN(4K);
    .data : ALIGN(4K) {
        *(.data .data.*)
    } :data

    .bss : ALIGN(4K) {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /DISCARD/ : {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.note .note.*)
        *(.comment)
        *(.dynsym .dynstr .hash .gnu.hash .dynamic)
    }
}
