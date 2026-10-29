/*
 * minix.rs user-space linker script (aarch64 EL0 `fileio` -- pre-Phase-6
 * chunk 5's C file-I/O proof, linked against the musl fork).
 *
 * A copy of `userland/hello/hello.ld`, and the rules below are identical to
 * it: same load base, same segments, same section homes, same discards. Read
 * that file's header for why each of them is load-bearing (the FILEHDR PHDRS
 * idiom that gives the binary an AT_PHDR, the crt-supplied .init/.fini and
 * constructor arrays, the "every input section gets an explicit home" rule,
 * and the deliberate absence of PT_TLS). Only this comment differs.
 *
 * It is a copy rather than a shared file because `kernel/build.rs` builds
 * every C program the same way -- `userland/<name>/<name>.c` linked with
 * `userland/<name>/<name>.ld` -- so a program that one day needs a different
 * layout changes its own script and nothing else. Keep the two in step until
 * then. Verify with:
 *   llvm-readobj --program-headers --elf-output-style=GNU target/fileio/fileio
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

    /* crti.o / crtn.o bracket these; _init and _fini live inside them. */
    .init : {
        KEEP(*(.init))
    } :text

    .fini : {
        KEEP(*(.fini))
    } :text

    . = ALIGN(4K);
    /*
     * minixrs identity note (tooling/docs/abi-note.md): first thing in the RO
     * segment, dual-assigned so it lands in the rodata PT_LOAD *and* its own
     * PT_NOTE phdr. KEEP() is mandatory — nothing references the section — and
     * this rule must stay ahead of the /DISCARD/ *(.note.*) rule below (first
     * match wins).
     *
     * For a C binary the note comes from crt1.o, where the fork emits it as a
     * .pushsection global asm, so every C program is branded with no opt-in.
     */
    .note.minixrs.ident : {
        KEEP(*(.note.minixrs.ident))
    } :rodata :note

    .rodata : {
        *(.rodata .rodata.*)
    } :rodata

    . = ALIGN(4K);

    /*
     * Constructor/destructor arrays. PROVIDE_HIDDEN matches what musl declares
     * (weak hidden), and bracketing them here is what keeps the walk bounded.
     */
    .preinit_array : {
        PROVIDE_HIDDEN(__preinit_array_start = .);
        KEEP(*(.preinit_array))
        PROVIDE_HIDDEN(__preinit_array_end = .);
    } :data

    .init_array : {
        PROVIDE_HIDDEN(__init_array_start = .);
        KEEP(*(SORT_BY_INIT_PRIORITY(.init_array.*)))
        KEEP(*(.init_array))
        PROVIDE_HIDDEN(__init_array_end = .);
    } :data

    .fini_array : {
        PROVIDE_HIDDEN(__fini_array_start = .);
        KEEP(*(SORT_BY_INIT_PRIORITY(.fini_array.*)))
        KEEP(*(.fini_array))
        PROVIDE_HIDDEN(__fini_array_end = .);
    } :data

    .data.rel.ro : {
        *(.data.rel.ro .data.rel.ro.*)
    } :data

    .got : {
        *(.got .got.plt)
    } :data

    .data : {
        *(.data .data.*)
    } :data

    /* No PT_TLS is emitted; these rules exist so a libc that grows TLS fails
     * loudly at link time instead of silently mapping it as ordinary data. */
    .tdata : {
        *(.tdata .tdata.*)
    } :data

    .bss : ALIGN(4K) {
        *(.tbss .tbss.*)
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /DISCARD/ : {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.note .note.*)
        *(.comment)
        *(.dynsym .dynstr .hash .gnu.hash .dynamic)
        *(.rela .rela.*)
    }
}
