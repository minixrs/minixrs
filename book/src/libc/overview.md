# The C toolchain and the musl port

Slice 5.6 is Phase 5's **milestone A**: an ordinary C program, compiled against a forked musl,
running on minix.rs with `printf` reaching the serial console. Since P3c it can be built either by
the in-tree musl sysroot or by the minix.rs SDK on the real `aarch64-unknown-minixrs` triple — see
[Two toolchains, one program](#two-toolchains-one-program).

Nothing in that program is minix.rs-specific — it is plain C against plain `<stdio.h>`. It works
because the libc underneath it was ported, not because the program was. That is the whole point of
the exercise.

```c
printf("minix.rs hello: Hello from C!\n");
```

```
minix.rs hello: Hello from C!
```

## Where the pieces live

| Piece                   | Location                                                                                             |
| ----------------------- | ---------------------------------------------------------------------------------------------------- |
| The libc fork           | `external/musl` (submodule → `minixrs/musl-minixrs`, branch `minixrs`)                               |
| In-tree sysroot builder | `tools/build-musl.sh` → `target/musl-sysroot`                                                        |
| SDK prefix              | `$MINIXRS_SDK`, default `$HOME/toolchains/minixrs`                                                   |
| SDK compiler            | `$MINIXRS_SDK/bin/clang` (the `minixrs/llvm-minixrs` fork)                                           |
| SDK sysroot             | `$MINIXRS_SDK/sysroot/usr/{include,lib}` + `sysroot/.stamp`                                          |
| The C programs          | `userland/hello/hello.c`, `userland/fileio/fileio.c` (each with its own `.ld`, **musl flavor only**) |
| Flavor selection        | `kernel/build.rs`'s `build_c_program`                                                                |
| Compile + link          | `build_c_sdk` / `build_c_musl`                                                                       |
| Generated ABI headers   | `cargo gen-c-headers` → `target/gen-c-headers/include/minixrs/`                                      |

## Two toolchains, one program

`hello.c` is built by whichever C toolchain is available, in a strict preference order:

1. **`Sdk`** — the minix.rs SDK at `$MINIXRS_SDK` (default `$HOME/toolchains/minixrs`).
   Toolchain-program milestone M3.
2. **`Musl`** — the in-tree sysroot from `tools/build-musl.sh`. Slice 5.6.
3. **`Worker`** — no C toolchain: the `worker` ELF is packed *under the name* `hello`.

`Musl` is **not** a fallback. CI's blocking `qemu-smoke` job cannot install an SDK — an LLVM build
is hours — while `tests/qemu-boot.expected` *requires* the C programs' markers (`hello`'s and
`fileio`'s), so the in-tree sysroot is that gate's real dependency. Only `Worker` loses markers.

An SDK is "usable" when exactly three files exist:

```
$MINIXRS_SDK/bin/clang
$MINIXRS_SDK/sysroot/.stamp
$MINIXRS_SDK/sysroot/usr/lib/libc.a
```

The crt objects, `libclang_rt.builtins.a`, and the `lib/clang/<ver>` resource dir are deliberately
**not** probed: the driver names those itself, and nothing in this repo may hard-code the version
component. From that follows the governing rule — **a present toolchain that fails is a build
failure; an absent one is a fall-through.** A usable SDK that cannot build `hello` panics, carrying
the prefix, the stamp, a `clang -###` reproduce line, and the escape hatch. It never demotes to
`Musl`, because the boot markers are byte-identical across flavors, so a silent demotion would
report a regressed toolchain as a healthy build.

### The SDK command line

```sh
$MINIXRS_SDK/bin/clang --target=aarch64-unknown-minixrs \
    -O2 -Wall -Wextra -Werror -o target/hello/hello userland/hello/hello.c
```

That is the whole build, and the **absences are the milestone**. There is no `-T`, `-nostdinc`,
`-isystem`, `--sysroot`, `-L`, `-static`, `-ffreestanding`, no explicit crt object, no
`compiler_builtins` glob, and no separate `rust-lld` step. From the triple alone the patched driver
supplies:

| Supplied                            | Why it matters                                                          |
| ----------------------------------- | ----------------------------------------------------------------------- |
| `-static`                           | there is no dynamic loader                                              |
| `--image-base=0x100000`             | LLVM patch 0006 — this is why the SDK flavor needs no linker script     |
| `-z max-page-size=4096`             | decision D13: 4 KiB pages                                               |
| `-z separate-loadable-segments`     | no two `PT_LOAD`s share a page; the loader maps per-segment permissions |
| `crt1.o`, `crti.o`, `crtn.o`, `-lc` | from `$MINIXRS_SDK/sysroot/usr/lib`                                     |
| `libclang_rt.builtins.a`            | the quad-float helpers, from the driver's own resource dir              |

Tooling's rule applies: **anything that has to be added back here is a bug to fix in the fork, not a
flag to paper over.** Check the contract with `clang -###`, or with tooling's
`verify/check-driver.sh`.

## The seam: Linux syscall numbers in, IPC out

minix.rs has no Linux syscall ABI. Its only `svc #0` entry point is the MINIX IPC trap, and every OS
service is a user-space server reached by message passing. Yet musl is written throughout in terms
of `__syscall(SYS_write, …)`.

The port resolves this in **one file**. `arch/aarch64/syscall_arch.h` normally executes `svc` with
the syscall number in `x8`; in the fork, every `__syscallN` instead calls `__minixrs_syscall`, which
switches on the Linux number and issues the corresponding server round-trip:

| Linux syscall        | minix.rs mapping                                              |
| -------------------- | ------------------------------------------------------------- |
| `writev`             | one `VFS_WRITE` per iovec, counts summed                      |
| `write`              | one `VFS_WRITE`                                               |
| `read`               | one `VFS_READ`                                                |
| `readv`              | one `VFS_READ` per iovec, stopping at the first short read    |
| `openat`             | `VFS_OPEN` (see [Files and memory](#files-and-memory-from-c)) |
| `close`              | `VFS_CLOSE`                                                   |
| `lseek`              | `VFS_LSEEK`                                                   |
| `brk`                | `VM_BRK`, with Linux's return convention                      |
| `mmap`               | `VM_MMAP`, anonymous private read-write only                  |
| `munmap`             | `VM_MUNMAP`                                                   |
| `exit_group`, `exit` | `PM_EXIT` (does not return)                                   |
| `set_tid_address`    | constant tid `1`                                              |
| `ioctl`              | `-ENOTTY`                                                     |
| everything else      | `-ENOSYS`                                                     |

Keeping musl's ~297 call sites unmodified is what lets the other ~1900 source files stay
byte-identical to upstream — and therefore rebase for free onto the next release. The **entire**
fork delta is `arch/aarch64/syscall_arch.h`, `src/minixrs/`, a brand block in `crt/crt1.c`, and a
`MINIXRS.md`.

The table grew from six rows to the fourteen above in pre-Phase-6 chunk 5. Startup still issues
exactly **one** of them, because musl's startup path avoids the rest: with
`AT_UID`/`AT_GID`/`AT_SECURE` absent from the auxv, `__init_libc` takes an early return that skips
its `ppoll`; `__init_ssp(NULL)` derives the stack canary arithmetically; and `__init_tls` uses its
static `builtin_tls` because the program has no `PT_TLS` (a program whose `PT_TLS` outgrew
`builtin_tls` would reach the `mmap` arm before `main`; neither C program here has one). The shim's
header comment in `src/minixrs/_syscall.c` records each of those facts, re-derived against v1.2.6
rather than assumed.

### `ioctl` is load-bearing

Answering `-ENOTTY` is not merely harmless. `__stdout_write` sets `f->lbf = -1` when `TIOCGWINSZ`
fails, which makes stdout **fully buffered** — so `printf` output does not appear when it is called,
but when `exit()` runs `__stdio_exit`. A boot log containing `Hello from C!` therefore proves the
flush path too, not just formatting.

## Files and memory from C

Everything above this heading was enough to print. Opening a file and calling `malloc` need more,
and the shim now supplies it in the same file, as a translation onto requests that already existed —
`VFS_LSEEK` is the one request chunk 5 added for it. The shim's header comment holds the full v1.2.6
fact-check; the consequences a program author meets are these.

**Paths and descriptors.** `openat` ignores the directory fd (there is no working directory; paths
are absolute), strips the `O_LARGEFILE` bit musl ORs into every open (VFS's `O_KNOWN` would refuse
it), and bounds the path scan at the VFS path limit so an unterminated string cannot run off a page.
A NULL path is `-EFAULT`. Descriptors are VFS's: `NR_FDS = 8`, with 0/1/2 pre-opened to the console.
`lseek` supports `SEEK_SET` and `SEEK_CUR`; **`SEEK_END` is `-EINVAL`**, because VFS holds no file
size and an FS-band size request has no consumer yet. `fclose` on a partly read stream seeks back by
a negative `SEEK_CUR`, which is why `lseek` was not optional.

**`brk`.** mallocng calls the raw `brk` syscall — `brk(0)` for the current break, then `brk(cur + 2
pages)` — and treats anything but exactly the value it asked for as failure. The `brk` arm therefore
follows Linux, not MINIX: it returns the new break on success and the *current* break on refusal,
never an errno. VM's heap now starts at the page-aligned end of the exec'd image (`VM_BRK(0)` reads
it back) and replies the exact break, so the arithmetic mallocng does on the result holds.

**`mmap` and `munmap`.** `mmap` is honoured only for what VM can do: anonymous, private, placed by
VM, read-write. `MAP_NORESERVE` and `MAP_STACK` are accepted as inert; `MAP_FIXED`, file-backed,
`MAP_SHARED` and the other flag bits get `-ENOSYS`. **`prot` is ignored**, so the `PROT_NONE` guard
pages mallocng asks for are ordinary read-write memory and do not guard. mallocng's one `MAP_FIXED`
call (a guard on the first page of a freshly grown brk area) gets `-ENOSYS` and is ignored, which
leaves `errno` at `ENOSYS` after a first `malloc` that succeeded. Each live mapping costs one of
VM's 16 per-process region slots, shared with the heap and the stack, so **at most 14 mappings are
live at once**; mallocng mmaps every large allocation and many groups, and a program holding many of
them meets `ENOMEM` long before it meets a shortage of memory. `munmap` must name the base and
length of a mapping `mmap` returned — mallocng only ever does — and VM's arena is bump-only, so an
unmapped range is not reused.

**What stays `-ENOSYS`.** Everything not in the table. The ones a real program will notice are
`mprotect` (mallocng tolerates its `ENOSYS`), `mremap` (`realloc` falls back to malloc, copy, free),
`fstat`/`stat` (no FS-band request exists), `dup`/`pipe`/`fcntl`, `getdents`, and time and signal
calls. `madvise` is never issued: mallocng's `USE_MADV_FREE` is 0.

**Every buffer is pre-faulted.** VM backs heap and mmap pages lazily, on the owner's first touch,
but VFS reaches a caller's buffer through the kernel's copy engine, which answers `EFAULT` for an
unmapped page rather than asking VM to map it. mallocng never touches fresh mmap memory (`calloc`
included — it is already zero), so a `read()` into memory `malloc` just returned would fail or
succeed by page-layout luck. The shim therefore issues **one volatile load per 4 KiB page** of each
buffer before the request that names it: `read`, every `readv` and `writev` iovec, and `write`. A
load is enough, because VM maps every fault read-write, and it leaves the bytes alone, so one helper
serves both directions.

This is a workaround for a kernel gap, carried in libc, and it has a deviation: a **bad buffer
pointer now kills the process** — the load faults outside every region, VM raises `SIGSEGV` — where
POSIX says `EFAULT`. A *mapped but read-only* destination still gets VFS's `EFAULT`, since the
pre-fault only reads. The proper fix (the copy engine asks VM to map the page and retries) is owed;
see the Deferred table in `docs/plans/phase-6-prep.md`.

**The proof.** `/bin/fileio` (`userland/fileio/fileio.c`) is a second C program on the root
filesystem, exec'd by `init` next to `hello`. It exercises, through musl: raw `read`, `lseek`
(including the `SEEK_END` refusal), `readv`, a read into a never-touched mmap'd buffer, a write past
end of file that leaves a hole (`/etc/seekhole`), `brk` against the heap origin, `fopen`, `fgets`,
`fread` and the negative `SEEK_CUR` that `fflush` issues on a partly read stream, a verified `mmap`,
a large verified `malloc`, and finally a descriptor deliberately left open at exit so that VFS's
`VFS_PM_EXIT` row reset is visible (`[diag vfs] pm-exit reset closed=1`). Each step prints its own
`minix.rs fileio: … ok` marker; the list lives in `tests/qemu-boot.expected`. Every run truncates
and rewrites `/etc/seekhole`, so `init` keeps at most one `fileio` alive at a time — it skips that
slot of its rotation until the running one is reaped — while still letting `hello` and `fileio`
overlap.

Two kernel defects surfaced while building it, and both were fixed in this chunk because they hit
every user process, not just fileio. The kernel never saved the user **thread pointer**
(`TPIDR_EL0`) per process, so with two C programs alive at once each wrote `errno` through the
other's TLS address and was killed by `SIGSEGV`. It also never saved the **FP/SIMD registers**
across a trap, so a vectorised loop that took a page fault on the lazily mapped buffer resumed with
its loop-carried vectors overwritten by whatever ran in between: `memset`/`memcpy`-shaped code
silently produced wrong bytes after the first fault. Both are now part of the per-process register
frame, saved on every EL0 entry — see `docs/conventions/kernel.md`.

**Size.** `/bin/fileio` links all of stdio and mallocng and is about 259 KB against the 256 KiB
`VFS_EXEC_MAX` ceiling that VFS stages an exec image through; around 110 KB of it is the symbol
table. The build asserts the ceiling, so growth fails loudly.

## What a `write` actually does

```
hello.c   printf(…)
musl      vfprintf → __stdio_write → __syscall(SYS_writev, …)
fork      __minixrs_syscall → VFS_WRITE (SENDREC to VFS)
VFS       issues a CPF_MAGIC grant over the caller's buffer → CDEV_WRITE
TTY       SYS_SAFECOPY pulls the bytes in, writes the PL011
```

The buffer never leaves the caller's address space until the driver copies it, and it moves in
exactly **one** copy. VFS names the grant's owner from the kernel-stamped `m_source`, never from the
payload — a caller-supplied owner would turn VFS into a confused deputy, since VFS holds `SYS_PROC`
and its clients do not.

## Building

```sh
tools/build-musl.sh     # configure + make into target/musl-sysroot
cargo kernel-aarch64    # build.rs picks a flavor, compiles and links userland/hello

# force a flavor
MINIXRS_SDK=/nonexistent cargo kernel-aarch64   # in-tree musl, deletes nothing
MINIXRS_SDK=~/toolchains/minixrs cargo kernel-aarch64
```

**Never write inside `$MINIXRS_SDK`.** Tooling's `build-musl.sh` does `rm -rf $SDK/sysroot`, so
anything the OS tree left there would vanish without warning. Every artifact this repo produces goes
to `target/hello/`.

The in-tree sysroot build uses `clang --target=aarch64-unknown-linux-musl` with `AR` and `RANLIB`
from the pinned Rust toolchain (`llvm-ar`, and `llvm-ar s` in place of `llvm-ranlib`, which that
toolchain does not ship). Linking uses `rust-lld` — no platform linker and no Homebrew LLVM anywhere
in the path. The SDK flavor uses the fork's own `clang` and `ld.lld` instead, and needs neither.

`build-musl.sh` also runs the **real** errno check that slice 5.0 deferred to here: it compiles the
generated `<minixrs/errno.h>`'s opt-in POSIX block against the fork's own `bits/errno.h`, so a POSIX
errno whose magnitude drifted from musl's is a build failure. Until this slice, CI could only prove
macro spellings.

### If a toolchain is missing

`kernel/build.rs` presence-checks the *sysroots*, and the three cases differ:

| Case                                             | Result                                                             |
| ------------------------------------------------ | ------------------------------------------------------------------ |
| No `$MINIXRS_SDK` (unset, default prefix absent) | silent; try the in-tree musl sysroot                               |
| `MINIXRS_SDK` **set** but unusable               | one `cargo::warning` naming the missing file, then try musl        |
| Neither sysroot                                  | `cargo::warning`; pack the `worker` ELF **under the name `hello`** |

In the last case a fresh clone still boots green and only the hello-specific markers go missing.
Neither builder builds its own sysroot — a multi-minute libc build launched from a cargo build
script would turn a first `cargo kernel-aarch64` into a mystery, and an LLVM build would be far
worse.

Presence-checking the sysroot rather than the `external/musl` submodule is the same reasoning: a
clone that ran `git submodule update` but not `tools/build-musl.sh` must not trigger a libc build.

Reporting is host-side only, and `Musl` stays **silent** — it is what every CI job builds, and
warning on the norm is how people learn to ignore build-script warnings. So: SDK warning ⇒ `sdk`;
fallback warning ⇒ `worker`; neither ⇒ `musl`.

## `hello.ld` — the musl flavor only

The **SDK flavor has no linker script**, because everything this one exists to say is in the driver:
the image base is pinned by LLVM patch 0006, and the two `-z` flags come from the triple. The script
below applies to the in-tree musl flavor, which links with a stock `rust-lld` that knows none of it.

The linker script starts from `userland/worker/user.ld` and keeps the parts that are load-bearing
there: page-aligned `PT_LOAD`s, no dynamic sections, a 1 MiB load base, and the `FILEHDR PHDRS`
idiom that puts `PT_LOAD` #0 at file offset `0`. That idiom is **mandatory** here rather than merely
nice: without it lld's default 64 KiB page size leaves `e_phoff` in an unmapped prefix, and musl's
`__init_tls` walks the program headers from exactly the `AT_PHDR` the kernel reports.

What it adds is everything a musl link brings that a Rust one does not, all of it from `crti.o` /
`crtn.o` / `libc.a`: `.init` and `.fini` (which define the `_init`/`_fini` that `crt1.c` hands to
`__libc_start_main`), `.init_array` / `.fini_array` with their bracketing symbols, `.got`, and
`.data.rel.ro`. Every input section gets an explicit home, because an orphan placed past the last
`PT_LOAD` is a **silent** load failure — the kernel maps what the program headers describe and never
sees it.

Verify any change with:

```sh
"$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-readobj --program-headers \
  --elf-output-style=GNU target/hello/hello
```

## Quad-float builtins

musl's `vfprintf` references soft-float `binary128` helpers (`__multf3`, `__floatsitf`, …):
aarch64's `long double` is IEEE quad with no hardware support. musl's `configure` finds no runtime
library on this host, so those symbols would be undefined at link time.

In the **musl flavor** they come from the pinned Rust toolchain's own `compiler_builtins`, built for
the custom `aarch64-unknown-minixrs` target by the server builds that run first. Note that the
*prebuilt* `aarch64-unknown-none` rlib in the rustup sysroot does **not** export the C-ABI names —
only the `-Zbuild-std` one does, which is why `build_c_musl` globs the nested target dir rather than
the sysroot.

In the **SDK flavor** the driver links `libclang_rt.builtins.a` from its own resource dir and the
problem simply does not arise. That archive lives under
`$MINIXRS_SDK/lib/clang/<ver>/lib/aarch64-unknown-minixrs/` — and **nothing in this repo may
hard-code that `<ver>`.** It is tooling's rule and it is why `usable_sdk` probes neither the archive
nor the resource dir: one driver invocation never mentions the version component, so let the driver
derive it.

## The host environment is a trap

clang folds `CPATH` and the `*_INCLUDE_PATH` family into the **front** of the include search list —
ahead of its resource dir *and* ahead of the sysroot — and `-nostdinc` does not suppress them. A
foreign `errno.h` reachable that way would shadow musl's, and decision D7 turns on the fork's errno
*values* being the ones the kernel agrees with. So `kernel/build.rs` routes every clang invocation
through `clang_command`, which removes `CPATH`, `C_INCLUDE_PATH`, `CPLUS_INCLUDE_PATH`,
`OBJC_INCLUDE_PATH`, `OBJCPLUS_INCLUDE_PATH`, `LIBRARY_PATH` and `SDKROOT`.

Check your own machine with one line:

```sh
$MINIXRS_SDK/bin/clang -E -v --target=aarch64-unknown-minixrs -x c /dev/null
```

Scrubbed, the list should be the resource dir, then the sysroot, then `/usr/local/include`. That
last entry is injected by the fork's driver itself and is **not** an environment leak; it sorts
*after* the sysroot, so it can only supply headers musl lacks. Resist "fixing" it with
`-nostdlibinc`, which would drop the sysroot along with it.

## Measured shapes

Same source, same libc, two toolchains:

|                  | SDK                | in-tree musl         |
| ---------------- | ------------------ | -------------------- |
| Size             | 46,664 B (45.6 KB) | 200,152 B (195.5 KB) |
| `PT_LOAD`s       | 4                  | 3                    |
| Entry            | `0x101000`         | `0x101000`           |
| Brand note       | `0x100200`         | `0x117000`           |
| Last mapped byte | `0x108ce0`         | `0x11caf8`           |

Both sit far below `uspace::USER_REGION_LIMIT` (`0x3FFE_F000`) — the ceiling every VA a process
places by hand has to clear, and the bound the tooling repo's `check-image.sh` refuses a `PT_LOAD`
against. The larger build's last mapped byte, `0x11caf8`, is three orders of magnitude short of it.

`0x200000` used to be the stack; it is not any more. The stack is 16 pages at `0x3FFF_0000`, flush
beneath the device window, with an unmapped guard page below it. `0x200000` is instead where
SDK-built images will *start* once the patched clang stops forcing `--image-base=0x100000` — that
pin existed only to keep images off the old stack page, so the compensation has no subject left. The
figures above were measured with the pin still in place: when it drops, the SDK column shifts up by
1 MiB and the in-tree-musl column does not move, because repo-built images keep `user.ld`'s explicit
`0x100000`. Two different load bases are fine — each image has its own TTBR0, the same reasoning
that already lets every server share one base.

The size gap is mostly `-z separate-loadable-segments` splitting RO/RX/RW/relro across four segments
in the SDK build versus three, plus differing section merging — not a libc difference.

This closes a loop from slice 5.7. At 4096 bytes per MFS block, 46,664 bytes is **12 blocks** —
still past the seven direct zones, so `hello` continues to exercise MinixFS's single-indirect arm in
the SDK flavor exactly as it does in the musl one. `/etc/pattern`'s 40 KiB mandate is therefore
unchanged: it is still what keeps that arm live in the `worker`-fallback configuration, where
`hello` is 4 blocks and fits inside the direct zones.

### Verifying a build

```sh
# SDK flavor
$MINIXRS_SDK/bin/llvm-readelf --file-header --program-headers --notes target/hello/hello
strings target/hello/hello | grep llvm-minixrs   # provenance: the fork's clang + lld

# tooling's gates -- run from the checkout, they are not installed into the SDK
~/src/tooling/verify/check-brand.sh target/hello/hello   # BRANDED minixrs abi_version=1
~/src/tooling/verify/check-image.sh target/hello/hello   # LOADABLE
```

Read `target/hello/hello` only after a **successful** build, and remember that both flavors write
that one path: a panic — or a run with a different `MINIXRS_SDK` — leaves the other flavor's binary
there. For the musl flavor use the SDK-free reader, since a musl-only developer has no
`$MINIXRS_SDK/bin`:

```sh
"$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-readobj --program-headers \
  --elf-output-style=GNU target/hello/hello
```

### Two things nothing enforces

The SDK links **its own** musl, built from the same fork but at whatever commit tooling's
`build-sysroot.sh` last saw. `$MINIXRS_SDK/sysroot/.stamp` records it as `musl=<sha>`; today that is
the merge commit of `external/musl`'s own HEAD and `git diff` between them is empty. An equality
check is impossible, because the two SHAs legitimately differ. So it is a **manual** check, and the
consequence of skipping it is that the two flavors quietly test different libc code — rebase the
fork without re-running `build-sysroot.sh` and that is exactly what happens.

The stamp's `minixrs=<sha>` is the same kind of snapshot for the installed `minixrs/*.h` headers: it
names the commit whose `gen-c-headers` output was installed, and it does not track. That is
tolerable only because of decision D8's ABI freeze — so **any `kernel-shared` ABI change requires
re-running tooling's `scripts/build-sysroot.sh`.**

## The ELF brand

The kernel refuses to load an unbranded ELF. For Rust binaries the 28-byte `.note.minixrs.ident`
note comes from `minixrs_abi_note::brand!()`; for C it is emitted from `crt/crt1.c` as a
`.pushsection` global asm, so every C program is branded with no opt-in. `crt1.o` is linked
explicitly rather than pulled from an archive, so — unlike a `libc.a` member — it can never be
dropped by archive-member selection.

Regressing it fails the **kernel build**, not the boot: `kernel/build.rs` runs the same `scan_brand`
check when it packs the boot archive.

## The ABI freeze

Slice 5.6 is the ABI freeze point (phase-5 decision D8). There is now C that depends on the message
layout, the call numbers, the endpoint encoding, and the errno values, and it lives in a **different
repository**. Past this slice those change only via a deliberate ABI-bump PR touching both repos
together.
