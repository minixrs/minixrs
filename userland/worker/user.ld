/*
 * minix.rs user-space linker script (aarch64 EL0 worker — the slice-4.7 exec
 * target).
 *
 * Like the server `user.ld`: a static ET_EXEC ELF the kernel's minimal loader
 * (`kernel/src/boot_image/elf.rs`) maps segment-by-segment into a fresh
 * per-process AddrSpace. Every PT_LOAD is page-aligned (vaddr *and* file
 * offset); no dynamic sections / relocations.
 *
 * One deliberate difference (slice 5.5): the text segment carries FILEHDR and
 * PHDRS, and `.` starts at SIZEOF_HEADERS above the load base, so PT_LOAD #0
 * begins at file offset 0 and covers the ELF header + program headers. Without
 * that, lld's default aarch64 max-page-size (64 KiB) puts the first PT_LOAD at
 * offset 0x10000 and leaves `e_phoff` in an unmapped prefix — so `SYS_EXEC`
 * would have no mapped VA to report as AT_PHDR, and musl's `__init_tls` (which
 * walks the headers from there) would have nothing to walk. The segment start
 * stays page-aligned in both vaddr and offset, so the loader's strict checks are
 * unaffected; only the entry moves up one page. Server scripts are NOT changed:
 * boot-loaded images get no initial stack frame and never read an auxv.
 *
 * Load base 0x0010_0000 (1 MiB): the same base servers use — the worker only
 * ever runs after `SYS_EXEC` builds it its own per-process TTBR0, so the shared
 * base never collides. The stack is NOT part of the ELF: exec allocates a stack
 * frame and sets SP_EL0 before eret.
 */

OUTPUT_FORMAT(elf64-littleaarch64)
OUTPUT_ARCH(aarch64)
ENTRY(_start)

PHDRS {
    text   PT_LOAD FILEHDR PHDRS FLAGS(5); /* R+X */
    rodata PT_LOAD FLAGS(4); /* R   */
    data   PT_LOAD FLAGS(6); /* RW  */
    note   PT_NOTE FLAGS(4); /* minixrs identity note */
}

SECTIONS {
    . = 0x00100000 + SIZEOF_HEADERS;

    .text : ALIGN(4K) {
        KEEP(*(.text._start))
        *(.text .text.*)
    } :text

    . = ALIGN(4K);
    /*
     * minixrs identity note (tooling/docs/abi-note.md): first thing in the
     * RO segment, dual-assigned so it lands in the rodata PT_LOAD *and* its
     * own PT_NOTE phdr. KEEP() is mandatory — nothing references the
     * section — and this rule must stay ahead of the /DISCARD/ *(.note.*)
     * rule below (first match wins).
     */
    .note.minixrs.ident : {
        KEEP(*(.note.minixrs.ident))
    } :rodata :note

    .rodata : {
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
