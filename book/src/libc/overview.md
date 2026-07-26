# The C toolchain and the musl port

Slice 5.6 is Phase 5's **milestone A**: an ordinary C program, compiled against
a forked musl and linked with `rust-lld`, running on minix.rs with `printf`
reaching the serial console.

Nothing in that program is minix.rs-specific — it is plain C against plain
`<stdio.h>`. It works because the libc underneath it was ported, not because the
program was. That is the whole point of the exercise.

```c
printf("minix.rs hello: Hello from C!\n");
```

```
minix.rs hello: Hello from C!
```

## Where the pieces live

| Piece | Location |
|---|---|
| The libc fork | `external/musl` (submodule → `minixrs/musl-minixrs`, branch `minixrs`) |
| Sysroot builder | `tools/build-musl.sh` → `target/musl-sysroot` |
| The C program | `userland/hello/{hello.c,hello.ld}` |
| Compile + link | `kernel/build.rs`'s `build_hello` |
| Generated ABI headers | `cargo gen-c-headers` → `target/gen-c-headers/include/minixrs/` |

## The seam: Linux syscall numbers in, IPC out

minix.rs has no Linux syscall ABI. Its only `svc #0` entry point is the MINIX
IPC trap, and every OS service is a user-space server reached by message
passing. Yet musl is written throughout in terms of `__syscall(SYS_write, …)`.

The port resolves this in **one file**. `arch/aarch64/syscall_arch.h` normally
executes `svc` with the syscall number in `x8`; in the fork, every `__syscallN`
instead calls `__minixrs_syscall`, which switches on the Linux number and issues
the corresponding server round-trip:

| Linux syscall | minix.rs mapping |
|---|---|
| `writev` | one `VFS_WRITE` per iovec, counts summed |
| `write` | one `VFS_WRITE` |
| `exit_group`, `exit` | `PM_EXIT` (does not return) |
| `set_tid_address` | constant tid `1` |
| `ioctl` | `-ENOTTY` |
| everything else | `-ENOSYS` |

Keeping musl's ~297 call sites unmodified is what lets the other ~1900 source
files stay byte-identical to upstream — and therefore rebase for free onto the
next release. The **entire** fork delta is `arch/aarch64/syscall_arch.h`,
`src/minixrs/`, a brand block in `crt/crt1.c`, and a `MINIXRS.md`.

Six syscalls is enough only because musl's startup path avoids the rest: with
`AT_UID`/`AT_GID`/`AT_SECURE` absent from the auxv, `__init_libc` takes an early
return that skips its `ppoll`; `__init_ssp(NULL)` derives the stack canary
arithmetically; and `__init_tls` uses its static `builtin_tls` because the
program has no `PT_TLS`. The fork's `MINIXRS.md` records each of those facts,
re-derived against v1.2.6 rather than assumed.

### `ioctl` is load-bearing

Answering `-ENOTTY` is not merely harmless. `__stdout_write` sets `f->lbf = -1`
when `TIOCGWINSZ` fails, which makes stdout **fully buffered** — so `printf`
output does not appear when it is called, but when `exit()` runs `__stdio_exit`.
A boot log containing `Hello from C!` therefore proves the flush path too, not
just formatting.

## What a `write` actually does

```
hello.c   printf(…)
musl      vfprintf → __stdio_write → __syscall(SYS_writev, …)
fork      __minixrs_syscall → VFS_WRITE (SENDREC to VFS)
VFS       issues a CPF_MAGIC grant over the caller's buffer → CDEV_WRITE
TTY       SYS_SAFECOPY pulls the bytes in, writes the PL011
```

The buffer never leaves the caller's address space until the driver copies it,
and it moves in exactly **one** copy. VFS names the grant's owner from the
kernel-stamped `m_source`, never from the payload — a caller-supplied owner
would turn VFS into a confused deputy, since VFS holds `SYS_PROC` and its
clients do not.

## Building

