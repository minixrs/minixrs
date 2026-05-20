/*
 * MINIX 4 aarch64 kernel linker script
 *
 * Higher-half kernel placed at 0xffffffff80000000 -- matches docs/memory-layout.md
 * and Limine's default aarch64 kernel base. The Limine bootloader scans the ELF
 * for a `.limine_requests` section bracketed by start/end markers, fills in
 * response pointers, sets up translation tables (the kernel is mapped at the
 * link address), and jumps to `_start`.
 */

OUTPUT_FORMAT(elf64-littleaarch64)
OUTPUT_ARCH(aarch64)
ENTRY(_start)

PHDRS {
    requests PT_LOAD FLAGS(4); /* R   */
    text     PT_LOAD FLAGS(5); /* R+X */
    rodata   PT_LOAD FLAGS(4); /* R   */
    data     PT_LOAD FLAGS(6); /* RW  */
}

SECTIONS {
    . = 0xffffffff80000000;

    .limine_requests : {
        KEEP(*(.limine_requests_start))
        KEEP(*(.limine_requests))
        KEEP(*(.limine_requests_end))
    } :requests

    . = ALIGN(4K);
    .text : {
        KEEP(*(.text.boot))
        *(.text .text.*)
    } :text

    . = ALIGN(4K);
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata

    . = ALIGN(4K);
    .data : {
        *(.data .data.*)
    } :data

    .bss : {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    } :data

    /DISCARD/ : {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.note .note.*)
        *(.comment)
    }
}
