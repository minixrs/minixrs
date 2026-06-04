/*
 * MINIX 4 user-space linker script (aarch64 EL0 servers).
 *
 * Produces a static ET_EXEC ELF that the kernel's minimal loader
 * (`kernel/src/boot_image/elf.rs`) maps segment-by-segment into a fresh
 * per-process AddrSpace. The loader is deliberately tiny, so this script
 * keeps every PT_LOAD page-aligned (vaddr *and* file offset) and emits no
 * dynamic sections / relocations.
 *
 * Load base 0x0010_0000 (1 MiB): comfortably below USER_VA_TOP and clear of
 * the kernel's hand-coded EL0 stub VAs (code 0x40_0000-0x43_0000, stacks
 * 0x80_0000-0x83_0000, stub-D heap 0x0100_0000). The stack is NOT part of
 * the ELF — the kernel allocates a stack frame and sets SP_EL0 before eret.
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
