/*
 * minix.rs user-space linker script (aarch64 EL0 `hello` — the slice-5.6 C
 * milestone, linked against the musl fork).
 *
 * Derived from `userland/worker/user.ld`, and the parts they share are load-
 * bearing in exactly the same way: a static ET_EXEC ELF the kernel's minimal
 * loader (`kernel/src/boot_image/elf.rs`) maps segment-by-segment into a fresh
 * per-process AddrSpace, every PT_LOAD page-aligned in vaddr *and* file offset,
 * no dynamic sections or relocations, and the same 0x0010_0000 load base (safe
 * to share, because exec always builds its own per-process TTBR0).
 *
 * The FILEHDR PHDRS idiom on the text segment is what gives this binary an
 * AT_PHDR. Without it lld's default aarch64 max-page-size (64 KiB) puts
 * PT_LOAD #0 at file offset 0x10000, leaving `e_phoff` in an unmapped prefix,
 * so SYS_EXEC has no mapped VA to report.
 *
 * Measured consequence today (slice 5.6, mutation-tested): the auxv drops from
 * 4 entries to 1 and hello still runs. That is not luck — `do_exec` OMITS
 * AT_PHDR rather than reporting an unmapped one, and musl's `__init_tls` then
 * walks zero program headers, finds no PT_TLS, and falls back to its static
 * `builtin_tls`, which is what this program uses anyway.
 *
 * Keep the idiom regardless. It is the difference between "there is no TLS
 * image" and "we could not look", and those stop being the same answer the
 * moment libc or this program gains a PT_TLS — at which point the failure is
 * silently wrong thread-local storage rather than a link error.
 *
 * What this script has to place that a Rust one does not, all of it arriving
 * from crti.o / crtn.o / libc.a:
 *
 *   .init / .fini              `_init` and `_fini`, which crt1.c hands to
 *                              __libc_start_main. Orphaned, they land outside
 *                              a PT_LOAD and the call jumps into nothing.
 *   .init_array / .fini_array  walked by __libc_start_init and libc_exit_fini.
 *                              The bracketing symbols are weak-hidden in musl,
 *                              so an undefined pair merely skips the walk —
 *                              but a section placed with no symbols would be
 *                              walked with garbage bounds. Define both.
 *   .got / .data.rel.ro        emitted even for a static non-PIE link.
 *
 * Every input section gets an explicit home. An orphan placed past the last
 * PT_LOAD is a SILENT load failure — the loader maps what the phdrs describe
 * and never sees it. Verify with:
 *   llvm-readobj --program-headers --elf-output-style=GNU target/hello/hello
 *
 * There is deliberately no PT_TLS: musl keeps errno in the pthread struct via
 * tpidr_el0 rather than in a __thread variable, so libc.a carries no .tdata or
 * .tbss and __init_tls uses its static builtin_tls. The rules below still catch
 * those sections so a future libc change surfaces as a link-time surprise
 * rather than a runtime one.
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