```sh
tools/build-musl.sh     # configure + make into target/musl-sysroot
cargo kernel-aarch64    # build.rs compiles and links userland/hello
```

The sysroot build uses `clang --target=aarch64-unknown-linux-musl` with `AR` and
`RANLIB` from the pinned Rust toolchain (`llvm-ar`, and `llvm-ar s` in place of
`llvm-ranlib`, which that toolchain does not ship). Linking uses `rust-lld` —
no platform linker and no Homebrew LLVM anywhere in the path.

`build-musl.sh` also runs the **real** errno check that slice 5.0 deferred to
here: it compiles the generated `<minixrs/errno.h>`'s opt-in POSIX block against
the fork's own `bits/errno.h`, so a POSIX errno whose magnitude drifted from
musl's is a build failure. Until this slice, CI could only prove macro
spellings.

### If the sysroot is missing

`kernel/build.rs` presence-checks `target/musl-sysroot/.stamp`. When it is
absent or stale it emits a `cargo::warning` and packs the `worker` ELF **under
the name `hello`**, so a fresh clone still boots green and only the
hello-specific markers go missing. It deliberately does not build the sysroot
itself — a multi-minute libc build launched from a cargo build script would turn
a first `cargo kernel-aarch64` into a mystery.

## `hello.ld`

The linker script starts from `userland/worker/user.ld` and keeps the parts that
are load-bearing there: page-aligned `PT_LOAD`s, no dynamic sections, a 1 MiB
load base, and the `FILEHDR PHDRS` idiom that puts `PT_LOAD` #0 at file offset
`0`. That idiom is **mandatory** here rather than merely nice: without it lld's
default 64 KiB page size leaves `e_phoff` in an unmapped prefix, and musl's
`__init_tls` walks the program headers from exactly the `AT_PHDR` the kernel
reports.

What it adds is everything a musl link brings that a Rust one does not, all of
it from `crti.o` / `crtn.o` / `libc.a`: `.init` and `.fini` (which define the
`_init`/`_fini` that `crt1.c` hands to `__libc_start_main`), `.init_array` /
`.fini_array` with their bracketing symbols, `.got`, and `.data.rel.ro`. Every
input section gets an explicit home, because an orphan placed past the last
`PT_LOAD` is a **silent** load failure — the kernel maps what the program
headers describe and never sees it.

Verify any change with:

```sh
"$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-readobj --program-headers \
  --elf-output-style=GNU target/hello/hello
```

## Quad-float builtins

musl's `vfprintf` references soft-float `binary128` helpers (`__multf3`,
`__floatsitf`, …): aarch64's `long double` is IEEE quad with no hardware
support. musl's `configure` finds no runtime library on this host, so those
symbols would be undefined at link time.

They come from the pinned toolchain's own `compiler_builtins`, built for the
custom `aarch64-unknown-minixrs` target by the server builds that run first.
Note that the *prebuilt* `aarch64-unknown-none` rlib in the rustup sysroot does
**not** export the C-ABI names — only the `-Zbuild-std` one does.

## The ELF brand

The kernel refuses to load an unbranded ELF. For Rust binaries the 28-byte
`.note.minixrs.ident` note comes from `minixrs_abi_note::brand!()`; for C it is
emitted from `crt/crt1.c` as a `.pushsection` global asm, so every C program is
branded with no opt-in. `crt1.o` is linked explicitly rather than pulled from an
archive, so — unlike a `libc.a` member — it can never be dropped by
archive-member selection.

Regressing it fails the **kernel build**, not the boot: `kernel/build.rs` runs
the same `scan_brand` check when it packs the boot archive.

## The ABI freeze

Slice 5.6 is the ABI freeze point (phase-5 decision D8). There is now C that
depends on the message layout, the call numbers, the endpoint encoding, and the
errno values, and it lives in a **different repository**. Past this slice those
change only via a deliberate ABI-bump PR touching both repos together.
