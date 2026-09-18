# Phase 5: musl Fork + File Systems — design + slice plan

Produced by the chunk-6 design session (`phase-5-prep.md`), 2026-07-24. Every design decision below
is **locked** (decided with rationale, alternatives recorded); the slice list is the working
decomposition. Markers follow the `docs/plan.md` convention: `◀ next` (unstarted), `◀ ready (branch
…, pending merge)`, `✓ shipped (PR #N, merged YYYY-MM-DD)`. Flip markers in each slice's own PR —
here and in `docs/plan.md`'s Phase 5 table.

**Milestone:** init execs `/bin/hello` — a C program compiled against the musl fork — from an MFS
root image: `PM_EXEC("/bin/hello")` → VFS lookup/read from MFS-on-ramdisk → kernel grant-sourced ELF
load → musl `printf` → VFS → TTY → serial. An earlier intermediate milestone (A, slice 5.6) proves
the musl half with a boot-embedded hello before any filesystem exists.

---

## Where Phase 4 left the ground

Facts the design rests on (verified against source at session time):

- `SYS_DIAGCTL`, `SYS_COPY`, `SYS_SAFECOPY`, `SYS_SETGRANT`, `SYS_TIMES`, `SYS_IRQCTL` are already
  numbered (`0x605`–`0x612` range), granted to every `SRV_T` priv by the existing `k_call_mask`
  fill, and routed to the caller-local dispatch arm — they are one-line `ENOSYS` macros in
  `kernel/src/system/stubs.rs`. **Phase 5 adds zero new kernel-call numbers** (`NR_KERN_CALLS` stays
  18); it fills in bodies.
- `Priv.grant_table` / `grant_entries` exist since slice 2.2, unused.
- Proc slots VFS 1, MEM 3, TTY 4, MFS 6, PFS 8 already have `BootEntry` rows, priv slots, and
  `SRV_T` `ipc_to`/`k_call_mask` wiring in `kernel/src/proc/table.rs` — they are simply never loaded
  because `kernel/build.rs` packs no ELF for them. Loading each is: crate + `user.ld`
  - a `servers` array row + a `qemu-boot.expected` line.
- Request bands `0x800`, `0x900`, `0xA00` are free (between PM `0x700` and VM `0xC00`, all below
  `NOTIFY_MESSAGE = 0x1000`). `0xB00` was the fourth until slice 5.3 claimed it for CDEV; the
  remaining three are earmarked VFS (`0x800`, 5.4), FS (`0x900`, 5.8), and BDEV (`0xA00`, 5.7), and
  `callnr_h.rs`'s `bands_are_in_ascending_numeric_order` test enforces where each one goes. (Slices
  5.3–5.6 recorded this pairing the other way round — BDEV at `0x900`, the FS band at `0xA00` — in
  four in-tree comments; slice 5.7 corrected them all when it claimed `0xA00`. What is load-bearing
  is only that the bands stay in ascending numeric order, which is why the correction was free.)
- The MXBI archive already supports non-ELF blobs: the boot loader skips negative `proc_nr` records
  and `BootImage::module_by_name` returns raw bytes with no ELF validation.
- `boot_image/elf.rs::load_into` takes a plain `&[u8]` (source-agnostic) but requires page-aligned
  `p_offset` per PT_LOAD and enforces W^X.
- IPC message copies are raw `read_volatile`/`write_volatile` through the active TTBR0; an
  in-range-but-unmapped user pointer is a **kernel panic** (the EL1 same-EL vector slots dump
  registers and panic; there is no fixup).
- The musl fork (`musl-minixrs`, sibling repo) is pristine: v1.2.5 + 102 upstream commits,
  MIT-clean, zero MINIX changes yet. A static musl hello-world needs only `writev`,
  `exit`/`exit_group`, `set_tid_address`, and a benign `ioctl(TIOCGWINSZ)` at runtime — **no malloc,
  no brk/mmap** — and aarch64's thread-pointer setup is syscall-free (`msr tpidr_el0`).
- `kernel-shared/src/error.rs` values are bespoke (its "matches MINIX 3" comment is wrong): classic
  book-era MINIX numbering (EPERM 1 … EINVAL 22, EDEADLK 35, ENOSYS 38) is *identical* to Linux/musl
  numbering for the POSIX block; modern MINIX uses NetBSD numbering with MINIX-specific IPC errnos
  above 200.

## Locked design decisions

### D1. Console/stdio sink: minimal TTY server (TX-only), now

A real user-space TTY driver lands in Phase 5 — not a kernel printf shim as the only console. Scope
is deliberately minimal: **TX only, polling PL011** (write `DR`, poll `FR.TXFF`), no interrupts. The
kernel **pre-maps the UART MMIO page (phys `0x0900_0000`) into TTY's address space at boot** with
Device-nGnRE attributes — TTY is a boot server, so this is a one-off boot step like stack setup, and
`AddrSpace` grows a device-memory mapping mode. VFS routes fd 1/2 over a new CDEV band to TTY; TTY
reads the payload via grant (`SYS_SAFECOPY`) and writes the UART. Kernel messages keep using the
kernel's own writer; interleaving is acceptable (raw tick bytes already interleave).

*Rejected:* `SYS_DIAGCTL` as the **only** console (no user-space driver ever exercised in Phase 5 —
but see D2, it still lands as the debug channel); VM-mediated `VMCTL_MAP_PHYS` device mapping (more
MINIX-authentic, revisit for virtio-mmio in Phase 6); TX+RX with IRQ routing (`SYS_IRQCTL`, GIC →
EL0 notification — nothing consumes input until the Phase 7 shell; deferred to Phase 6 alongside
virtio-console).

### D2. `SYS_DIAGCTL` becomes real, early — as the debug channel

Servers currently cannot print at all; every later slice needs observability while TTY/VFS/grants
are still under construction. `SYS_DIAGCTL` (MINIX 3 pedigree: `kernel/system/do_diagctl.c`) gets a
body in slice 5.1 with an **inline-payload** form — length + up to ~90 text bytes inside the 96-byte
message payload — so it needs *zero* user-copy machinery. A `server-rt` helper loops longer strings.
TTY/CDEV is the real stdio path; DIAGCTL is for server bring-up debugging, exactly like MINIX's
kernel message path.

### D3. Root image: MFS image in the MXBI archive + `memory` ramdisk over BDEV

The root filesystem is an **MFS-formatted image** built at compile time by a new host tool
(`tools/mkfs-mfs`), packed into the MXBI archive as a non-ELF blob (`proc_nr = -1`, name `rootfs`).
At boot the kernel **copies the blob into freshly allocated RAM frames and maps them RW into the
`memory` driver's address space** (the archive copy in kernel `.rodata` stays pristine; the RAM copy
makes the 5.10 write path natural). MEM discovers `(va, len)` via a new `SYS_GETINFO` selector. MEM
serves the image over a minimal **BDEV band** to MFS. This is the MINIX boot shape (`memory` driver
ramdisk) — Phase 6 swaps virtio-blk in under an unchanged MFS.

`tools/mkfs-mfs` is a host Rust binary sharing on-disk structs with the `fs/mfs` library half, so
the format logic is host-tested round-trip (mkfs writes, mfs reader reads).

*Rejected:* MFS reading the image directly without BDEV (one fewer protocol now, MFS rework when
real block devices arrive); a cpio-style initramfs unpacked by VFS (avoids MFS entirely — but MFS
*is* the Phase 5 deliverable).

### D4. Grant model: real MINIX-style grants — direct + magic — plus `SYS_COPY`

Full MINIX shape, not an interim: each granting process keeps a **grant table in its own address
space** (`GrantEntry` array); `SYS_SETGRANT` records `(addr, entries)` in the caller's `Priv` (the
fields already exist); `SYS_SAFECOPY` resolves a grant id, reads the entry from the *granter's*
address space, validates (kind, `CPF_READ`/`CPF_WRITE` access, grantee endpoint, range, idx+seq
staleness — MINIX `GRANT_SHIFT` packing), then copies. Two grant kinds in Phase 5:

- **direct** — granter grants a range of its own memory to a grantee (server↔server, e.g. PM→TTY
  console writes);
- **magic** — a server-grade granter (VFS) grants one process's memory to another (TTY/MFS reading a
  *user's* buffer) — the real single-copy read/write data path. Gated on server-grade priv.

**Indirect grants are deferred** (documented `EINVAL` arm) — nothing in Phase 5 re-grants a received
grant.

`SYS_COPY` (raw privileged endpoint+addr copy, MINIX `sys_datacopy`) is implemented on the same
engine — VFS uses it for small control-plane reads (e.g. fetching a path string from a caller).

**Copy engine + inherent fault safety:** all cross-address-space access goes through **explicit
page-table walks via the HHDM** — the kernel never dereferences a user VA through the live TTBR0 for
grant work. An unmapped page is a walk miss, returned as `EFAULT`; no exception-fixup machinery
exists or is needed. Page-at-a-time: walk source frame, walk destination frame, `memcpy` through
HHDM aliases. (`AddrSpace` already promises all table access is HHDM-based and works on non-active
address spaces.)

`GrantEntry` is `#[repr(C)]` in `kernel-shared` (flat struct — flags, seq, grantee, third-party
endpoint, addr, len — no union needed for direct+magic), with the CPF flag constants and id packing
helpers, host-tested.

*Rejected:* direct-only grants (double copy through VFS for every FS byte, magic retrofitted later
anyway); `SYS_COPY`-only interim (ungoverned byte movement; the chunk-6 brief explicitly prefers
real grants).

### D5. Fault-safe user copy: PT-walk replaces `read/write_volatile`

The chunk-6-mandated safety floor, first feature slice (5.1 — right after the errno resequence so
its `EFAULT` marker carries the final value from day one). The same walk-via-HHDM technique from D4
replaces the two message-copy functions (`copy_msg_from_user` / `copy_msg_to_user`): a bad user
message pointer becomes **`EFAULT` in the caller's `x0`**, never a panic. The deferred receive-side
flush (`flush_deliver_msg`, currently `let _ =` with a comment admitting the error is dropped)
surfaces failure as `EFAULT` in the *receiver's* parked `x0` — the receiver asked for delivery to a
bad buffer; the message is consumed. `user_va_ok` stays as the cheap range/alignment pre-gate.

*Rejected:* a real `el1h_sync` handler + ELR fixup table (Linux-style `extable`) — strictly more
machinery for the same observable behavior, and the PT-walk engine has to exist for grants anyway.
Revisit only if message round-trip cost ever matters (walks can be cached per dispatch).

### D6. ELF authority for FS-backed exec: the kernel keeps it

`boot_image/elf.rs` stays the single ELF loader. It is refactored over a small **chunked-source
abstraction** — read header/phdrs into stack buffers, copy segment pages — with two sources: the
existing boot-image byte slice, and a **granted user-space buffer** (cross-AS page reads on the D4
engine). `SYS_EXEC` gains a second payload form carrying `(granter, grant_id, len)` alongside the
existing name form (the name form stays for boot-embedded regression). PM/VFS stage the file: VFS
reads the whole binary from MFS into a static exec buffer (capped; a compile-time assert covers
`/bin/hello`), direct-grants it to PM's exec flow. No kernel heap, no kernel staging.

The loader stays strict (page-aligned `p_offset`, W^X): user binaries are linked to comply (see
D13). Frame-exhaustion hardening (a `p_memsz` cap) lands with the grant-source form, since exec
input stops being build-produced.

*Rejected:* a user-space loader in VFS/PM/VM (MINIX-authentic `libexec` direction — needs
map-into-third-party-AS surface through `SYS_VMCTL` and a second loader implementation for the same
milestone; revisit if Phase 7+ wants PIE/interpreters).

### D7. Errno ABI: classic-MINIX values (≡ Linux/musl), MINIX extras above 200

`error.rs` is renumbered once, as the **opening slice (5.0)** — before any C exists *and* before any
Phase 5 slice bakes an errno value into a trace marker, so no later slice ever re-touches an
expected line over a value change:

- **POSIX block:** classic book-era MINIX values, which are identical to Linux/musl numbering for
  everything Phase 5 needs (EPERM 1 … EACCES 13, EFAULT 14, EINVAL 22, EDEADLK 35, ENOSYS 38 —
  negated in-kernel as today). Where classic MINIX and Linux ever diverge, **musl's value wins**
  (the point is that musl's stock `bits/errno.h` and `syscall_ret.c`'s `r > -4096UL` convention work
  unmodified).
- **MINIX-specific IPC errnos** (`EDEADSRCDST`, `EDONTREPLY`, `EGENERIC`, `ELOCKED`, `EBADCALL`, …):
  modern-MINIX 200-band values (202, 203, 204, 208, 209, …) — clear of Linux's entire range, so they
  can never collide with a musl-visible errno.
- The missing FS errnos (EEXIST, ENOTDIR, EISDIR, ENOTTY, EMFILE, ENFILE, ENOSPC, EROFS, ESPIPE,
  EPIPE, EBUSY, ENODEV, ENAMETOOLONG, ENOTEMPTY, EXDEV, …) are added in the same renumber at their
  Linux values.
- The wrong "values match MINIX 3" comment is fixed to state the actual policy above.

*Rejected:* keeping bespoke values + a translation table in the musl wrapper (permanent
two-numbering-systems tax); NetBSD/modern-MINIX numbering (most faithful to the modern reference
tree, but every errno diverges from Linux muscle memory and musl needs a full `bits/errno.h`
override).

### D8. C header bridge: hand-rolled generator, frozen when the first C lands

A small host tool (`tools/gen-c-headers`) depends on `kernel-shared` as a normal Rust dependency and
**prints** the C headers (`minix/ipc.h` — `message` struct + IPC primitive numbers; `minix/com.h` —
endpoints; `minix/callnr.h` — call numbers + payload offsets; errno values for `bits/errno.h`
verification). Values are read from the live Rust constants, so they are correct by construction —
no const-eval limits, no drift *possible* because the headers are **generated at build time into the
musl sysroot, never committed**. The generator lands with the errno renumber in slice 5.0; CI
regenerates and compiles the headers (`clang -fsyntax-only`) as a host check from 5.0 on, so
breakage fails fast — the headers simply grow as later slices add bands. **ABI freeze point: slice
5.6** (first C file) — after it, `Message` layout, call numbers, endpoints, and errnos are frozen;
changes require a deliberate ABI-bump PR touching both repos.

*Rejected:* cbindgen (cannot const-eval the `ProcNr::new()` endpoint constants — would need mirror
consts for everything plus config to track); hand-written headers + drift test (manual upkeep as the
ABI grows).

### D9. musl vendoring: git submodule at `external/musl`

The fork enters the build as a **submodule pinned to `musl-minixrs`'s `main`**. Rationale: keeps
1537 C files out of this repo's history; `external/**` is already Sonar-excluded;
cargo-audit/deny/geiger only walk the cargo graph so a C tree is invisible to them either way; MINIX
changes land as reviewable PRs in the fork repo and are pinned here by submodule bumps. The kernel
build **presence-checks the submodule** and, when uninitialized, skips packing hello with a
`cargo::warning` and falls back gracefully (see 5.6) — plain `cargo` workflows never break for
contributors who haven't run `git submodule update --init`. musl license attribution (MIT) is
recorded in the repo's license notes since cargo-deny cannot see it.

*Rejected:* vendoring the tree in-repo (~11 MB + 1537 C files of history, manual upstream sync);
build-time fetch at a pinned SHA (network-dependent builds, pin outside git metadata).

### D10. C toolchain: clang `--target` + llvm-ar + rust-lld

`CC="clang --target=aarch64-unknown-linux-musl"` with clang's integrated assembler compiles musl on
both macOS (Xcode/Homebrew clang) and ubuntu CI with the same flags; `llvm-ar` archives `libc.a`;
**final links use `rust-lld`** (ships with the pinned Rust toolchain, ld.lld-compatible, GNU flavor)
so no platform linker is involved. musl is configured `--disable-shared` (no ldso — Phase 5 is
static-only). One build script (`tools/build-musl.sh`) drives configure/make into a cached sysroot
under `target/`.

*Rejected:* a dedicated cross-gcc (per-developer install, different names per platform); zig cc
(hermetic, but a third-party toolchain dependency).

**Amendment (P3c, toolchain milestone M3a).** D10 stands as the *in-tree* toolchain and remains what
CI builds, but it is no longer the only one. The toolchain program's `minixrs/llvm-minixrs` fork now
provides a real `aarch64-unknown-minixrs` target, so when a usable SDK is present at `$MINIXRS_SDK`
(default `$HOME/toolchains/minixrs`) `kernel/build.rs` builds `hello` with a single driver
invocation instead:

```sh
$MINIXRS_SDK/bin/clang --target=aarch64-unknown-minixrs \
    -O2 -Wall -Wextra -Werror -o target/hello/hello userland/hello/hello.c
```

Every part of D10 that this replaces is supplied by the patched driver from the triple alone:
`-static`, the crt objects and `-lc` from its own sysroot, both `-z` flags from D13, and
`--image-base=0x100000` from LLVM patch 0006 — which is why this flavor needs **no linker script at
all**. `rust-lld` and `llvm-ar` are not involved; the fork's `ld.lld` does the link.

Selection is three-way — SDK, then D10's in-tree sysroot, then `worker`-as-`hello` — and **D10 is
not demoted to a fallback**: no CI job installs an SDK (an LLVM build is hours) while
`tests/qemu-boot.expected` requires the five C markers, so D10 is the blocking `qemu-smoke` gate's
real dependency and has to keep working. Only the third case loses markers.

A usable SDK that fails to build **panics** rather than falling through to D10, because the boot
markers are byte-identical across the two flavors: the log cannot tell them apart, so demoting would
report a regressed toolchain as a healthy build.

### D11. Scope fences

**In (post-milestone stretch):** MFS write path (5.10); `/dev/null` + `/dev/zero` via the memory
driver's CDEV minors, plus the `CDEV_READ` request reading zero needs (5.11).

**Out — deferred with owners:** PFS/pipes → **Phase 7** (first consumer is the shell; `plan.md`'s
old Phase 5 bullet moves out); TTY RX/IRQs + `SYS_IRQCTL` → Phase 6; indirect grants → when a
re-granting consumer exists; SENDA → unchanged non-goal; more signals/handlers → Phase 5.x+ as
needed (musl hello needs none); threads (real futex, `syscall_cp` porting) → far future; dynamic
linking/ldso → far future; malloc-backed C programs (the `mmap` wrapper is a link-satisfying stub;
hello provably pulls in no malloc) → revisit when a real program needs it, likely via `VM_MMAP` +
opening USER `ipc_to` to VM.

### D12. Milestone bar

Phase 5 closes on **exec-from-FS** (slice 5.9): every subsystem — grants, TTY/CDEV, VFS,
BDEV/ramdisk, MFS, musl, grant-sourced kernel ELF load — in one QEMU trace. The boot-embedded musl
hello (5.6) is intermediate milestone A, deliberately de-risking musl independently of the FS stack.

### D13. Derived decisions (recorded so slices don't re-litigate)

- **Request bands:** VFS `VFS_RQ_BASE = 0x800`; VFS↔FS protocol `FS_RQ_BASE = 0x900`; block devices
  `BDEV_RQ_BASE = 0xA00`; character devices `CDEV_RQ_BASE = 0xB00`. All below `NOTIFY_MESSAGE`, each
  with the conventional `const _` ordering guards and host tests.
- **USER priv:** `populate_user_priv`'s `ipc_to` widens `{PM}` → `{PM, VFS}` (POSIX shape: user
  procs talk to PM and VFS, nothing else).
- **exec initial stack:** the kernel builds a **Linux-SysV frame** —
  `[argc][argv…][NULL][envp…][NULL][auxv pairs][AT_NULL]` with minimal auxv (`AT_PAGESZ`,
  `AT_PHDR`/`AT_PHNUM`/`AT_PHENT` when the first PT_LOAD maps the headers, `AT_NULL`) — so musl's
  crt/`__libc_start_main`/`__init_tls` run **unpatched**. Keep-the-musl-diff-minimal is the standing
  principle: the fork's delta stays `arch/aarch64/syscall_arch.h` (gutted) + `src/minix/` (new) +
  build glue.
- **User-binary link contract:** `-z max-page-size=4096 -z separate-loadable-segments` (page-aligned
  `p_offset` — the kernel loader stays strict rather than learning offset slack).
- **musl wrapper set for milestone A:** real `writev`/`write` (→ `VFS_WRITE`), `exit`/`exit_group`
  (→ `PM_EXIT`), `set_tid_address` (constant tid), `ioctl` (→ `-ENOTTY`, harmlessly forcing full
  buffering); link-satisfying stubs for `close`, `lseek`, `ppoll`, `openat`, `mmap`, `futex`. Real
  `open`/`read`/`close` wrappers follow in 5.8+.
- **`fs/mfs` shape:** library half (`no_std` on-disk structs + pure superblock/inode/dirent/zone
  logic, host-tested, consumed by `tools/mkfs-mfs`) + server bin half (SEF/IPC glue) — the
  measured-submodule Sonar convention (`fs/**/src/main.rs` joins `sonar.coverage.exclusions`; the
  lib joins the CI miri list).
- **Path resolution simplification:** VFS sends MFS whole paths; MFS resolves internally. MINIX's
  component-at-a-time `REQ_LOOKUP` protocol is deliberately simplified for a single-FS, root-only
  world; revisit when mounts/multiple FSes arrive (Phase 6+).
- **FS request subset:** READSUPER, LOOKUP, PUTNODE, READ, STAT-lite; GETDENTS only if free.
  Write-side requests arrive with 5.10.

---

## Slice decomposition

Ordering rationale: **ABI prep first** — the errno renumber lands before any slice bakes an errno
value into a trace marker or a line of C, so nothing is ever re-touched over a value change;
copy-safety next (everything after touches user memory); grants third (TTY, CDEV, BDEV, FS, exec all
consume them); console fourth (every later slice gains visible EL0 output); musl **before** the FS
slices (the root image must contain `/bin/hello`, so musl must build before `mkfs-mfs` packs an
image); FS next; exec-from-FS closes the milestone; stretch slices after.

### Slice 5.0: errno renumber + `tools/gen-c-headers` ✓ shipped (PR #40, merged 2026-07-25)

**Goal:** D7 + D8 — the ABI is C-ready before any other Phase 5 work, so every later slice writes
final errno values into its markers and code from day one.

**Scope:** renumber `error.rs` per D7 (classic/Linux POSIX block, 200-band MINIX extras, add the
missing FS errnos); sweep the workspace for hardcoded errno literals (host tests + boot markers are
the net — the current `qemu-boot.expected`/`.forbidden` carry no errno literals, so marker churn is
zero); fix the errno policy comment. New `tools/gen-c-headers` host crate (workspace member)
emitting the D8 headers to a target directory; a host test snapshots the generated `message` struct
layout against `Message`'s const asserts. Deliberately mechanical and isolated, like the chunk-5
toolchain bump.

**Proof:** QEMU boot markers green (values changed, behavior identical); `cargo gen-c-headers`
output compiles under `clang -std=c11 -fsyntax-only` (wired into CI as the blocking `c-headers` host
check in this slice).

**As built** (differences from the sketch above, recorded so 5.6 does not rediscover them):

- The POSIX block is the **full contiguous `1..=40`** rather than D7's named subset — classic MINIX
  and musl agree on all forty, so no later slice has to come back and add one more errno. `error.rs`
  now defines its constants through a small `errnos!` macro that also emits `pub const ALL: &[(&str,
  i32)]`; that table is the single source of truth for the header generator, the compile-time band
  guards, and the host tests. `EBADSRCDST` takes 216 (modern MINIX spells that condition `EBADEPT`),
  the one name the reference tree lacks.
- The package is **`minixrs-gen-c-headers`** (workspace `minixrs-*` convention), invoked through the
  new `cargo gen-c-headers` alias.
- Four headers plus two check artifacts: `include/minix/{ipc,com,callnr,errno}.h`, a CI-only
  `abi-check/errno.h`, and `abi-selftest.c` — a header is never a translation unit, so without the
  selftest none of the `_Static_assert`s would ever fire.
- `minix/ipc.h` includes **nothing**: `offsetof` comes from `__builtin_offsetof` under the private
  name `_MINIX_OFFSETOF`. Apple's clang redirects `<stddef.h>` to the system header for any `*-musl`
  triple, which breaks a hermetic sysroot-less check; and an ABI header should be includable from
  freestanding C anyway.
- **Errno verification is genuinely deferred to 5.6.** `minix/errno.h` defines only the MINIX
  200-band and puts the forty POSIX assertions behind `#ifdef MINIX_ABI_CHECK_POSIX_ERRNO`, because
  CI has no musl sysroot and a host `<errno.h>` has different values (Darwin `EDEADLK` is 11). CI
  compiles that block against the generated stand-in, which proves the syntax and the macro
  spellings but **not** the values; `tools/build-musl.sh` must define `MINIX_ABI_CHECK_POSIX_ERRNO`
  in 5.6 to make the value check real.
- Nominated for 5.1 (both deliberately out of scope here): renaming `NR_KERN_CALLS_PHASE4` — the
  header already emits it as `NR_KERN_CALLS` with a provenance comment, and the C name should not
  diverge past the 5.6 freeze — and the nine `sef.receive(&mut msg) != 0` sites that should read `!=
  OK`.

#### Design record

`kernel-shared/src/error.rs` ended up with two bands (D7). The **POSIX block, magnitudes `1..=40`**,
sits at classic book-era MINIX values, which are identical to Linux's and musl's — that identity is
the whole point, because it is what lets musl's stock `bits/errno.h` and `syscall_ret.c`'s `r >
-4096UL` window work unmodified. The **MINIX-specific IPC band at `>= 200`** takes modern MINIX 3
`sys/sys/errno.h` values (`ENOTREADY` 201, `EDEADSRCDST` 202, `EDONTREPLY` 203, `EGENERIC` 204,
`ELOCKED` 208 …) plus `EBADSRCDST` 216, the one name modern MINIX lacks — it spells that condition
`EBADEPT`. Nothing may land in the `41..=199` gap, where musl defines errnos minix.rs has *not*
adopted.

Constants are stored **negated**. The `errnos!` macro takes the positive magnitude and emits both
the `pub const` and `pub const ALL: &[(&str, i32)]`; `ALL` is the single source of truth for the
header generator, the `const _` band and distinctness guards, and the host tests.

`tools/gen-c-headers` (package `minixrs-gen-c-headers`) is the **first host/`std` crate in the
workspace**: the lib holds the renderers and their tests, `main.rs` is argv and IO only and is
Sonar-coverage-excluded via `tools/**/src/main.rs`. It prints
`include/minixrs/{ipc,com,callnr,errno}.h` from the live constants, and those headers are a build
artifact under `target/` that is **never committed** (D8 — drift is impossible by construction, and
CI asserts nothing was written into the tree).

Four properties of the generated C were chosen deliberately and must survive any extension:

- It uses **C11 keywords** — `_Static_assert`, `_Alignas` on the struct's *first member* (C11
  forbids it on a type declaration), `_Alignof` — never GNU attributes.
- `minixrs/ipc.h` **includes nothing**. `offsetof` comes from `__builtin_offsetof` under the private
  name `_MINIXRS_OFFSETOF`, because Apple's clang redirects `<stddef.h>` to the system header for
  any `*-musl` triple and would break the hermetic `-nostdlibinc -ffreestanding` CI check.
- Every process gets **both** `<NAME>_PROC_NR` and `<NAME>_EP`. They differ for kernel tasks —
  `SYSTEM_PROC_NR` is −2, `SYSTEM_EP` is 32766 — and the header `_Static_assert`s the C decode macro
  against the Rust-computed endpoints.
- `minixrs/errno.h` **asserts but never defines** the POSIX block, behind `#ifdef
  MINIXRS_ABI_CHECK_POSIX_ERRNO`, since CI has no musl sysroot and a host `<errno.h>` disagrees
  (Darwin `EDEADLK` is 11). Slice 5.6's `tools/build-musl.sh` defines that macro and makes the value
  check real.

`abi-selftest.c` exists because a header is never a translation unit: without a `.c` file none of
the `_Static_assert`s would ever fire.

**The trap this slice left behind**, discovered in 5.10a: unlike `error.rs`'s `ALL`, the per-band
`members` lists in `callnr_h.rs`'s `bands()` are **hand-maintained**, so bumping `NR_*_MSGS` without
adding the row leaves the constant silently absent from the generated header — and the `c-headers`
CI gate still passes, because it compiles a header that simply never mentions it. The crate's own
`every_band_member_list_matches_its_count` test is what catches this, and it did.

(The standing rules are in [abi.md](../conventions/abi.md#errno-bands-d7) and
[abi.md](../conventions/abi.md#generated-c-headers).)

### Slice 5.1: fault-safe user copy + real `SYS_DIAGCTL` ✓ shipped (PR #41, merged 2026-07-25)

**Goal:** no user pointer can panic the kernel (D5), and servers can print (D2) — the
observability + safety floor for everything after.

**Scope:** replace `copy_msg_from_user`/`copy_msg_to_user` with PT-walk + HHDM copies against the
caller's (resp. receiver's) address space; `EFAULT` to the caller's `x0` on send/kernel-call
request/reply, `EFAULT` to the receiver's parked `x0` when the deferred `flush_deliver_msg` hits a
bad deliver buffer (replacing the silent `let _ =`). `SYS_DIAGCTL` body: inline-payload text (len +
bytes in the 96-byte payload), kernel writes it to the UART with a `[diag <name>]`-style prefix;
`server-rt` gains a `diag_print` helper that chunks longer strings. Drive-bys nominated by
`phase-5-prep.md` for the first PR touching these files: stale era comments in
`kernel/src/ipc/{message.rs,senda.rs,mod.rs}`, and `ipc_const.rs`'s wrong "`x16`" trap-ABI comment
(the real register is `x1`).

**Proof:** a one-shot deliberate bad-pointer IPC from the stub battery (boot-stubs-gated) traces
`result=-14` (`EFAULT` at its final 5.0 value) with no panic; a server's `diag_print` line appears
in the boot log and joins `tests/qemu-boot.expected`.

**As built** (differences from the sketch above, recorded so later slices do not rediscover them):

- The copy engine is a **general byte-level module**, `kernel/src/mm/uaccess.rs`
  (`copy_from_user_as` / `copy_to_user_as` / `probe_user_range`, all over a raw `ttbr0_pa`), not
  message-specific helpers. Slice 5.2's grant engine builds on it directly — a cross-AS copy is two
  walks and a `memcpy`. `addrspace.rs` gains the missing member of its `*_in` free-function family,
  `walk_pt_in(ttbr0_pa, va) -> Option<(u64, Prot)>`; `AddrSpace::walk_pt` delegates to it. The
  `Prot` half is load-bearing: the kernel copies through the HHDM alias, which the MMU's EL0
  permission bits do **not** police, so `copy_to_user_as` must reject a read-only destination
  explicitly.
- **Writes are all-or-nothing** (`probe_user_range` first). A 104-byte `Message` is only 8-aligned
  and really can straddle two pages, so copy-as-you-go would leave a half-written message in a user
  buffer before returning `EFAULT`.
- The page-split arithmetic lives in `kernel-shared` as `page_chunks` / `PageChunk` /
  `USER_PAGE_SIZE` beside `user_va_ok`, with 6 host tests — the kernel crate has no `#[cfg(test)]`,
  and this is the one piece of the slice with real off-by-one risk. `kernel-shared` goes 60 → 68
  tests.
- `ipc/message.rs` ends with **zero `unsafe`**: messages stage through a `[u8; 104]` and are
  reassembled field-by-field, so every raw-pointer operation in the user-copy path is in
  `mm::uaccess` alone.
- `MF_MSGFAILED` already existed in `proc/flags.rs` (unused since Phase 2); the flush now sets *and
  clears* it, so it records "the last delivery to this proc failed" rather than being write-only.
  Nothing reads it yet — a later signals slice can turn it into `SIGSEGV`, which is what MINIX's
  `delivermsg()` does.
- Traces are **uncounted**, unlike the sampled `[ipc {n}]` form, so they are stable boot markers:
  `[efault] proc=A nr=11 call=1 va=…` (emitted in `do_ipc` keyed on `result == EFAULT`, which covers
  all three immediate copy sites at once — nothing else in the kernel produces `EFAULT`) and
  `[efault deliver] proc=A nr=11 va=…`.
- **Four** probes on stub A's prologue, not one, covering every arm of the engine: an unmapped page,
  a *page-straddling* buffer (8 bytes in A's stack page, 96 in the unmapped page above), the
  deferred-flush path, and a *mapped-but-read-only* destination (A's own code page) for the
  `Prot::writable` check. That last one was added on review: without it a regression dropping the
  writable check would still have passed every marker. Mutation-tested — stubbing the check out
  makes exactly the `va=0x400000` marker disappear. A new stub E was rejected: `NR_STUB_PROCS` feeds
  `FORK_POOL_BASE`, so a fifth stub would shift init's forked children 15 → 16 and break three
  existing markers.
- Granule and layout coupling is pinned by `const _` asserts rather than convention:
  `USER_PAGE_SIZE == FRAME_SIZE` in `uaccess.rs` (the chunker and the walker are in different
  crates), `FRAME_SIZE == PAGE_SIZE` and `1 << PAGE_SHIFT == PAGE_SIZE` in `addrspace.rs`, and
  `offset_of!`-based asserts in `ipc/message.rs` tying its `M_TYPE_OFF` / `PAYLOAD_OFF` to the real
  `Message` layout. The last is the one that matters most: a field added ahead of `payload` would
  otherwise be a *silent* miscopy (the slice length stays right), not a panic.
- `SYS_DIAGCTL` takes a **subcode** (`DIAGCTL_CODE_DIAG = 1`, MINIX 3's numbering, with 2–4 reserved
  and `EINVAL`), so text budget is `DIAG_TEXT_MAX = 88` after the subcode and length words. Text is
  sanitized to printable ASCII, which is what guarantees one call = one line and keeps the `grep
  -aF` marker contract intact.
- `diag_print` is called from **`sef_startup`**, so all six SEF servers announce themselves (`[diag
  vm] sef ready`, …) for one line of code. Two are asserted as markers. It is placed before the
  `init_fresh` callback so the line proves `SYS_DIAGCTL` independently of DS.
- Done here rather than deferred: the `NR_KERN_CALLS_PHASE4` → `NR_KERN_CALLS` rename (25
  references; the C header already emitted the un-suffixed name, and a
  `nr_kern_calls_is_not_phase_suffixed` test now forbids any phase-scoped name in the ABI header).
  The `sef.receive(…) != 0` → `!= OK` sweep was **5** sites, not the 9 slice 5.0 estimated —
  `drivers/`, `fs/` and `userland/` have no receive loops yet. VFS's `let _ = sef.receive(…)` is
  left alone: it discards every message, so the discard is the honest form.
- `tests/qemu-boot.forbidden` gains `!!! kernel exception (vector index` — the same-EL banner, which
  is precisely what a kernel dereference of a bad user pointer produced before this slice. `!!!
  KERNEL PANIC:` already caught it transitively, but this is the sharper canary.
- **Verification note for future slices:** the highest risk was *surfacing* a previously-silent
  failure — kernel-originated notifies (`deliver_alarm`, `deliver_ksig`, `mini_pf_send`,
  `send_no_quantum`) set `MF_DELIVERMSG` on a proc whose `deliver_msg_vir` came from its own last
  RECEIVE, and the old `let _ =` swallowed a bad one. The guard is a **stub-free boot**
  (`--no-default-features`) grepped for `[efault]`: it must be empty. It is.

#### Design record

The invariant this slice bought: the kernel **never dereferences a user VA**, not even the active
TTBR0's. Every byte in or out of a user address space goes through `kernel/src/mm/uaccess.rs` —
`copy_from_user_as`, `copy_to_user_as`, `probe_user_range` — which page-chunks the range, resolves
each page with the new `addrspace::walk_pt_in(ttbr0_pa, va) -> Option<(u64, Prot)>` (the missing
member of the `map_page_in` / `unmap_page_in` / `walk_leaves` free-function family;
`AddrSpace::walk_pt` delegates to it), and `memcpy`s through the frame's HHDM alias.

Four consequences that later slices build on rather than re-derive:

- An unmapped page is a walk miss returning **`EFAULT`, not an EL1 abort** — so **no exception-fixup
  table is needed or wanted**. D5 rejected the Linux `extable` approach on exactly this ground.
- The copy is **address-space-independent**, so `flush_deliver_msg` can write a receiver's buffer
  before its TTBR0 is installed. The TTBR0-before-flush ordering in `sched::schedule_next` is no
  longer load-bearing; only `eret` needs it.
- `copy_to_user_as` must **check `Prot::writable`**, because the HHDM alias is a kernel mapping that
  the MMU's EL0 permission bits do not police.
- Writes are **all-or-nothing** (probe first), since a 104-byte `Message` is only 8-aligned and
  really can straddle two pages.

`user_va_ok` stays as the cheap range/alignment pre-gate; the page arithmetic (`page_chunks`,
`PageChunk`, `USER_PAGE_SIZE`) lives in `kernel-shared/src/message.rs` beside it, because the kernel
crate has no `#[cfg(test)]`. `ipc/message.rs` now carries **zero `unsafe`** — messages stage through
a `[u8; 104]` — so all raw-pointer work is in `mm::uaccess` alone.

Error routing: `EFAULT` reaches the caller's `x0` via `do_ipc` for the three immediate sites, and a
blocked receiver's **parked `x0`** (`p.regs.x[0] = e as i64 as u64`, the MINIX `retreg` idiom
`do_exit::unblock_dependents` uses) for the deferred flush, which also sets and clears the
previously-unused `MF_MSGFAILED`. Both traces are **uncounted** — `[efault] proc=… nr=… call=… va=…`
and `[efault deliver] proc=… nr=… va=…` — so they are stable boot markers, unlike the sampled `[ipc
{n}]` form. The `do_ipc` one keys on `result == EFAULT`, which is unambiguous because nothing else
in the kernel produces it.

**`SYS_DIAGCTL` is the servers' debug channel** (D2). Servers run at EL0 with no console, and this
had to keep working while grants, TTY and VFS were under construction — so the text rides **inline
in the payload** (subcode `0..4`, len `4..8`, up to `DIAG_TEXT_MAX = 88` bytes from `DIAG_TEXT_OFF =
8`) and needs no user-copy machinery at all. `kernel/src/system/do_diagctl.rs` prints one line per
call as `[diag <name>] <text>`, where `<name>` is the caller's kernel-known `Proc::name` — never
payload data, so a server can only identify itself — and the text is sanitized to printable ASCII,
so one call is always exactly one line, which the `grep -aF` marker contract depends on.
`server-rt::diag_print` is the client, chunking longer strings; it is called from `sef_startup`, so
all six SEF servers emit `[diag <name>] sef ready` at boot.

Adding a stub is discouraged from here on: `NR_STUB_PROCS` feeds `FORK_POOL_BASE`, so a fifth stub
shifts init's forked children 15 → 16 and breaks checked-in markers. New probe behaviour goes in an
existing stub's prologue — 5.1 used stub A's.

(The standing rules are in [kernel.md](../conventions/kernel.md#fault-safe-user-access) and
[kernel.md](../conventions/kernel.md#how-a-server-prints-sys_diagctl).)

### Slice 5.2: grant table + `SYS_SETGRANT` / `SYS_SAFECOPY` / `SYS_COPY` ✓ shipped (PR #42, merged 2026-07-25)

**Goal:** the D4 grant model, live end-to-end between two boot servers.

**Scope:** `kernel-shared`: `GrantEntry` (`#[repr(C)]`), CPF flags, idx+seq grant-id packing, host
tests. `server-rt`: static grant table (`UnsafeCell` newtype, the `vm/region.rs` pattern),
`cpf_grant_direct` / `cpf_grant_magic` / `cpf_revoke`, lazy one-time `SYS_SETGRANT` registration.
Kernel: `do_setgrant` (record after `user_va_ok`), `verify_grant` (read the entry from the granter's
AS via PT walk; validate kind/access/grantee/range/ seq; magic gated on server-grade priv), the
page-at-a-time cross-AS copy engine, `do_safecopy` (direction flag in the payload — one call number
covers from/to), `do_copy` (raw privileged copy on the same engine). Indirect grants: documented
`EINVAL`.

**Proof:** VFS direct-grants a checksummed buffer at init and publishes the grant id through DS
(`grant.test` key — DS is already a name→i32 registry); PM retrieves it, `SYS_SAFECOPY`s the buffer,
and `diag_print`s the checksum. Marker line in `qemu-boot.expected`; `[ksys]` traces show the new
calls.

**As built** (differences from the sketch above, recorded so later slices do not rediscover them):

- **The grant id travels in-band, not through DS.** `DS_PUBLISH` deliberately registers the
  kernel-stamped `m_source` and ignores the payload — that is its anti-spoof property — so it cannot
  carry an id at all, and an init-ordering handoff through DS would race besides. VFS `ipc_send`s PM
  a `PM_GRANT_TEST` message carrying `{gid, len, rw_gid, addr}` instead. SEND blocks until PM's loop
  receives, so the demo is self-synchronizing; and this is the shape a grant id really travels in
  (slice 5.3's `CDEV_WRITE {minor, grant_id, len, offset}` is the same, and takes its granter from
  `m_source` for the same reason). Cost: one demo-only PM request number (`PM_GRANT_TEST`,
  `NR_PM_MSGS` 5 → 6), to retire when a real consumer lands.
- **The granter is `m_source`, never a payload field** — and the same rule binds every later band
  that carries a grant id. Review caught the first draft reading the granter out of the payload: PM
  holds `SYS_COPY` / `SYS_SAFECOPY` and its clients (init, every forked child on the shared USER
  privilege) hold neither, so a caller-supplied granter endpoint would let any of them aim a
  privileged cross-address-space copy at a third party *through PM* and read a checksum of the
  result back off the console — a confused deputy. Taking the granter from the kernel stamp means a
  client can only ever name its own address space. PM additionally serves the demo request only when
  `m_source` is VFS. **CDEV/BDEV/FS must not reintroduce a payload granter field.**
- **The magic arm gets a live QEMU proof**, not just host tests: PM magic-grants init's text page
  (`0x0010_0000`, the load base every user binary shares) to itself and safecopies 8 bytes out of
  init's address space — a genuine third-party read from a process that granted nothing. Uses only
  init's endpoint, which PM already holds. Without it the arm would be dead code until 5.5. The line
  reports only the length; init's `_start` bytes change on rebuild.
- **`SYS_COPY` is proved in the same exchange**: PM re-reads the *same* bytes from VFS's raw address
  with no grant and compares checksums (`copy ok match=1`). That comparison is what proves the grant
  path moved the right bytes rather than merely some bytes.
- **`server-rt` keeps `#![forbid(unsafe_code)]`.** `GrantPool<const N>` is a *value* the server owns
  (a `main`-frame local that outlives the receive loop), not a static — so there is no `UnsafeCell`
  and no `unsafe impl Sync`. It also makes registration self-healing: `ensure_registered` compares
  the pool's live address against the one last registered and re-issues `SYS_SETGRANT` when they
  differ, and both taking a raw pointer and casting it to an integer are safe operations. `revoke`
  re-registers too (review catch): clearing a slot only revokes anything if the kernel is reading
  *that* copy of the table, so a moved pool would otherwise clear its entry while the kernel kept
  honouring the stale address — a revocation that silently does nothing is the worst failure this
  type has. The pool must be built in `main`, not `init_fresh` — that frame is gone by the time a
  grantee safecopies.
- **Magic is gated on `Priv::flags & SYS_PROC`**, minix.rs's variant of MINIX's hardcoded "only VFS
  and MIB may issue magic grants" — the same trust boundary with no proc-nr list to keep in sync.
- **`SYS_SETGRANT` rejects a shared privilege slot** (`priv.proc_nr != Some(caller)` → `EPERM`).
  Unreachable today (the shared `USER_PRIV_ID` has an empty `k_call_mask`), but one table address
  cannot describe several processes' memory, and this is what stops a future grant-capable user
  class inheriting the hole. **Both `do_exit` and `do_exec`** clear `grant_table` / `grant_entries`
  on a dedicated slot: exit so a recycled server slot cannot inherit a stale table address, and exec
  — caught in review — because exec preserves the privilege slot while replacing the address space,
  so the registered VA would otherwise describe the discarded image and aim `verify_grant` at
  whatever the new image maps there. The exec arm is unreachable in this slice's boot (the only
  exec'ing procs are forked children on the shared USER privilege, which the `proc_nr` guard skips)
  and becomes live the first time a server execs.
- `kernel-shared` gained **`user_range_ok(va, len)`** beside `user_va_ok`: a granted range is a byte
  buffer and need not be 8-aligned, so the message-grade predicate is the wrong gate for it.
  Bounding the range is also what stops `page_chunks` being handed a 2^64 length. `user_va_ok` is
  still right for `SYS_SETGRANT`, whose table *is* 8-aligned.
- **Seven denial probes, not one demo line.** `[diag pm] grant.deny ok n=7` covers wrong grantee,
  wrong access, stale sequence, out-of-range index, range overrun, a granter *lying* about
  writability over `.rodata`, and a grant over an unmapped page. Each is constructed so every check
  but its target passes. Mutation-tested one at a time: removing the grantee / seq / access / range
  check, or `copy_between_as`'s destination-writable probe, each flips the line to `grant.deny FAIL
  <name>`. Two findings from that exercise:
  - the **index range check is not independently observable** — an id past the table makes the entry
    read fail first (unmapped page, or a slot without `CPF_USED`), so it is defence in depth rather
    than the observed cause;
  - the **`SYS_PROC` magic gate cannot be probed at all** in this slice, because every process able
    to call `SYS_SETGRANT` is server-grade. Inverting the gate was used instead to prove it is
    evaluated on the live path (`magic ok` → `magic FAIL rc=-1`). A non-server granter arrives with
    the musl slices; the real negative probe belongs there. `CPF_INDIRECT` is likewise unprobed —
    `GrantPool` has no API to mint one. The `.rodata` probe is the one that needed new plumbing (VFS
    issues a second, deliberately-lying `CPF_READ | CPF_WRITE` grant over the same read-only buffer)
    and is the grant-path analogue of slice 5.1's fourth bad-pointer probe: the kernel copies
    through the HHDM alias, where EL0 permission bits do not apply, so `Prot::writable` is the only
    thing between a lying granter and a corrupted `.rodata` page.
- `dual_page_chunks` lives in `kernel-shared::message` beside `page_chunks` for the same reason (the
  kernel crate has no `#[cfg(test)]`), with 8 host tests including the case both sides straddle at
  *different* offsets — a 128-byte copy that takes three chunks. `kernel-shared` goes 68 → 91 tests,
  `server-rt` 12 → 25.
- Traces are head-carved (6) like `do_vmctl`'s, not sampled: these are low-rate callers the `[ksys
  N]` every-100th sampler would never catch.

#### Design record

The shape D4 settled on: a granting process keeps its `GrantEntry` table **in its own address
space** and registers `(addr, entries)` with `SYS_SETGRANT`. `SYS_SAFECOPY` reads the entry back out
of the *granter's* address space on every call, so a granter revokes by writing its own memory and
the kernel caches nothing that could go stale.

The ABI is `kernel-shared/src/grant.rs`: a flat `#[repr(C)]` `GrantEntry`
(flags/seq/who_to/who_from/addr/len, 32 bytes, layout pinned by `offset_of!` asserts because the
kernel decodes it from raw bytes), MINIX's CPF flag values, and `GRANT_SHIFT = 20` id packing
(`grant_id` / `grant_idx` / `grant_seq`).

The engine is one new function beside the 5.1 trio: `mm::uaccess::copy_between_as(src_ttbr0, src_va,
dst_ttbr0, dst_va, len)` probes both ranges, then walks and `memcpy`s per `dual_page_chunks` —
**all-or-nothing on the destination**. Its `Prot::writable` check is load-bearing: a granter may
*lie* about writability, and the HHDM alias is a kernel mapping the MMU's EL0 bits do not police.

`verify_grant` (`system/do_safecopy.rs`) checks, in MINIX's order: `okendpt` the granter; table
registered and idx in range; the entry reads back (a walk miss is hidden as **`EPERM`, not
`EFAULT`**, so a grantee cannot probe the granter's address space); `CPF_USED|CPF_VALID`; `seq`
matches the id; requested access ⊆ flags; `who_to` == the caller's *stored* endpoint; `offset +
bytes <= len`. `CPF_DIRECT` resolves the memory to the granter; `CPF_MAGIC` resolves it to
`who_from` and additionally requires the granter's `Priv::flags & SYS_PROC` (minix.rs's stand-in for
MINIX's hardcoded VFS/MIB list); `CPF_INDIRECT` is a documented `EINVAL`.

`SYS_COPY` is the same engine with no grant — `resolve_target` on both endpoints (`SELF` works),
`k_call_mask` is the only gate, the `do_vmctl` trust stance. All three route from the
**target-taking `match`** in `kernel_call_dispatch`, `SYS_SETGRANT` included: it is caller-local in
effect but needs `&mut Priv`, which `dispatch_caller_local` cannot hand out. `SYS_SETGRANT` also
**rejects a shared priv slot** (`proc_nr != Some(caller)` → `EPERM`), because one table address
cannot describe several processes' memory; and **both `do_exit` and `do_exec`** clear a dedicated
slot's registration — exit so a recycled slot cannot inherit a stale table address, exec because the
registered VA describes the image being discarded while the privilege slot survives.

Client side, `server-rt::GrantPool<const N>` is a **value the server owns** (a `main`-frame local,
never `init_fresh`'s frame), which is what keeps `server-rt` `#![forbid(unsafe_code)]`;
`ensure_registered` compares the pool's live address against the last registered one and re-issues
`SYS_SETGRANT` if they differ. Byte buffers use the new `kernel-shared::message::user_range_ok`,
which has no alignment requirement, unlike `user_va_ok`, which stays right for the 8-aligned grant
table. Traces are head-carved (6) like `do_vmctl`'s — these are low-rate callers the `[ksys N]`
sampler never catches.

The live demo is VFS → PM (`PM_GRANT_TEST`, demo-only, retired when CDEV landed): a direct grant,
the same bytes again via `SYS_COPY` for a checksum comparison, a magic read out of init's text page,
and **seven denial probes** (`[diag pm] grant.deny ok n=7`) that keep every validator exercised by a
boot marker.

(The standing rules — the grant ABI, and the confused-deputy rule that the granter is always the
kernel-stamped `m_source` — are in [abi.md](../conventions/abi.md#grant-abi-d4),
[kernel.md](../conventions/kernel.md#grants--the-governed-cross-address-space-copy) and
[servers-and-drivers.md](../conventions/servers-and-drivers.md#the-granter-is-m_source-never-a-payload-field).)

### Slice 5.3: TTY driver (TX-only, premapped PL011) + CDEV band ✓ shipped (PR #43, merged 2026-07-25)

**Goal:** D1 — first user-space driver; EL0-originated text on the serial console.

**Scope:** the `map_page_in` free-function family grows a device mapping mode via `Prot.device` (not
a new `AddrSpace` method — the whole family already takes a `ttbr0_pa`); boot pre-maps the UART page
into TTY's AS at `kernel-shared::uspace::TTY_UART_VA`. New `drivers/tty` crate (workspace member,
`user.ld`, SEF loop, DS publish; polls `FR.TXFF`, writes `DR`, LF→CRLF like the kernel writer).
`CDEV_RQ_BASE = 0xB00` with `CDEV_WRITE {minor, grant_id, len, offset}` → TTY safecopy-reads and
transmits; replies bytes-written. **No payload `granter`** — the driver takes it from the
kernel-stamped `m_source`, the 5.2 confused-deputy rule. `kernel/build.rs` `servers` array +1
(proc_nr 4, at index 2 so the console is serving before its first client); `qemu-boot.expected`
gains the `[as]` line and the demo markers. `CDEV_READ` was absent until 5.11 defined it for
`/dev/zero`; TTY serves it in Phase 6.

**Proof:** VFS retrieves TTY's endpoint from DS and `CDEV_WRITE`s a banner via direct grant — the
banner reaches serial *from EL0* (no kernel trace prefix), distinguishable from every other line in
the log. Plus a short write (`CDEV_MAX_IO + 8` requested, `CDEV_MAX_IO` returned) and two denial
probes. VFS rather than PM/RS because it is already the 5.2 granter, already owns a `GrantPool`, and
5.4 puts its fd 1/2 on this exact path.

**As built** (differences from the sketch above, recorded so later slices do not rediscover them):

- **The kernel never writes `MAIR_EL1`** — it *reads* it for an index that already encodes a Device
  type (`mmu::init_device_attr_idx`). Writing byte *i* would retroactively retype every live mapping
  using `AttrIndx=i`, including Limine's TTBR1 kernel and HHDM mappings, whose indices this repo
  cannot enumerate; turning the HHDM into Device memory is silent, unrecoverable corruption.
  Read-and-reuse is sufficient because an *unprogrammed* MAIR byte reads `0x00`, which is itself a
  valid MMIO encoding (Device-nGnRnE) — so "an index already encoding Device" and "an index nobody
  uses" coincide. Only `0x04` (nGnRE, preferred per D1) and `0x00` (nGnRnE) qualify: nGRE would let
  two `DR` stores gather into one lost character, GRE would let the `DR` store pass the `FR` poll.
  On QEMU/Limine today the scan picks **index 1, byte `0x00`** (`[mair] device attr_idx=1
  byte=0x00`) — forensic only, deliberately *not* a boot marker, since the value is
  firmware-dependent. The write-MAIR fallback recipe (program the high index `mair[7]`, then `msr /
  isb / dsb ish / tlbi vmalle1is / dsb ish / isb`) lives in the panic message so it is not
  re-derived.
- **`Prot` grew a `device` field, and the invariant became total.** Adding the field is a compile
  error at every struct literal, which is the point — there were exactly two (`pte_prot` and
  `do_vmctl`'s `pt_map`). `map_page_in` now asserts `prot.device != mm::is_usable_pa(pa)` on every
  leaf, so each mapped leaf is provably `(RAM ∧ ¬device)` or `(device ∧ ¬RAM)` with no third case.
  That lemma is what makes `if !prot.device { free_frame(…) }` *sound* rather than merely plausible,
  and it is why `free_frame` keeps its loud bounds assert: a catch-all that silently skipped non-RAM
  frames would demote a forged-PA or double-free bug into an untraceable leak. `pte_prot` decodes
  `device` **statelessly** (AttrIndx ≠ `ATTR_IDX_NORMAL`), which is sound because the kernel emits
  only two indices and byte 0 is pinned to Normal-WB.
- **Five leaf sweeps needed the guard, not four.** The plan listed `do_exit::teardown_addrspace`,
  `do_fork::copy_addrspace`, `do_vmctl`'s `pt_unmap`, and `userland::destroy_addrspace_with_leaves`;
  `copy_addrspace`'s *out-of-memory unwind* sweep is a fifth, distinct from its copy loop. In
  `copy_addrspace` a device leaf is **re-mapped, not copied** — MMIO is inherently shared, and a
  `memcpy` of 4 KiB of live device registers through a cacheable HHDM alias would read
  side-effecting registers into RAM. `mm::uaccess` also rejects a device leaf as copy source *or*
  destination (`EFAULT`, via the new `resolve_copyable`), closing the hole where a server grants its
  own UART window.
- **The teardown selftest is load-bearing, not decoration.** TTY never exits, so the `prot.device`
  arm would be dead code sitting on a live landmine — and a missing guard is a *kernel panic*, not a
  leak. `userland::device_teardown_selftest` therefore builds a throwaway address space with exactly
  one device leaf and tears it down at boot, unconditionally (so `--no-default-features` covers it),
  costing four table frames once. It asserts **both** counts: `freed=0 devs=1` — a guard that
  skipped every leaf would report `freed=0 devs=0`. It also widened `do_exit::teardown_addrspace`
  from `pub(super)` to `pub(crate)` (and `system::do_exit` from a private module to `pub(crate)
  mod`), and changed the return type to `(freed, devs)`, which adds `devs=` to the `[ksys SYS_EXIT]`
  trace.
- **The pre-map lives in `load_boot_server`, not `load_exec_image`.** The latter is shared with
  `system::do_exec`, so putting it there would hand a device window to every binary any process ever
  exec'd. Consequence worth knowing: a proc that exec'd would *lose* its device window, which is why
  `do_exec` drops the device-leaf count rather than tracing it.
- **No TLB maintenance on the pre-map**, for two independent reasons: the address space was built
  moments ago and has never been installed in TTBR0 (and a recycled ASID is always clean —
  `teardown_addrspace` flushes before `free_asid`), and `switch_ttbr0_with_asid`, which runs on
  TTY's first schedule, already issues `isb; tlbi aside1; dsb ish; isb`. Same reasoning the
  `SERVER_STACK_VA` mapping already relies on.
- **`0x4000_0000` is a whole L1 slot, and VM's arena is now capped.** The device window
  (`kernel-shared::uspace`, a new module — it is an *address* ABI, not a message one) is 1
  GiB-aligned and 16 MiB wide, clear of every occupied user VA. A `const _` on the region *bases* is
  **not enough** on its own — it proves only where each starts, and two of them grow on request:
  VM's mmap arena is a bump cursor, and `set_brk` raises the heap's end to whatever the client asks
  for. So **both** now carry a runtime `region::REGION_LIMIT` check returning `ENOMEM` (review
  caught `set_brk` missing it after `mmap` got one — capping one and not the other was plainly
  inconsistent). Nothing is exploitable today, but the window's purpose is to be kernel-owned in
  *every* address space, so Phase 6 can pre-map a device page into any driver without asking whether
  VM already promised that VA to the process's heap. Adding the cap obsoleted one existing
  assertion: `set_brk_overflow_is_einval_not_wrap`'s control case (largest break that still aligns)
  flipped `Ok` → `ENOMEM`, which is a *stronger* no-wrap witness — a wrapped `end` would have been
  small, sailed under the cap, and returned `Ok`, so the two rejection reasons must stay
  distinguishable. These constants are deliberately not emitted into the generated C headers — no
  Phase 5 C touches them.
- **Extracting `is_usable_pa` out of `free_frame` introduced aliasing UB**, caught in review.
  `free_frame` held `&mut *ALLOC.0.get()` across the call, and `is_usable_pa` takes its own
  `&*ALLOC.0.get()` — two live references to one object with one of them exclusive, which `noalias`
  on the `&mut` makes the kind a compiler may act on. (The inline loop it replaced was a *reborrow*
  of the existing `&mut`, which was fine.) Fix: hoist the check above the `&mut`, which is also
  better ordering — validate before acquiring. **General rule for this codebase:** every static
  table is an `UnsafeCell` newtype, so extracting a read-only loop into a shared-borrow helper is
  never a pure refactor — check what `&mut` is live at each call site. Same borrow-ending discipline
  as `sched::rts_set`/`rts_unset`.
- **Helpers were lifted into `server-rt`** rather than copied into a sixth crate: `payload.rs`
  (`rd_i32`/`wr_i32`/`rd_u64`/`wr_u64`/`buf_addr`, measured and host-tested, replacing copies in
  PM/DS/VM/SCHED), `kcall.rs` (`sys_safecopy`/`sys_copy`, out of PM, coverage-excluded), `diag_fmt`
  (PM's private `diag_line`, promoted), and `sef_retrieve_from_ds`. The accessors use `checked_add`
  for the offset: servers ship `--release` with `overflow-checks = false`, where `off + 4` on a huge
  offset *wraps* and happens to be safe by accident while panicking under `cargo test`.
- **The DS lookup falls back on purpose.** DS publish-before-retrieve is *not* deterministic by
  construction: it works because `build.rs` packs TTY before VFS, so TTY's `DS_PUBLISH` reaches DS's
  FIFO first. Rather than let archive ordering become load-bearing, VFS falls back to
  `boot_endpoint(TTY_PROC_NR)` and emits a distinguishable `cdev.ds FAIL` line — so a boot where the
  ordering shifted still produces the rest of the proof, while the required `cdev.ds ok` marker
  disappears and CI goes red on that regression specifically.
- **A driver replies to an unknown `m_type`; a server may drop it.** DS drops one harmlessly because
  nothing SENDRECs it in anger, but a driver's clients all SENDREC, and a dropped request blocks the
  caller forever. Every path out of TTY's dispatch replies. A negative `SYS_SAFECOPY` result is
  relayed **verbatim** — `EPERM` ("bad grant") and `EFAULT` ("unmapped buffer") are different client
  bugs.
- **PL011 register offsets are deliberately duplicated** between `kernel/src/arch/aarch64/uart.rs`
  and `drivers/tty/src/pl011.rs`. They cannot be shared: the kernel crate is bare-metal-only and
  pinned by `forced-target`, so it can never be a user-space dependency — and a register layout is a
  hardware fact, not a shared ABI, so `kernel-shared` is the wrong home. Noted in both files.
  `drivers/tty` also deliberately does *not* depend on `minixrs-driver-rt` (a 4-line placeholder);
  Phase 6 makes that move, together with adding `drivers/driver-rt/src` to `kernel/build.rs`'s
  shared watch list.

**Honest gaps** (the 5.2 precedent for the unprobeable magic-`SYS_PROC` gate):

- **Under TCG the Device *attribute* is not observably load-bearing.** QEMU's PL011 works fine
  through a Normal-WB mapping — which is why the kernel's own HHDM alias has always been one.
  Substituting `ATTR_IDX_NORMAL` changes no marker. The attribute is proved by construction and
  assertion (`prot_attrs`' hard assert, the MAIR scan's encoding whitelist), not empirically. Same
  for the `FR.TXFF` poll (TCG's FIFO never fills) and LF→CRLF (`grep -aF` markers cannot express
  `\r`).
- **`copy_addrspace`'s and `pt_unmap`'s device arms are defense-in-depth.** Neither is reachable
  today: nothing forks TTY, and VM has no reason (or `ipc_to` edge) to unmap a driver's device
  window. `map_page_in`'s RAM/device assert is the compensating total invariant, and the boot
  selftest covers the one arm that *is* a live hazard.

**Mutation tests run** (each applied, observed, reverted):

| Mutation                                 | Observed                                                                                                                                                                                                                                                                                                                                                                                                                       |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Delete the `nr == TTY_PROC_NR` pre-map   | 5 markers vanish (`[devmap] tty`, the banner, all three `cdev.*` results). **Not** a `!!!` banner as first predicted: the store faults at `far=0x40000018` (`TTY_UART_VA + FR_OFFSET` — the flag-register poll), which is a *handled* fault routed to VM, whose out-of-region arm raises `SYS_KILL(SIGSEGV)`; PM then terminates TTY, and VFS blocks forever in its `CDEV_WRITE` SENDREC. Checker FAILs on the missing markers |
| Drop `teardown_addrspace`'s device guard | `!!! KERNEL PANIC: free_frame: PA 0x9000000 is outside all USABLE regions`, at boot, from the selftest                                                                                                                                                                                                                                                                                                                         |
| Read the granter from a payload field    | `cdev.write ok match=1` → `cdev.write FAIL rc=-1` (`EPERM`)                                                                                                                                                                                                                                                                                                                                                                    |
| Remove TTY's minor check                 | `cdev.deny ok n=2` → `cdev.deny FAIL bad-minor rc=35` (the write *succeeded* on minor 7)                                                                                                                                                                                                                                                                                                                                       |
| Halve the `CDEV_MAX_IO` clamp            | `cdev.short ok n=256` → `cdev.short FAIL rc=128`                                                                                                                                                                                                                                                                                                                                                                               |
| Reply `OK` instead of the byte count     | `cdev.write ok match=1` → `cdev.write FAIL rc=0`                                                                                                                                                                                                                                                                                                                                                                               |

#### Design record

The first user-space *driver*. `drivers/tty` is a workspace member packed at **index 2** of
`kernel/build.rs`'s `servers` array, between `ds` and `vfs`, so the console is in its receive loop
before its first client; it owns the PL011 and serves `CDEV_WRITE`.

**Device memory comes from reading `MAIR_EL1`, never writing it.** Writing byte *i* retroactively
retypes every live mapping using `AttrIndx=i`, including Limine's TTBR1 kernel and HHDM mappings,
whose indices this repo cannot enumerate. `mmu::init_device_attr_idx` (called once from
`userland_bootstrap`, before any device mapping) scans bytes **1..8** for `0x04` (Device-nGnRE,
preferred) else `0x00` (nGnRnE). That is sufficient because an *unprogrammed* MAIR byte reads
`0x00`, which is itself a valid MMIO encoding, so "already Device" and "unused" coincide. Only those
two qualify: nGRE would gather two `DR` stores into one lost character, and GRE would let the `DR`
store pass the `FR` poll. On QEMU today it picks index 1; `[mair] device attr_idx=… byte=…` is
**forensic, not a boot marker**, because it is firmware-dependent.

`Prot` grew a third field, `device` (plus `Prot::DEVICE_RW`), which is a compile error at every
struct literal — the point. `do_vmctl`'s `pt_map` answers `device: false` **permanently**, because
D1 rejected a VM-mediated `VMCTL_MAP_PHYS` for Phase 5; revisit for Phase-6 virtio-mmio, gated on a
per-driver PA whitelist. `pte_prot` decodes `device` **statelessly** (`pte_attr_idx_of(pte) !=
ATTR_IDX_NORMAL`), which is sound because the kernel emits only two indices and byte 0 is pinned
Normal-WB (`const _: () = assert!(ATTR_IDX_NORMAL == 0)`).

The **total invariant** is `map_page_in`'s pair of asserts against the extracted `mm::is_usable_pa`:
`prot.device ⇒ ¬RAM` and `¬prot.device ⇒ RAM`. Every leaf is therefore provably `(RAM ∧ ¬device)` or
`(device ∧ ¬RAM)` — the lemma that makes `if !prot.device { free_frame(…) }` sound in **five** leaf
sweeps (`do_exit::teardown_addrspace`, `do_fork::copy_addrspace`'s copy loop *and* its OOM unwind,
`do_vmctl::pt_unmap`, `userland::destroy_addrspace_with_leaves`). `free_frame` keeps its loud
out-of-range assert deliberately: a silent skip would demote a forged-PA or double-free into an
untraceable leak. Fork **re-maps** a device leaf rather than copying it (MMIO is shared, and a
`memcpy` through the cacheable HHDM alias would read side-effecting registers into RAM), and
`mm::uaccess::resolve_copyable` rejects a device leaf as copy source *or* destination (`EFAULT`).

`teardown_addrspace` became `pub(crate)` (and `system::do_exit` a `pub(crate) mod`) returning
`(freed, devs)` — hence `devs=` on `[ksys SYS_EXIT]` — for `userland::device_teardown_selftest`,
which is **load-bearing, not decoration**: TTY never exits, so the device arm would otherwise be
dead code on a live landmine whose failure mode is a *kernel panic*. It runs unconditionally at
boot, so `--no-default-features` covers it, and asserts **both** counts: `[devmap] selftest ok
freed=0 devs=1` (a guard skipping every leaf gives `devs=0`).

The device VA map is a new module, `kernel-shared/src/uspace.rs` — an *address* ABI, not a message
one: `USER_DEVICE_WINDOW_BASE = 0x4000_0000` (one whole L1 slot), `USER_DEVICE_WINDOW_SIZE = 16
MiB`, `TTY_UART_VA` = page 0; **not** emitted in the generated C headers. A `const _` on the region
*bases* is *not enough* — it proves only where each starts, and two of them grow on request (the
mmap bump cursor, and the heap's end via `set_brk`) — so both carry a runtime `region::REGION_LIMIT`
check (`ENOMEM` past it).

The pre-map lives in `load_boot_server` gated on `nr == TTY_PROC_NR`, **deliberately not in
`load_exec_image`** (shared with `do_exec`, so every exec'd binary would inherit a device window —
and conversely a proc that exec'd would *lose* its window, which is why `do_exec` drops the device
count). **No TLB maintenance**, because the AS was built moments ago and never installed in TTBR0 (a
recycled ASID is clean — `teardown_addrspace` flushes before `free_asid`), and
`switch_ttbr0_with_asid` already issues `isb; tlbi aside1; dsb ish; isb` on TTY's first schedule.
That is the same reasoning `SERVER_STACK_VA` already relies on; do not diverge.

**The CDEV band** is `CDEV_RQ_BASE = 0xB00`, between PM `0x700` and VM `0xC00`. (VFS took `0x800` in
5.4 and BDEV `0xA00` in 5.7, leaving `0x900` reserved for the VFS↔FS band in 5.8. Slices 5.3–5.6
recorded the BDEV/FS pairing the other way round in four comments, corrected in 5.7; only the
ascending order is load-bearing, which `callnr_h.rs`'s `bands_are_in_ascending_numeric_order`
enforces.) `CDEV_WRITE {minor 0..4, grant_id 4..8, len 8..12, offset 16..24}` — **no payload
`granter`**, the driver takes it from the kernel-stamped `m_source`. Reply `m_type` **is the byte
count** (`>= 0`, `0` legal), and a request longer than `CDEV_MAX_IO = 256` is a **short write, not a
failure** — POSIX `write()`, and what lets a driver stage through a `main`-frame buffer with no
allocator; the client re-sends with `offset` advanced. `CDEV_READ` was absent until slice 5.11
defined it.

PL011 register offsets are **duplicated** in `kernel/src/arch/aarch64/uart.rs` and
`drivers/tty/src/pl011.rs` and cannot be shared: the kernel crate is bare-metal-only and pinned by
`forced-target`, so it can never be a user-space dep, and a register layout is a hardware fact, not
a shared ABI. `drivers/tty` deliberately does **not** depend on `minixrs-driver-rt` (a 4-line
placeholder at the time) — Phase 6 makes that move, plus the `kernel/build.rs` watch entry.

Helpers lifted into `server-rt` this slice: `payload.rs` (`rd_i32` / `wr_i32` / `rd_u64` / `wr_u64`
/ `buf_addr`, measured, replacing PM/DS/VM/SCHED copies), `kcall.rs` (`sys_safecopy` / `sys_copy`
out of PM, coverage-excluded), `diag_fmt` (PM's `diag_line` promoted), and `sef_retrieve_from_ds`.

**DS publish-before-retrieve is not deterministic** — it works only because `build.rs` packs TTY
before VFS — so a DS lookup must **fall back to `boot_endpoint(…)`** and emit a distinguishable diag
line, keeping the rest of the proof alive while the `cdev.ds ok` marker, and only it, disappears.

**TCG caveat.** The Device *attribute* is not observably load-bearing under QEMU: its PL011 works
through Normal-WB, which is what the kernel's own HHDM alias has always been, so substituting
`ATTR_IDX_NORMAL` changes no marker. The attribute is proved by construction and assertion, as are
the `FR.TXFF` poll (TCG's FIFO never fills) and LF→CRLF (`grep -aF` cannot express `\r`).

(The standing rules are in [kernel.md](../conventions/kernel.md#device-memory),
[abi.md](../conventions/abi.md#request-band-allocation) and
[servers-and-drivers.md](../conventions/servers-and-drivers.md#drivers).)

### Slice 5.4: VFS write path — fd 1/2 → CDEV(TTY) ✓ shipped (PR #45, merged 2026-07-26)

**Goal:** the POSIX write shape: user proc → VFS → TTY, single copy.

**Scope:** `VFS_RQ_BASE = 0x800`; `VFS_WRITE {fd, buf, len}` (SENDREC from the caller). VFS
pre-opens fd 0/1/2 → console (minor 0) in a static per-proc fd table (`NR_SERVED_PROCS` rows);
`write(1/2)` makes a **magic grant** naming the caller's buffer and forwards it over `CDEV_WRITE` to
TTY (the D4 single-copy data path, first magic-grant consumer); other fds `EBADF`, other ops
`ENOSYS` for now. `populate_user_priv` opens `ipc_to = {PM, VFS}`. init sends one `VFS_WRITE` banner
before its fork loop (raw `minixrs-ipc` message — init stays a plain Rust user program).

**Proof:** init's banner reaches serial through VFS→TTY; `[ipc]` head traces show init→VFS SENDREC +
VFS→TTY CDEV round-trip.

**As built** (differences from the sketch above, recorded so later slices do not rediscover them):

- **init reports through the path it tests, and that forced a fourth marker.** init holds the shared
  USER privilege — no kernel calls, so no `SYS_DIAGCTL` and no debug channel of any kind. `write()`
  is the only thing it can say anything with, which is a nice property (a regression takes the
  evidence with it) but means every assertion has to be phrased as console text. The banner and the
  `vfs.deny ok n=4` summary were in the plan; the **`vfs.long ok match=1`** line was not, and it
  turned out to be load-bearing. Without it *nothing* checked a successful write's return value —
  init ignores it — so "reply `OK` instead of the byte count", which the plan listed as a mutation
  expected to produce a `FAIL`, moved no marker at all. The tail marker `vfs-loop-end` does not
  cover it either: a `write()` that moves every byte and then reports `OK` prints the tail and is
  still broken, in exactly the way a caller looping on the result would hit forever. Two markers,
  two independent halves of one contract.
- **The short-write loop clamps rather than trusts the driver.** `off += n` would walk past the
  buffer if a driver ever reported more than it was asked for, and over-report the write;
  `off.saturating_add(n).min(len)` cannot. TTY is trusted today, but the loop is the shape every
  future block/FS driver client copies. A driver reporting `0` breaks the loop rather than
  re-sending forever — a server that spins is worse than a short write.
- **An error after partial progress reports the progress**, not the error (POSIX). The bytes really
  did go out, and telling the caller otherwise makes it send them twice. The error resurfaces on the
  next write, where it is still true.
- **`user_range_ok` is defence in depth, and the mutation table says so.** Removing it changes no
  marker: the kernel's copy engine walks the caller's page tables and answers `EFAULT` regardless
  (D5), which is the load-bearing gate. It stays because rejecting a malformed buffer before issuing
  a grant is cheaper and more legible than discovering it two hops away — but nothing should mistake
  it for the check that makes the path safe.
- **The pure logic is a sibling module, `write.rs`** (the `drivers/tty/src/cdev.rs` split), holding
  `parse` / `validate` / `advance` with the SENDREC left in `main.rs`. Two of the short-write loop's
  four rules are **unreachable through a working TTY** — a driver reporting `0`, and one reporting
  more than it was asked for — so in the loop they could not be exercised without breaking the
  driver; as a step function they are three lines of test each. It also gets the `len < 0`
  (`EINVAL`) and `len == 0` (`Ok(0)`, checked *before* the buffer so an empty write never issues a
  grant) arms under host tests, and keeps them measured — `main.rs` is Sonar-coverage-excluded, a
  sibling module is not.
- **The fd table is an immutable `static`, so VFS still carries zero `unsafe`.** Nothing opens or
  closes in 5.4, so the table is fully determined at compile time and needs no interior mutability.
  Slice 5.8's `open` flips storage to the `UnsafeCell<[FdRow; N]>` + `unsafe impl Sync` newtype
  `vm/region.rs` and `ds/registry.rs` already use; `resolve_in` takes the rows as a borrowed slice
  precisely so it survives that switch untouched. Sized from `NR_SERVED_PROCS`, the shared ceiling,
  with the same `const _` guard PM/VM/SCHED carry.
- **`NR_FDS = 4`, not 3.** The fourth slot exists so "past the end of the row" and "in range but not
  open" are distinguishable cases in a test — and the `bad-fd` boot probe uses descriptor 3 for
  exactly that reason, since a descriptor that is merely out of bounds would be rejected by
  arithmetic rather than by the table.
- **`resolve` returns `Fd`, not a minor**, and `do_write` matches on it. The `Ok(Fd::Unused)` arm is
  unreachable today (`resolve` maps a closed descriptor to `EBADF` itself) and is kept so 5.8's
  regular-file variant lands as a compile error to be routed rather than a silent fallthrough.
- **VFS resolves TTY once, in `main`.** `tty_endpoint` moved out of `tty_demo`; a DS round-trip per
  `write()` would be a lookup per write for an endpoint that cannot change. The 5.3 demos are kept
  in full — they are the regression battery for the three contracts the real path does not reach
  (the *direct* grant form, the *visible* short write, and the two `CDEV_WRITE` refusals a
  well-formed `write()` never provokes).
- **The C header emits `VFS_WRITE` but not its payload offsets.** Same deferral the CDEV band uses,
  for a nearer reason: slice 5.6's `write()` wrapper is precisely the C that will need
  `VFS_FD_OFF`/`VFS_LEN_OFF`/`VFS_BUF_OFF`, so the test asserting their absence is what makes
  emitting them a deliberate act in that slice rather than something that drifted in early and froze
  un-reviewed at the ABI freeze.
- **The USER `ipc_to` widening is a list, not a second lookup.** `populate_user_priv` now loops over
  `USER_IPC_TO = [PM, VFS]`, resolving every server's priv slot in one read-only pass before taking
  any `&mut Priv` — the borrow discipline the existing `// SAFETY:` comments already justified, now
  with two servers instead of one. Every entry costs a *pair* of bits: `init_boot_image` fills a
  boot server's bitmap only over `[0, n_active)`, and `USER_PRIV_ID` is 20, so each reverse reply
  edge must be opened explicitly. The mutation table below shows the two directions failing in
  visibly different ways, which is worth knowing when adding a third server (5.8's MFS will not need
  one — a user process talks to VFS, not to MFS).

- **Four denial probes, not three.** A negative length (`EINVAL`) joined the boot battery on review:
  unchecked it widens into a ~16 EiB `u64` on the grant VFS is about to issue over the caller's
  buffer, which is worth an end-to-end marker and not only a unit test. A **zero**-length write is
  deliberately not in that battery — it is a legal no-op rather than a denial, and folding it in
  would have meant a positive assertion wearing a `deny` label; `write::validate` covers it, and the
  check order that stops it issuing a grant, in host tests.
- **`UNMAPPED_VA` carries compile-time guards**, not just prose. The `EFAULT` probe only proves what
  it claims if the address is well-formed enough to *reach* the page-table walk: one that VFS's own
  `user_range_ok` rejected would answer `EFAULT` a hop earlier and silently test the wrong thing. So
  `const _` asserts non-NULL, below `USER_VA_TOP`, and clear of `USER_DEVICE_WINDOW_BASE`. The two
  layout facts it also rests on — init's link base and its stack VA — stay prose, because neither is
  a constant a user program can see (`SERVER_STACK_VA` is kernel-internal, the link base lives in
  `user.ld`).

**Honest gaps:**

- **The confused-deputy rule is proved by construction, not by probe.** There is no `who_from` field
  in `VFS_WRITE`, so no boot marker can demonstrate a client failing to spoof one — the same shape
  as 5.2's unprobeable magic-`SYS_PROC` gate and 5.3's absent granter field. The mutation below (add
  such a field) is the only evidence, and it is evidence about a build that does not exist.
- **Only one process ever writes.** init is the sole `VFS_WRITE` client, so the fd table's
  per-process rows are exercised at exactly one index at runtime; the host tests sweep the whole
  `NR_SERVED_PROCS` range instead. Nothing yet writes from a forked child, which is what would prove
  the inherited-descriptor story end to end.

**Mutation tests run** (each applied, observed, reverted):

| Mutation                                                          | Observed                                                                                                                                                                                                                                                  |
| ----------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Read the grant's owner from a payload field instead of `m_source` | all four init markers vanish — the grant names the wrong owner, so TTY's safecopy is refused and every write fails. TTY itself stays online, so the failure is localized to the new path                                                                  |
| Drop VFS's short-write loop (single `CDEV_WRITE`)                 | `vfs-loop-end` **and** `vfs.long ok match=1` vanish (`vfs.long FAIL` printed); banner and `vfs.deny ok n=4` survive — the two markers really are independent halves                                                                                       |
| Make `resolve_in` accept any descriptor                           | `vfs.deny ok n=4` → `vfs.deny FAIL bad-fd` (the write to descriptor 3 succeeded)                                                                                                                                                                          |
| Drop the `ipc_to += VFS` bit in `populate_user_priv`              | all four init markers vanish — init's SENDREC is refused before VFS ever sees it                                                                                                                                                                          |
| Drop the reverse VFS → USER `ipc_to` bit                          | the banner **still prints** (the request was delivered and served; only the *reply* is refused), then init hangs in that SENDREC forever — so the other three init markers and all three `SYS_FORK`/`SYS_EXEC`/`SYS_EXIT` cycle markers vanish. 6 missing |
| Reply `OK` instead of the byte count from `do_write`              | `vfs.long FAIL`, and `vfs.deny FAIL bad-buf` — the `EFAULT` probe reads as success. `bad-fd`/`no-such` still pass, since both return before the mutated line                                                                                              |
| Drop the `user_range_ok` pre-check                                | **nothing moves** — all 51 markers still pass. Confirms it is defence in depth and the kernel's page-table walk is the load-bearing `EFAULT` gate                                                                                                         |
| Drop `write::validate`'s negative-length check                    | `vfs.deny ok n=4` → `vfs.deny FAIL bad-len` (the write with `len = -1` was accepted)                                                                                                                                                                      |

#### Design record

`VFS_RQ_BASE = 0x800`, between PM `0x700` and CDEV `0xB00`, with one request: `VFS_WRITE {fd 0..4,
len 4..8, buf 8..16}`. `buf` is a **raw buffer address, not a grant id**, because VFS's client is an
ordinary user process with no grant table. Reply `m_type` **is the byte count** (`>= 0`) — the
`CDEV_WRITE` contract, and what musl's `write()` returns.

VFS is the granter. `do_write` issues a **`CPF_MAGIC` grant naming the caller's buffer with the
driver as grantee**, so the bytes move in **one** copy, caller → driver, and VFS never touches them.
This is the first magic-grant consumer on a real data path. **The grant's owner is the
kernel-stamped `m_source`, never a payload field** — VFS holds `SYS_PROC`, so a caller-supplied
owner would let any client aim a privileged cross-AS copy at a third party. That is the 5.2/5.3
confused-deputy rule applied to the *granting* side, and there must never be a field for it.

**VFS absorbs short writes**, because `CDEV_MAX_IO` is a driver staging detail that must not reach
`write()`'s return value. `write_all` re-sends with `offset` advanced over the *same* grant until
the buffer is out, clamps `off` with `.min(len)` so an over-reporting driver cannot walk past the
buffer, breaks on `n == 0` rather than spinning, and on an error **after partial progress reports
the progress** (POSIX: those bytes really went out).

`servers/vfs/src/fd.rs` is the fd table: `NR_FDS = 4` — VFS-local, *not* ABI; the 4th slot is what
makes "past the end" and "not open" distinguishable — with rows sized from `com::NR_SERVED_PROCS`
under the usual `const _` guard, and fds 0/1/2 pre-opened to `CDEV_MINOR_CONSOLE` in every row.
Storage is an **immutable `static` until slice 5.8's `open`**: nothing mutates, so VFS keeps zero
`unsafe`. `resolve_in` takes the rows as a borrowed slice precisely so it survives the later switch
to the `UnsafeCell<[FdRow; N]>` newtype (the `vm/region.rs` shape) untouched.

`user_range_ok` in `do_write` is **defence in depth, not the gate** — removing it moves no marker,
because the kernel's page-table walk answers `EFAULT` regardless (D5).

`populate_user_priv`'s `USER_IPC_TO = [PM, VFS]` widens the shared USER slot to `ipc_to = {PM,
VFS}`. Every entry costs a **pair** of bits, since `init_boot_image` fills a boot server's bitmap
only over `[0, n_active)` and `USER_PRIV_ID` is 20, so each **reverse reply edge must be opened
explicitly**. The two failure modes differ usefully: drop the forward bit and init's SENDREC is
refused and every marker vanishes; drop the reverse and the write still *happens* and only the reply
is refused, so init hangs mid-SENDREC and takes the fork/exec/exit cycle markers with it.

init is the client and reports **through the path under test** — it has no `SYS_DIAGCTL`, being user
grade — which is why a successful write's count needs its own marker: **`vfs.long ok match=1`**
alongside the `vfs-loop-end` tail, because a `write()` that moves every byte and replies `OK` prints
the tail and is still broken.

Pure logic lives in `servers/vfs/src/write.rs` (`parse` / `validate` / `advance`), the
`drivers/tty/src/cdev.rs` split. That is not tidiness: **a retry loop's rules for a misbehaving peer
are unreachable while the peer works** — a driver reporting `0`, or more than it was asked for — so
lifting the loop body into a step function is the only way to test them. It also keeps them
Sonar-measured, since `main.rs` is coverage-excluded and a sibling module is not.

The generated C header emits `VFS_WRITE` but **not** its payload offsets — deferred to slice 5.6's
`write()` wrapper, which is exactly the C that will need them.

(The standing rules are in [servers-and-drivers.md](../conventions/servers-and-drivers.md#vfs) and
[servers-and-drivers.md](../conventions/servers-and-drivers.md#short-transfers-who-may-who-may-not).)

### Slice 5.5: exec ABI — SysV initial stack + minimal auxv ✓ shipped (PR #46, merged 2026-07-26)

**Goal:** D13's stack contract — musl's crt runs unpatched.

**Scope:** `do_exec`/`load_exec_image` build the Linux-SysV frame on the stack page (argc=1,
argv[0]=exec name, empty envp, auxv: `AT_PAGESZ`, `AT_PHDR`/`AT_PHNUM`/`AT_PHENT` when the first
PT_LOAD covers the ELF header, `AT_NULL`), writing through the new AS via HHDM before release; `sp`
points at `argc`. Boot-server loads keep the bare-`sp` path (servers' `_start` doesn't read the
stack). Worker is unaffected by content (it reads nothing) but proves the frame doesn't break a raw
`_start`.

**Proof:** worker cycle unchanged (fork/exec/exit markers green); a new `[exec]` trace line reports
`sp`/argc/auxv count and joins the expected file.

**As built** (differences from the sketch above, recorded so later slices do not rediscover them —
note the Scope paragraph's "worker … reads nothing" is superseded: worker validates the frame, which
is where most of this slice's proof lives):

- **`AT_PHDR` ships live, not as a dead conditional.** "When the first PT_LOAD covers the ELF
  header" was, as written, *never*: lld's default aarch64 `max-page-size` (64 KiB) puts the first
  `PT_LOAD` at file offset `0x10000`, while `e_phoff = 64` sits in the unmapped `[0, 0x10000)`
  prefix of every binary this repo builds. So `userland/worker/user.ld` took the `FILEHDR PHDRS`
  idiom (`text PT_LOAD FILEHDR PHDRS FLAGS(5)` plus `. = 0x00100000 + SIZEOF_HEADERS`), which lld
  accepts and which yields `PT_LOAD` #0 at offset `0x0` / vaddr `0x100000` — both still
  page-aligned, so the loader's strict `p_offset`/`p_vaddr` checks are untouched. The entry moves to
  `0x101000`. Server `user.ld`s were **not** changed: boot-loaded images keep the bare-`sp` path and
  never see an auxv.
- **The frame is proved at EL0, not by the kernel trace.** `[exec]` only says the kernel *thinks* it
  wrote a frame, so `worker` reads its own `sp` and checks it byte for byte. That is what its
  `_start` is `#[unsafe(naked)]` for — `mov x0, sp` is the first instruction the process executes,
  where an ordinary prologue could perturb `sp` first and taking the value in `x0` from the kernel
  would prove nothing about `SP_EL0`. The `#[cfg(all(not(test), target_arch = "aarch64"))]` split
  with a plain fallback is copied from `minixrs-ipc`'s SVC asm, so CI's x86_64 clippy job still
  checks the crate.
- **The verdict rides out as the exit status.** `worker` runs once per init fork cycle, so an
  unconditional success line would flood the console; it exits with `0` or the number of the first
  failing check, and init — which reaps it anyway — prints that one status. A failure additionally
  writes `minix.rs worker: stack FAIL` to fd 2 directly, belt and braces for the case where the
  status path is itself what regressed. Both spellings are on the forbidden list.
- **"The first reap" is not the worker's reap, and mutation testing is the only reason that was
  caught.** init's report keyed on its first `PM_WAIT` reply, and the marker passed — but PM's
  `mproc::seed` parents the demo stubs to init, and stub D is SIGSEGV'd on purpose early in boot, so
  init's first `wait()` reaps *that* zombie, whose status is 0. The marker was printing `exec stack
  ok` regardless of the frame: the argc mutation produced 81 `worker: stack FAIL` lines and a
  cheerful `exec stack ok` beside them. The fix is to key on the **pid** of the first child init
  forks (`alloc_pid` never returns 0, so 0 is the "not yet" sentinel) and report when `wait()`
  replies that pid. Ordering in the log now confirms it: `exec stack ok` follows the first `[ksys
  SYS_EXIT] target=w`, where before it preceded it. Note this was invisible under
  `--no-default-features`, where there are no stubs and the first reap really is the worker's — a
  marker can be right in one build configuration and vacuous in the other.
- **Silence must not read as success — the second vacuous-marker hole.** The verdict was first
  encoded as "exit 0 = pass", which is wrong for the same reason the first-reap keying was: PM's
  `mproc::handle_kill_in` marks a signalled process a zombie **without touching `exit_status`**, so
  a worker that died before reporting is reaped with status 0. That is not hypothetical — `validate`
  dereferences pointers taken out of the frame, so a sufficiently malformed frame faults it, and
  VM's out-of-region arm raises SIGSEGV rather than the `!!! EL0 data abort` banner the forbidden
  list watches for. A pass is therefore the positive sentinel `execstack::EXEC_STACK_PROBE_PASS`
  (`0x5A`), and status 0 prints `exec stack FAIL no-verdict`, naming the case. General rule for
  probes reporting through an exit status: **encode the pass, not the absence of a failure.**
- **What is deliberately missing from the auxv is now documented against the fork.** `execstack.rs`
  records why `AT_RANDOM`, the uid/gid/`AT_SECURE` block, `AT_HWCAP`/`AT_SYSINFO`/`AT_EXECFN` are
  safe to omit today — musl decodes into `size_t aux[AUX_CNT] = { 0 }`, so an absent entry reads 0,
  `__init_ssp(NULL)` falls back to an address-derived canary, and an all-zero uid/gid/secure block
  takes `__init_libc`'s early return. Slice 5.6 should confirm the SSP path actually runs and decide
  whether an address-derived canary is acceptable; `AT_PAGESZ` is the one entry that is **not**
  optional (`libc.page_size = aux[AT_PAGESZ]` is unconditional), which is why it is always emitted.
- **Frame failures tear down the fresh image.** `build_initial_stack` returning `None` (→ `E2BIG`)
  or `copy_to_user_as` failing (→ `ENOMEM`) both run `do_exit::teardown_addrspace` on the image just
  built and return before the point of no return, so the target stays cleanly on its old image —
  extending the existing `load_exec_image`-failure invariant rather than weakening it. The copy's
  errno is relayed verbatim rather than flattened into `ENOMEM` (TTY's `SYS_SAFECOPY` rule): both
  are kernel bugs here, but `EFAULT` and `ENOMEM` point at different ones.
- **`load_into` now returns a `LoadedElf`** (`entry`, `phdr_va: Option<u64>`, `phnum`, `phentsize`)
  instead of a bare entry VA, with `phdr_va` computed in the existing segment loop from the first
  `PT_LOAD` whose *file* range covers the header table. `ExecImage` carries the three new fields;
  `load_boot_server` is otherwise untouched.
- **No `gen-c-headers` entry.** The `AT_*` values are the Linux/SysV ones and musl defines them
  itself — emitting them would be the opposite of D13's keep-the-diff-minimal principle.

**Mutation tests run** (each applied, observed, reverted):

| Mutation                                  | Observed                                                                                                                                                                                                                           |
| ----------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Write `argc = 0`                          | `exec stack ok` → `exec stack FAIL code=2` (worker's argc check), plus `worker: stack FAIL`. **On the first attempt this printed `ok` beside 81 `worker: stack FAIL` lines** — the stale-zombie defect above, found here and fixed |
| Drop the `AT_PAGESZ` pair                 | `[exec] … auxv=3` (so that marker vanishes) and `exec stack FAIL code=6`                                                                                                                                                           |
| Off-by-one on the `argv[0]` string VA     | `exec stack FAIL code=3` — the argv[0] check dereferences the stored pointer, so a pointer one byte off reads `"orker\0"`                                                                                                          |
| Subtract 8 from `sp` (break 16-alignment) | `exec stack FAIL code=1`. No EL0 alignment abort — worker's own check fires first, which is the point of putting alignment first                                                                                                   |
| Skip the `copy_to_user_as`                | `exec stack FAIL code=2`: worker reads the zeroed stack page, so `argc` is 0. Confirms the frame really arrives via the copy and is not somehow present already                                                                    |
| Revert `user.ld`'s `FILEHDR PHDRS`        | `[exec] … auxv=1` (the `auxv=4` marker vanishes) and `exec stack FAIL code=7` — the phdr check. Confirms the `AT_PHDR` arm is live rather than dead                                                                                |

#### Design record

`SYS_EXEC` no longer just points `sp_el0` at the stack page top — it builds the **Linux/SysV initial
frame** there first: `[argc][argv…][NULL][envp…][NULL][auxv pairs][AT_NULL]`, then the
NUL-terminated name, padded so `sp` is 16-aligned, with `argc = 1` and an empty envp. That is what
lets musl's crt, `__libc_start_main` and `__init_tls` run **unpatched**; the standing
keep-the-musl-diff-minimal principle only holds if the *kernel* supplies the frame.

The byte layout lives in `kernel-shared/src/execstack.rs` (`build_initial_stack`, pure, zero
`unsafe`, host-tested — the `user_va_ok` / `page_chunks` carve-out, since the kernel crate has no
`#[cfg(test)]`). `do_exec` stages it in a `[u8; INITIAL_STACK_MAX]` **kernel-stack** buffer and
installs it with 5.1's `copy_to_user_as`, which is address-space-independent by design, so there is
no new copy machinery and the TTBR0 need not be installed. Both frame failures (`None` → `E2BIG`,
copy error → `ENOMEM`) run `do_exit::teardown_addrspace` on the **fresh** image and return
**before** the point of no return, extending `load_exec_image`'s leave-the-target-on-its-old-image
invariant rather than weakening it.

`elf::load_into` now returns a `LoadedElf { entry, phdr_va: Option<u64>, phnum, phentsize }`, with
`phdr_va` computed in the existing segment loop from the first `PT_LOAD` whose *file* range covers
`[e_phoff, e_phoff + phnum*phentsize)`; `ExecImage` carries the three through. Auxv order is **fixed
by the caller** — `AT_PHDR`, `AT_PHNUM`, `AT_PHENT`, `AT_PAGESZ` — not Linux's incidental order, so
the trace and the tests are deterministic. The `AT_*` values are the Linux/SysV ones and are
deliberately **not** emitted by `gen-c-headers`; musl defines them itself.

**`AT_PHDR` needs a linker-script opt-in, and the plan's "when the first PT_LOAD covers the ELF
header" was otherwise *never* true.** lld's default aarch64 `max-page-size` (64 KiB) puts PT_LOAD #0
at file offset `0x10000`, leaving `e_phoff = 64` in an unmapped prefix, so a reported VA would fault
`__init_tls`. `userland/worker/user.ld` therefore takes the `FILEHDR PHDRS` idiom (`text PT_LOAD
FILEHDR PHDRS FLAGS(5)` plus `. = <base> + SIZEOF_HEADERS`), giving PT_LOAD #0 at offset `0x0` and
vaddr `0x100000` — both still page-aligned, so the loader's strict checks are untouched; only the
entry moves one page up. **Server `user.ld`s stay unchanged**: boot-loaded images keep `sp =
SERVER_STACK_VA + PAGE_SIZE` and never read an auxv.

**EL0 is where the frame is proved.** `[exec] argc=1 envc=0 auxv=4 sp=0x` only says the kernel
*thinks* it wrote one, so `worker` reads its real `sp` and validates the frame — alignment, argc,
the `argv[0]` bytes through the stored pointer, both NULLs, `AT_PAGESZ`, and a phdr that reads back
as `PT_LOAD` at the link base — returning `0` or the **1-based number of the first failing check**.
That is what its `_start` is `#[unsafe(naked)]` plus `naked_asm!("mov x0, sp", "b {main}")` for: a
prologue could perturb `sp`, and taking the value in `x0` from the kernel would prove nothing about
`SP_EL0`. The verdict rides out as the **exit status**, not a console line (worker runs once per
init fork cycle), with a direct `stack FAIL` write to fd 2 as backup for the case where the status
path is what broke.

**The trap worth remembering.** PM's `mproc::seed` parents the demo stubs to init, and stub D is
SIGSEGV'd on purpose early in boot, so init's *first* `wait()` reaps that zombie (status 0), not a
child it forked. A marker keyed on "the first reap" printed `exec stack ok` beside 81 `worker: stack
FAIL` lines under the argc mutation — vacuous, and *only* under default features, since
`--no-default-features` has no stubs and its first reap really is the forked child. Key any
reap-derived marker on the child's **pid** (`alloc_pid` never returns 0, so 0 is the "not yet"
sentinel), and treat "a marker can be right in one feature config and prove nothing in the other" as
the general lesson: mutation-test in the config that has the stubs.

**The same trap has a second mouth.** `mproc::handle_kill_in` zombifies a *signalled* process
without touching `exit_status`, so a probe that died before reporting is reaped with status 0 — and
`validate` dereferences pointers out of the frame, so a bad enough frame faults it into VM's SIGSEGV
arm, which prints no `!!! EL0 data abort` for the forbidden list to catch. Hence the pass is the
positive sentinel `execstack::EXEC_STACK_PROBE_PASS` (`0x5A`), and status 0 prints `FAIL
no-verdict`.

(The standing rules are in [kernel.md](../conventions/kernel.md#exec--the-initial-stack),
[testing-and-markers.md](../conventions/testing-and-markers.md#exit-status-probes) and
[rust-style.md](../conventions/rust-style.md#naked-_start-and-the-cross-architecture-clippy-fallback).)

### Slice 5.6: musl fork + `src/minixrs` port + boot-embedded hello — **milestone A; ABI freeze** ✓ shipped (PR #47, merged 2026-07-26)

**Goal:** a C program built against the musl fork runs on minix.rs (exec'd from the boot archive;
the FS comes later).

**Scope (fork repo, as PRs there, pinned here by submodule bump):** commit the existing
`linux-inventory.md`; gut `arch/aarch64/syscall_arch.h` (replace `svc 0` Linux ABI with calls into
the MINIX layer; drop `VDSO_*`); add `src/minix/`: the IPC trap asm (x0=endpoint, x1=primitive,
x2=&msg — matching `minixrs-ipc`), `_syscall.c`, and a dispatcher mapping the milestone syscall set
per D13 (`writev`/`write`, `exit`/`exit_group`, `set_tid_address`, `ioctl`→`-ENOTTY`, stubs).
**Scope (this repo):** submodule at `external/musl`; `tools/build-musl.sh` (D10: configure
`--disable-shared`, clang/llvm-ar, cached sysroot, `gen-c-headers` output installed);
`userland/hello` (`hello.c`); `kernel/build.rs` packs hello as an exec-only module **when the
submodule is initialized, else packs the worker ELF under the name `hello`** (name-level fallback:
init always execs `hello`, boots are green either way, no feature flags); init's exec target flips
`worker` → `hello`; CI (qemu-smoke + a musl-build step): submodule checkout, clang/llvm-ar install,
sysroot cache keyed on submodule SHA + toolchain; header `-fsyntax-only` check (5.0) wired in.
License note for musl (MIT) added to the repo's licensing docs.

**Proof (milestone A):** `printf("Hello from C on minix.rs!\n")` output reaches serial through
VFS→TTY from a musl-linked binary exec'd out of the MXBI archive; marker in `qemu-boot.expected`.

**As built** — deltas from the scope above:

- **The fork was recreated, not reused.** The old `KevinBarnard/musl-minix` (v1.2.5 + 103 commits, a
  stray `CLAUDE.md` on its working branch) is **archived**; the port lives in a fresh
  `minixrs/musl-minixrs` pushed from a clean clone of `git.musl-libc.org`, with `main` = pristine
  upstream mirror and `minixrs` (the default branch) = the port, based on tag **`v1.2.6`**
  (2026-03-20). The v1.2.5 `linux-inventory.md` was **not** carried over — it was re-derived against
  v1.2.6 into the fork's `MINIXRS.md`, which also carries the branch and delta contracts.
- **Split 5.6a (fork) / 5.6b (this repo)**, the 4.6a/4.6b precedent, per the cross-repo rule.
- **`minix` → `minixrs` throughout**, since 5.6 is the ABI freeze and this was the last cheap
  moment: `src/minixrs/` and `__minixrs_syscall` in the fork, and on this side the generated C
  surface too — `include/minixrs/*.h`, `_MINIXRS_OFFSETOF`, `MINIXRS_ABI_CHECK_POSIX_ERRNO`. Prose
  citing *MINIX 3's* own headers (`callnr_h.rs`'s deviation note, `com_h.rs`'s `<minix/const.h>`)
  was deliberately left alone. The `minix-ipc/` **directory** was renamed `minixrs-ipc/` to match
  its long-standing package name.
- **`PM_EXEC` gained a 16-byte name payload** (`PM_EXEC_NAME_OFF`), replacing PM's hardcoded
  `EXEC_TARGET`, and **init alternates `worker` / `hello`** (`EXEC_TARGETS`) rather than flipping
  outright — the written scope's "flips `worker` → `hello`" predates 5.5 making `worker` the
  exec-stack probe, and flipping would have retired `[exec] … auxv=4` and `init: exec stack ok`.
  Groundwork 5.9's path form needs anyway. Reading the name uses a new host-tested
  `server-rt::payload::rd_name`.
- **The fallback presence-checks the SYSROOT, not the submodule** (`target/musl-sysroot/.stamp`).
  Checking the submodule would let a fresh clone that ran `git submodule update` but not
  `tools/build-musl.sh` trigger a multi-minute libc build from inside a cargo build script.
- **`RANLIB` is `llvm-ar s`, not `llvm-ranlib`** — the pinned toolchain ships `llvm-ar` but not
  `llvm-ranlib`.
- **Quad-float builtins had to be sourced.** musl's `vfprintf` references `__multf3` / `__floatsitf`
  / … (aarch64 `long double` is IEEE quad, no hardware support) and musl's configure finds no
  runtime library on this host. They come from the toolchain's own `compiler_builtins` **as built by
  `-Zbuild-std` for `aarch64-unknown-minixrs`**; the prebuilt `aarch64-unknown-none` rlib in the
  rustup sysroot does *not* export the C-ABI names. No compiler-rt/LLVM build was added. *P3c
  counterpart:* in the SDK flavor the problem does not arise — the patched driver links
  `libclang_rt.builtins.a` out of its own resource dir, under
  `$MINIXRS_SDK/lib/clang/<ver>/lib/aarch64-unknown-minixrs/`. Nothing in this repo may hard-code
  that `<ver>`, which is why `usable_sdk` probes neither the archive nor the resource dir: one
  driver invocation never names the version component, so the driver derives it.
- **`hello.ld` places what a musl link brings and a Rust one does not** — `.init`/`.fini`,
  `.init_array`/`.fini_array` with bracketing symbols, `.got`, `.data.rel.ro` — since an orphan past
  the last `PT_LOAD` is a silent load failure. No `PT_TLS`: musl keeps `errno` in the pthread struct
  via `tpidr_el0`, so `libc.a` carries no `.tdata`/`.tbss`.
- **The 4 KiB exec stack was sufficient** — §7's contingency (grow to 4 pages) was *not* needed.
  Measured worst chain is ~2 KB
  (`printf`→`vfprintf`→`printf_core`→`pad`→`__stdio_write`→`__minixrs_syscall`). Note for later:
  v1.2.6's `fmt_fp` allocates a **VLA**, ~512 B for `%f` but ~7.4 KB for `%Lf` — a future C program
  printing a `long double` *will* need a bigger stack, and a static frame scan will not predict it.
- **The real D7 errno check now runs**, as 5.0 promised it would here: `build-musl.sh` compiles the
  opt-in POSIX block against the fork's own `bits/errno.h`. Mutation-verified — swapping
  `EDEADLK`/`ENAMETOOLONG` fails the build with `libc EDEADLK disagrees with kernel-shared
  error.rs`. **It was vacuous when first written**: macOS `sed` has no `\b`, so `CHECK_MACRO` kept
  its old spelling while CI and the script defined the new one, and the guarded block compiled zero
  assertions.
- **The `SYS_EXEC` marker drops the slot number.** The written scope's `target=15 name=hello` holds
  only `--no-default-features`; with stubs, init's fork pool gives `hello` slot **16**. Keying on
  `name=hello entry=` keeps it true in both configs — the 5.5 "right in one feature config, vacuous
  in the other" lesson, applied before it bit.
- **The fallback made a *forbidden* marker fire, and only exercising it found that.** `worker`
  validates `argv[0]` against the exact bytes the kernel wrote, and it hardcoded `b"worker"` — so
  packing that same ELF under the name `hello` meant a missing sysroot printed `worker: stack FAIL`,
  i.e. an absent optional dependency was indistinguishable from a broken exec ABI. Now `ARGV0_NAMES`
  is the set `{worker, hello}`, which keeps the exact-byte check. General lesson: **a fallback path
  needs its own boot, not just a code review** — the name-level substitution was reviewed twice and
  read correctly both times.

#### Design record

The libc is a **git submodule at `external/musl`** pointing at `minixrs/musl-minixrs`, tracking
branch **`minixrs`** (the port; `main` is a pristine upstream mirror with nothing on it), based on
tag **`v1.2.6`**. The old `KevinBarnard/musl-minix` is archived and nothing was carried over.

The fork's whole delta is `arch/aarch64/syscall_arch.h` plus `src/minixrs/` plus a brand block in
`crt/crt1.c` plus `MINIXRS.md`, and staying that small is the design goal — **musl globs `src/*/`,
so `src/minixrs/` needs no Makefile edit**. Every `__syscallN` calls `__minixrs_syscall`, which
switches on the *Linux* number musl still emits and issues a server round-trip: `writev`/`write` →
`VFS_WRITE`, `exit`/`exit_group` → `PM_EXIT`, `set_tid_address` → 1, `ioctl` → `-ENOTTY`, everything
else `-ENOSYS`. musl's ~297 call sites stay untouched, which is what lets ~1900 files rebase for
free. `syscall_ret.c` is untouched because D7's magnitudes (`1..=40`, `>=200`) all sit inside its `>
-4096UL` window.

**`ioctl` → `-ENOTTY` is load-bearing**: it makes `__stdout_write` set `f->lbf = -1`, so stdout is
**fully buffered** and `printf` only reaches the console via `exit()`'s `__stdio_exit` — the
milestone marker therefore proves the flush path too.

Six syscalls suffice only because of startup facts that must be **re-verified, never assumed, on any
musl bump** (all re-read against v1.2.6 and recorded in the fork's `MINIXRS.md`):

- `__init_libc` early-returns past its `ppoll` when `AT_UID` / `AT_GID` / `AT_SECURE` are absent
  (they read 0).
- `__init_ssp(NULL)` derives the canary arithmetically.
- `__init_tp`'s only syscall is `set_tid_address` (`__set_thread_area` is `msr tpidr_el0` inline asm
  since v1.2.6), and it **walks `libc.auxv`**, so the auxv must stay `AT_NULL`-terminated.
- `__init_tls` uses static `builtin_tls` absent a `PT_TLS` (so no `mmap`) and walks 5.5's `AT_PHDR`
  / `AT_PHNUM` / `AT_PHENT`.

**`tools/build-musl.sh`** builds an out-of-tree sysroot into `target/musl-sysroot` — the submodule
work tree stays **pristine**, which the `c-headers` job asserts with a clean `git status` — keyed by
a `.stamp` of submodule SHA plus toolchain. `RANLIB` is **`llvm-ar s`**, because the pinned
toolchain ships `llvm-ar` but **not** `llvm-ranlib`. It also runs the **real D7 errno *value*
check** that 5.0 deferred here: the opt-in POSIX block compiled against the fork's own
`bits/errno.h` under `-nostdinc -isystem <sysroot>/include`. The CI `c-headers` job keeps only the
hermetic stand-in check.

`userland/hello` is **not a cargo crate** — `hello.c` plus `hello.ld`, compiled by clang and linked
by `rust-lld` inside `kernel/build.rs` (`build_hello` at the time, `build_hello_musl` since P3c
added the SDK flavor beside it; *in the musl flavor* — the SDK flavor uses neither `hello.ld` nor
`rust-lld`), packed under proc_nr `-1` like `worker`. **The fallback presence-checks the SYSROOT
(`.stamp`), not the submodule** (both sysroots, since P3c), and packs the `worker` ELF *under the
name* `hello` with a `cargo::warning` — checking the submodule would let a fresh clone trigger a
multi-minute libc build from inside a build script.

That substitution is why `worker`'s `ARGV0_NAMES` is the **set** `{worker, hello}` rather than one
literal: it checks the exact `argv[0]` bytes the kernel wrote, so the hardcoded `b"worker"` made a
*missing sysroot* print `worker: stack FAIL` — a **forbidden** marker — making an absent optional
dependency indistinguishable from a broken exec ABI. **Boot the fallback config when you change
it**; two readings of the name-level substitution missed this and only running it caught it.

musl's `vfprintf` needs soft-float **binary128** builtins (`__multf3`, `__floatsitf`, …; aarch64
`long double` is IEEE quad) which musl's configure finds no library for. They come from
**`compiler_builtins` as built by `-Zbuild-std` for `aarch64-unknown-minixrs`** — the *prebuilt*
`aarch64-unknown-none` rlib in the rustup sysroot does **not** export the C-ABI names, so glob the
nested target dir, never the sysroot.

`hello.ld` is `worker/user.ld` plus what only a musl link brings — `.init` / `.fini` (they define
the `_init` / `_fini` that `crt1.c` passes to `__libc_start_main`), `.init_array` / `.fini_array`
with bracketing symbols, `.got`, `.data.rel.ro` — because **an orphan section past the last
`PT_LOAD` is a silent load failure**. The `FILEHDR PHDRS` idiom is mandatory here, not just an auxv
nicety as it is for Rust: `__init_tls` dereferences `AT_PHDR`. There is no `PT_TLS`, because musl
keeps `errno` in the pthread struct via `tpidr_el0`, so `libc.a` has no `.tdata` / `.tbss`.

**The 4 KiB exec stack was measured sufficient (~2 KB worst chain) and was NOT grown** — but
v1.2.6's `fmt_fp` allocates a **VLA**, about **512 B for `%f`** and **~7.4 KB for `%Lf`**, so a
future C program printing a `long double` will overflow it and *no static frame scan will predict
that*.

`PM_EXEC` gained a name payload (`PM_EXEC_NAME_OFF`, reusing `EXEC_NAME_LEN`), PM's `EXEC_TARGET` is
gone, and **init alternates `worker` / `hello`** (`EXEC_TARGETS`, worker **first** — `probe_pid`
keys 5.5's verdict on the first child) so the exec-stack proof survives the C milestone; the target
is chosen *before* the fork so both halves of the SENDREC agree. The boot marker is **`name=hello
entry=` with no `target=` slot**: init's fork pool gives `hello` slot 15 stub-free but **16** with
stubs, so a slot-keyed marker passes in one config and is vacuous in the other.

**ABI freeze (D8).** Past this slice, `Message` layout, call numbers, endpoints and errnos change
only via a deliberate ABI-bump PR touching both repos — there is now C in another repository
depending on all four. Since the port branch is **force-pushed** on rebase, a fork rebase and the
`external/musl` bump must land in the **same PR**, or this repo pins an orphaned commit.

(The standing rules are in [abi.md](../conventions/abi.md#d8-the-abi-freeze) and
[build-and-boot.md](../conventions/build-and-boot.md).)

### Slice 5.7: BDEV band + `memory` ramdisk driver + `tools/mkfs-mfs` + rootfs blob ✓ shipped (PR #48, merged 2026-07-27)

**Goal:** D3 — a block-device story with a real MFS image behind it.

**Scope:** `fs/mfs` library half first: MinixFS v3 on-disk structs (superblock/inode/dirent, 4 KiB
blocks) + pure readers, host-tested. `tools/mkfs-mfs` host tool builds `rootfs.img` from a manifest
(`/bin/hello` from 5.6's build, `/etc/motd`); round-trip host tests against the mfs readers.
`kernel/build.rs` runs mkfs and packs the image as (`rootfs`, −1). Kernel boot: copy blob to RAM
frames, map RW into MEM's AS; new `SYS_GETINFO` selector returns `(va, len)`. `drivers/memory`
becomes a real server (roster +1, proc_nr 3): serves `BDEV_RQ_BASE = 0xA00` `BDEV_READ`/`BDEV_WRITE`
against the mapped image via safecopy.

**Proof:** the `[as]` roster grows to 13; `[ramdisk] mem va=… len=… pages=…` from the kernel's copy
loop; `[diag memory] ramdisk ok blocks=256 tail=1`; and a live VFS client exercising
`bdev.{ds,read,head,tail,deny}`.

#### As built — five differences from the sketch above

1. **No `granter` field in the payload, and no grant offset.** The sketch's `{minor, block, count,
   granter, grant_id}` would have made every grant-holding block driver a confused deputy. The
   driver takes the granter from the kernel-stamped `m_source`, the 5.2/5.3 rule; the offset is
   absent because every client through 5.9 grants a buffer whose block starts at 0. The payload is
   `{minor, grant, len, block}`, mirroring `CDEV_WRITE`.
2. **A fixed-size image (`ROOTFS_IMAGE_BLOCKS = 256`, 1 MiB), not a content-sized one.** In the
   musl-sysroot-absent fallback `build_hello` returns the 15 KB *worker* ELF, so a content-sized
   image would make every size-derived marker config-dependent — the 5.5/5.6 "right in one config,
   vacuous in the other" trap. Oversizing is a build failure (`MkfsError::TooBig`) naming one
   constant.
3. **`/etc/pattern` (40 KiB) is mandatory, not filler.** `/bin/hello` is ~200 KB = 49 blocks and
   forces the single-indirect zone arm — but in the fallback config it is 15 KB = 4 blocks, *inside*
   the 7 direct zones, so both the arm and mkfs's indirect writer would be dead in exactly the
   config CI's non-QEMU jobs build. A constant-size file past the boundary keeps them live in both.
4. **MEM's init check is device-level, not format-level.** A superblock-magic check would make a
   *block* driver depend on the *filesystem format* crate, which Phase 6 then has to unwind when
   virtio-blk replaces MEM under an unchanged MFS. Instead mkfs writes a 32-byte image header into
   block 0's unused boot block (bytes 0..1024, which MFS never reads) and a 32-byte tail label into
   a reserved last zone; MEM verifies those, and format-level facts are verified by a client
   *through a real BDEV round trip* — strictly stronger, since MEM reading its own mapping proves
   only that the copy loop ran. The header/superblock agreement is a mkfs round-trip test.
5. **The image-header ABI lives in `kernel-shared::rootfs`, not in `tools/mkfs-mfs`.** mkfs writes
   those bytes, MEM and VFS read them, and none of the three may depend on the others — the same
   reason `com::ROOTFS_MODULE_NAME` is shared. `image.rs` re-exports them so the writer's own
   constants still read locally.

Also: `BDEV_WRITE` is defined now and answers `EROFS` rather than `ENOSYS` (which is already the
unknown-`m_type` answer, so reusing it would make "doesn't know about writes" and "knows and
refuses" indistinguishable), and an over-long or out-of-range request is `EINVAL` rather than a
short read — a deliberate departure from CDEV, because a filesystem cannot interpret half a block.

#### Design record

A MinixFS v3 image is built **at compile time** by `tools/mkfs-mfs`, a `[lib] + [[bin]]` host crate
whose **library `kernel/build.rs` calls directly as a `[build-dependencies]` path dep** — the
`brand::scan_brand` precedent, no nested host cargo. Cargo re-runs a build script when a
build-*dependency* changes, so neither it nor `fs/mfs` needs a `rerun-if-changed` entry. The image
is packed into the MXBI archive as a **non-ELF blob** under `com::ROOTFS_MODULE_NAME = "rootfs"` /
`EXEC_ONLY_PROC_NR`, copied into RAM frames at boot, and mapped `Prot::RW_DATA` into the `memory`
driver's address space.

**`BDEV_RQ_BASE = 0xA00`** (D13); `0x900` stays reserved for 5.8's FS band. Four in-tree comments
said the reverse and were corrected here, since only *ascending order* is load-bearing. The payload
mirrors CDEV — `{minor, grant, len, block}` — with **no granter field** (`m_source`, the
confused-deputy rule) and **no grant-offset field** (every client through 5.9 grants a buffer whose
block starts at 0). Reply `m_type` **is the byte count**.

Two deliberate departures from CDEV, each with its own doc comment:

- **An over-long or out-of-range request is `EINVAL`, not a short read.** A short *write* is a POSIX
  contract clients loop over; a short *block read* is useless, because a filesystem cannot interpret
  half a block. `EIO` stays reserved for Phase 6's real media errors, where the request was
  well-formed and the *device* failed.
- **`BDEV_WRITE` was defined by this slice and answered `EROFS`** — it stores for real as of 5.10a —
  rather than `ENOSYS`, because `ENOSYS` is already the unknown-`m_type` answer and reusing it would
  make "doesn't know about writes" and "knows and refuses" indistinguishable. That is what let 5.10a
  change one line inside one arm.

**Kernel side.** `uspace::RAMDISK_WINDOW_BASE = 0x8000_0000` (one L1 slot, 4 MiB, `RAMDISK_VA` =
page 0) is placed **above** the device window on purpose: `region::REGION_LIMIT` is the base of the
*lowest* kernel-owned window, so VM needs **no edit** when a window is added, and
`assert!(USER_DEVICE_WINDOW_BASE + USER_DEVICE_WINDOW_SIZE <= RAMDISK_WINDOW_BASE)` is what keeps
that true. It also makes the ramdisk's separation from every *process* VA transitive, which is why
`userland.rs`'s VA-collision block needs no new entries — say so, or someone adds six redundant
asserts.

The pre-map is a `nr == MEM_PROC_NR` arm in `load_boot_server` beside TTY's, **not** in
`load_exec_image` (shared with `do_exec`). No TLB maintenance (TTY's two reasons); no cache
maintenance (the kernel writes and MEM reads the same PA through Normal-WB mappings, and it is data,
so there is no icache concern); and **not** the TTY device-page problem — RAM mapped non-device
takes the ordinary `free_frame` path in all five leaf sweeps. Every step `.expect()`s: a `let _ =
map_page_in(..)` would turn a non-advancing loop into a 1-page ramdisk with 255 leaked frames
instead of an `AlreadyMapped` panic.

New selector **`GET_RAMDISK = 64`** returns `(va, len)` and is **gated on `caller.nr == MEM_PROC_NR`
→ `EPERM`**, because the VA is meaningless in any other address space. No new kernel call and no new
kernel statics: the VA is a const and the length is `module_by_name("rootfs")`'s. `pack_mxbi` gained
`u32` asserts on offset and length (unchecked casts, the latent class `checked_add` exists to kill).

**The driver contains no `unsafe` block and never dereferences the mapping**: client transfers are
`sys_safecopy`, and the boot self-check is `sys_copy(SELF, …, SELF, …)`, so a page that failed to
map is an `EFAULT` *return value* rather than an EL0 abort — hence no MMIO sibling module and no new
Sonar exclusion. `server-rt::sys_getinfo` is new, with `sef_startup`'s inline `GET_WHOAMI`
refactored onto it so every server's `sef ready` marker proves the refactor.

**Three scope amendments forced by repo lessons, all load-bearing:**

- The image is a **fixed `ROOTFS_IMAGE_BLOCKS = 256` (1 MiB)**, because the musl-sysroot-absent
  fallback packs the 15 KB `worker` ELF as `hello`, and a content-sized image would make every
  size-derived marker config-dependent — the 5.5/5.6 trap. Oversizing is a build failure naming one
  constant.
- **`/etc/pattern`, 40 KiB, is mandatory**, because `hello` is ~200 KB = 49 blocks and forces the
  single-indirect arm, but the *fallback* `hello` is 4 blocks — inside the 7 direct zones — so both
  that arm and mkfs's indirect writer would be dead in exactly the config CI's non-QEMU jobs build.
- **MEM's init check is device-level, not format-level.** A block driver must not depend on the
  filesystem format (Phase 6 unwinds that when virtio-blk replaces MEM under an unchanged MFS), so
  mkfs writes a 32-byte **image header** into block 0's unused boot block (bytes `0..1024`, which
  MFS never reads) plus a 32-byte **tail label** in a reserved last zone; MEM verifies those, and
  format-level facts are verified by a *client through a real BDEV round trip*.

That header ABI lives in **`kernel-shared::rootfs`** — mkfs writes it, MEM and VFS read it, and none
may depend on the others, the `ROOTFS_MODULE_NAME` reason; `image.rs` re-exports it. The **tail
label is not decoration**: a copy loop that failed to advance maps 256 pages of block 0 and passes
every header check. Mutation-tested, and the only marker that moved was `ramdisk FAIL tail label`.

`fs/mfs` gains the pure read path (`superblock` / `inode` / `dirent` / `layout` / `read`, `no_std`,
`forbid(unsafe_code)`, **one dependency**) and is **I/O-free by construction**: every reader takes
bytes the caller already fetched, which is both what makes it host-testable with no fake device and
the shape 5.8's server needs. `zone_for_offset` distinguishes a `Hole` (reads as zeroes) from
`OutOfRange` (past the single-indirect span — explicit, never silent zeroes). Double-indirect is
justified out arithmetically: `7 + 1024` zones is 4.03 MiB, which is also why the VA window is 4
MiB.

VFS's demo runs **last** in `main`'s prologue — a hang then localizes to `bdev.*` instead of
blacking out 5.2/5.3/5.4's markers, so do not tidy the prologue into alphabetical order — and it
exists because **there is no successful `SAFECOPY_TO` anywhere else in the tree**: `BDEV_READ` is
the first copy that writes into a grantee's buffer and succeeds, so without it the kernel's
`CPF_WRITE` plus `Prot::writable` success path stays unproven until 5.8. Its granted buffer is a
32-byte **local**, not a `.rodata` static (the driver writes into it) and not a whole block — do not
grow VFS's one-page stack for a demo that dies in 5.8, and note that a 4096-byte local would fault
into VM's SIGSEGV arm, which prints no `!!! EL0 data abort` for the forbidden list to catch.

`module_by_name` is now a namespace shared by ELFs and blobs: asking `do_exec` for `"rootfs"` fails
safely (`scan_brand` rejects), but **5.9's path→module resolution must not be able to name it**.

(The standing rules are in [kernel.md](../conventions/kernel.md#the-ramdisk-window),
[abi.md](../conventions/abi.md#request-band-allocation) and
[servers-and-drivers.md](../conventions/servers-and-drivers.md#block-drivers).)

### P3c: the SDK toolchain flavor — `kernel/build.rs` consumes `$MINIXRS_SDK` (toolchain milestone M3a)

Out-of-band work, not a Phase 5 slice: it lands in the tooling repo's M-ladder and only touches this
repo at `kernel/build.rs`. Recorded here because it sits between 5.7 and 5.8 chronologically and
because the measurements below close a loop 5.7 opened.

#### Design record

Slice 5.6 ran C on minix.rs through a *stand-in* triple — `--target=aarch64-unknown-linux-musl`, a
hand-written `hello.ld`, a globbed `compiler_builtins` rlib, a hand-assembled `rust-lld` line. M3's
claim is that all of it collapses into `clang --target=aarch64-unknown-minixrs hello.c -o hello`,
and it does: the patched driver supplies `-static`, `--image-base=0x100000` (LLVM patch 0006, which
is *why* this flavor needs no linker script), both `-z` flags (D13), the crt objects and `-lc` from
the sysroot, and `libclang_rt.builtins.a` for the quad-float helpers. **The port was never the
remaining work; the toolchain flavor was.**

Selection is **three-way** — `HelloFlavor::{Sdk, Musl, Worker}` — and `Musl` is **not a fallback but
`qemu-smoke`'s real dependency**, since no CI job installs an SDK and `tests/qemu-boot.expected`
requires the five C markers. Only `Worker` loses markers.

**Once an SDK is usable every failure is a `panic!`, never a demotion**, because the boot markers
are **byte-identical across flavors**: the log physically cannot distinguish them, so a silent
demotion would turn "your patched clang regressed" into "nothing looks wrong". The panic carries the
prefix, the stamp, a `clang -###` reproduce line, and the `MINIXRS_SDK=/nonexistent` escape hatch.

`usable_sdk` probes **exactly three files** — `bin/clang`, `sysroot/.stamp`,
`sysroot/usr/lib/libc.a` — and deliberately **not** the crt objects, the builtins archive, or the
`lib/clang/<ver>` resource dir: the driver names those, and **nothing here may hard-code the `22`**.
A sysroot missing them is a *broken* SDK, which must fail loudly.

Both stamps are watched with `rerun-if-changed` **unconditionally**. Watching the SDK's only when it
exists looks like a free optimization but breaks "run the sysroot builder, then `cargo
kernel-aarch64`" — a *declared-but-missing* path is exactly what makes cargo notice the stamp
**appearing**.

`clang_command` scrubs `CPATH`, `*_INCLUDE_PATH`, `LIBRARY_PATH` and `SDKROOT` from **every** clang
site, the `.S` loop included (a no-op there today). clang folds those into the **front** of the
include search list, ahead of the sysroot, and **`-nostdinc` does not suppress them** — verified, a
host `C_INCLUDE_PATH` really did sort first — so a foreign `errno.h` would shadow musl's, and D7
turns on those values. `/usr/local/include` survives the scrub because the *fork's driver* injects
it; it sorts after the sysroot, so it can only add headers musl lacks. Do not "fix" it with
`-nostdlibinc`, which would drop the sysroot too.

**No pack-time ELF/loader gate, and that is now measured rather than argued.** An image-base
regression is already loud: mutation-testing `-Wl,--image-base=0x200000` cost `name=hello entry=`
*and* all five C markers, because `load_exec_image`'s `SERVER_STACK_VA` map returns `AlreadyMapped`
and the exec unwinds. A gate would mean duplicating a private kernel const to re-detect what the
kernel already refuses. Loader rules stay tooling's `verify/check-image.sh`, by hand.

**Measured:** SDK **46,664 B / 4 `PT_LOAD`s** vs musl **200,152 B / 3**, both entry `0x101000`, both
a megabyte clear of `SERVER_STACK_VA` — and 46,664 B is **12 MFS blocks**, still past the 7 direct
zones, so slice 5.7's `/etc/pattern` mandate is unchanged.

**Never write inside `$MINIXRS_SDK`** (tooling's `build-musl.sh` does `rm -rf $SDK/sysroot`); all
output goes to `target/hello/`, which **both flavors share**, so read it only after a *successful*
build of the flavor you mean.

Two things nothing enforces, both manual: the SDK links **its own** musl (the stamp's `musl=<sha>`
is a merge commit of `external/musl`'s HEAD, so an equality check is impossible — rebase the fork
without re-running tooling's `build-sysroot.sh` and the two flavors silently test different libc
code), and the stamp's `minixrs=<sha>` is a **snapshot** of the installed `minixrs/*.h`, tolerable
only under D8's ABI freeze — so any `kernel-shared` ABI change requires re-running
`build-sysroot.sh`.

(The standing rules — the three-way flavor selection, and the fact that the SDK flavor has zero CI
coverage — are in [build-and-boot.md](../conventions/build-and-boot.md) and
[ci.md](../conventions/ci.md#the-sdk-flavor-has-zero-ci-coverage).)

### Slice 5.8: MFS server (read-only) + FS band + VFS mount/open/read ✓ shipped (PR #51, merged 2026-08-02)

**Goal:** files readable through the full VFS→MFS→BDEV→ramdisk stack.

**Shipped as two commits on one branch** (5.8a MFS half, 5.8b VFS half), boot green at each — the
a/b split this entry anticipated.

**Scope, as built.** `fs/mfs` server half (roster +1, proc_nr 6, packed between `memory` and `vfs`):
SEF loop serving `FS_RQ_BASE = 0x900`. The request set is **`FS_READSUPER` + `FS_LOOKUP` + `FS_READ`
only** — three departures from this entry's original list, each deliberate:

- **No `PUTNODE`.** MFS keeps no per-open state, so there is no node to put.
- **No STAT-lite / `VFS_STAT`.** `FS_LOOKUP`'s reply already carries mode and size, which is the
  whole of what a client needs before reading.
- **No GETDENTS.** Nothing consumes it — the `CDEV_READ` precedent that a request without a consumer
  is better absent than stubbed.

The path travels **inline** (NUL-padded, `FS_PATH_MAX = 64`), not by grant: it is control plane, it
costs MFS no staging buffer, and it deletes the confused-deputy question outright. Data travels by
grant, with no granter and no grant-offset field. An over-long `FS_READ` is **clamped** (a short
read), the deliberate departure back towards `CDEV_WRITE` and away from `BDEV_READ`.

`0x900` was the last reserved band slot, so `0x700..0xC00` is now fully allocated; the callnr test
that recorded it as free is replaced by one recording that it is not.

VFS: `VFS_OPEN`/`VFS_READ`/`VFS_CLOSE` (`NR_VFS_MSGS` 1→4), lazy root mount whose *failure is not
cached*, `write.rs` renamed `rw.rs` (read reuses write's payload and `advance`'s rules are
direction-agnostic) plus a new `open.rs`, and an fd table that is now interior-mutable with
`Fd::File { ino, pos }` and `NR_FDS` 4→8. VFS's slice-5.7 `bdev_demo`/`bdev_denials` **retire** —
MFS is the real BDEV client, so the battery relocates there and VFS goes back to knowing nothing
about block devices. Sonar `fs/**/src/main.rs` exclusion landed with the code; the miri `-p` entry
already existed.

Two hazards found during design, both real:

1. init's `("no-such", VFS_WRITE + 1, …)` probe would have become `VFS_OPEN` and silently retired
   the `vfs.deny ok n=4` marker. Fixed to the band-relative `VFS_RQ_BASE + NR_VFS_MSGS`.
2. `[[bin]] required-features = ["server"]` removes `fs/mfs/src/main.rs` from every CI job.
   Mitigated by a lib-heavy layout (`proto.rs`/`walk.rs` hold every decision), one extra clippy step
   in `ci.yml`, and a note in `Cargo.toml`.

**Proof.** MFS's own prologue is self-driving: `[as] mfs nr=6`, `[diag mfs] sef ready`, `bdev.ds ok
ep=`, `mount ok root=1 bs=4096 blocks=256`, `fs.selfcheck ok n=31 match=1`, `fs.indirect ok match=1`
(the only marker that reaches the single-indirect arm), `bdev.tail ok match=1`, `bdev.deny ok n=10`.
Then VFS: `fs.ds ok ep=`, `fs.mount ok root=1 bs=4096 blocks=256`, `fs.deny ok n=10`. Then the
milestone — **`minix.rs rootfs: motd from MFS`**, `/etc/motd` read by init and written to fd 1 —
plus `fs.read ok match=1` (bytes moved *and* `pos` advanced *and* EOF is `0`), `fs.fd ok match=1`
(open→3, open→4, close both, re-open→3), and `open.deny ok n=8`. Retired: `bdev.read ok n=32` and
`bdev.head ok match=1`, strictly subsumed by `mount ok`.

**Mutation results** (applied to an uncommitted tree, restored from the scratchpad, `grep -rn
MUTATION` clean afterwards). Nine mutations moved the marker they were aimed at:

| mutation                                        | marker that moved                                                                                                                                     |
| ----------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| drop the `mfs` row from `servers`               | every `[diag mfs]` line (0 remain); VFS then wedges in `mount_root`'s SENDREC, taking init's markers too                                              |
| decode the superblock at `&blk[0..]`            | `mount FAIL rc=-22`, cascading to `fs.mount FAIL` and `fs.read FAIL open` — and MFS *keeps answering*, which is the degraded-not-fatal design working |
| `zone_for_offset`'s `Indirect` arm → `Hole`     | `fs.indirect FAIL n=32`, with `fs.selfcheck` untouched — the two probes really are independent arms                                                   |
| delete `memory`'s over-long check               | `bdev.deny FAIL too-long rc=-1`                                                                                                                       |
| `CPF_READ` for `CPF_WRITE` in `do_read`'s grant | the **milestone line vanishes** (kernel refuses the copy) + `fs.read FAIL`                                                                            |
| delete `do_read`'s `fd::advance`                | `fs.read FAIL` — the second read repeats the first                                                                                                    |
| `.rev()` on `alloc_in`'s scan                   | `fs.fd FAIL` — the first open returns 7                                                                                                               |
| delete `open::classify`'s `EISDIR` arm          | `open.deny FAIL is-dir`                                                                                                                               |
| break MFS's `"memory"` DS key                   | `bdev.ds FAIL rc=-3 fallback=3`, everything downstream still green                                                                                    |

Two results worth keeping, because both contradict what the slice plan predicted:

- **Deleting MFS's `!node.is_dir() → ENOTDIR` guard does not move `fs.selfcheck`.** It cannot:
  resolving `/etc/motd` only ever walks through real directories, so the guard never fires on that
  path. What moves is `fs.deny FAIL not-dir rc=-2` — which is the probe that exists for exactly this
  check, and the better answer.
- **Swapping the `memory`/`mfs` rows does not move `bdev.ds`** — `bdev.ds ok ep=3` still resolves.
  This is the documented non-result (publish-before-retrieve is scheduler-dependent and usually
  still wins the race), so the DS *fallback* branch was exercised the recommended way instead: break
  the key, and separately drop the peer, which produced `fs.ds FAIL rc=-3 fallback=6` for VFS's
  lookup.

#### Design record

The **FS band is `FS_RQ_BASE = 0x900`**, which was the last reserved slot, so **`0x700..0xC00` is
now fully allocated** (PM / VFS / FS / BDEV / CDEV). A tenth band needs a home outside that span,
and `the_server_band_space_below_vm_is_fully_allocated` replaces the test that recorded `0x900` as
free.

The request set is **`FS_READSUPER` + `FS_LOOKUP` + `FS_READ` and nothing else**: no PUTNODE,
because MFS keeps **no per-open state** — there is no node to put, which is also why `VFS_CLOSE`
sends the FS nothing; no stat, because `FS_LOOKUP`'s reply carries mode and size; no GETDENTS — the
`CDEV_READ` precedent that a request without a consumer is better absent than stubbed.

**The path travels inline** (`FS_PATH_OFF`, NUL-**padded** to `FS_PATH_MAX = 64`, so the longest
path is 63 bytes and a field with no NUL is `ENAMETOOLONG` rather than a truncation that could
resolve to another file). It is control plane, not the data path D4 provisioned grants for — the
`PM_EXEC` name and `DS_PUBLISH` key precedent — it costs MFS no staging buffer, its scarcest
resource, and it deletes the confused-deputy question because there is no granter. Data travels by
grant with **no granter field and no grant-offset field**; VFS issues a fresh grant over exactly the
round's bytes.

**An over-long `FS_READ` is clamped — a short read, not `EINVAL`** — the deliberate departure back
towards `CDEV_WRITE`: BDEV refuses because *its* client is a filesystem that cannot interpret half a
block, but here the client is VFS, whose job is hiding staging from POSIX, and a read is short at
EOF regardless. **Two copies, not one** (device → MFS's block buffer → the caller's granted buffer),
unlike the 5.4 write path's advertised single copy: a MinixFS read is rarely block-aligned at both
ends, and a *hole* has no device block to copy from at all. MINIX 3's own shape.

**`fs/mfs` is split unusually hard because `[[bin]] required-features = ["server"]` removes
`main.rs` from every CI job** — clippy `--all-targets`, miri, llvm-cov, none of which passes the
feature. So every line with a decision in it is in the lib (`proto.rs` wire codec, `walk.rs`
traversal and read policy, the `Vec`-free rewrite of `mkfs-mfs`'s `verify.rs`), mitigated further by
one extra `cargo clippy -p minixrs-mfs --features server` step in `ci.yml` and a note in
`Cargo.toml`. `kernel/build.rs` threads `--features server` through the nested build the way it
threads PM's `--no-default-features`. `proto.rs` re-derives its own `rd_i32` / `rd_u64` because
`server-rt` is an *optional* dep — the trade `userland/init` already makes.

**The 4 KiB block buffer is a `.bss` static, not a `main`-frame local**: a server stack is exactly
one page, so a local would put the frame base *below* the mapping, and the fault becomes a VM
SIGSEGV that prints nothing `qemu-boot.forbidden` catches. This does **not** contradict TTY's "stage
in `main`'s frame, not a static" — that rule's load-bearing half is *the buffer outlives every call
that names it*, and a static satisfies it strictly better: the address never changes, so MFS's grant
to MEM is issued once at boot and `ensure_registered` never re-fires. It is reached only through the
**`Blocks` capability token**, whose `read(&mut self) -> Result<&[u8; N], i32>` makes "hold a
directory block across the next fetch" a **borrow-check error**. `uspace::SERVER_STACK_BYTES` exists
solely to carry `fs/mfs`'s `const _` tripwire for that, and `userland.rs` const-asserts its own
stack mapping against it.

**Two error-relay rules that read as contradictory and are not:** a `BDEV_READ` failure becomes
**`EIO`**, because MFS's client addressed a *file*; while a `SYS_SAFECOPY` failure against VFS's
grant is relayed **verbatim**, because `EPERM` and `EFAULT` are different caller bugs (the 5.3
rule).

MFS is **degraded, never fatal and never a panic** past `sef_startup` — a failed mount means
`ENODEV` to everything, MEM's `blocks = 0` precedent — and **every device-derived loop bound has a
cap** (`walk::dir_size`'s `MAX_DIR_BYTES`, `size < 0 → EIO`, `zone_ok` before every read). A corrupt
`size = i32::MAX` would otherwise spin MFS, blocking VFS, blocking init; VFS's probes cannot reach
that class, which is why it needs unit tests.

**VFS side.** `VFS_OPEN 0x801` / `VFS_READ 0x802` / `VFS_CLOSE 0x803` (`NR_VFS_MSGS` 1→4).
`VFS_READ` **reuses `VFS_WRITE`'s payload verbatim**, which is why `write.rs` is now `rw.rs` —
`advance`'s four rules are direction-agnostic, and "a peer reporting 0" *is* EOF — with a new
`open.rs` for what differs. **VFS reads the path with `sys_copy(caller_e, …)`**, the first live
consumer of D4's `SYS_COPY`-for-control-plane sentence, and the confused-deputy rule in its sharpest
form: `SYS_COPY` has **no per-target authorization at all**, so a payload-supplied source would let
any client read any process through VFS. **VFS does not loop on read** (POSIX allows a short
`read()`; it loops on `write` because it may not).

`fd.rs` gains `Fd::File { ino, pos }` with **no cached size** — EOF is `n == 0`, one source of
truth, and a cached size is what 5.10's writes would invalidate — `NR_FDS` 4→8, so "lowest free" and
"table full" are distinct tests, and interior-mutable `UnsafeCell<[FdRow; N]>` storage, whose rule
is **never hold a table borrow across a SENDREC**; `Fd` is `Copy`, so the borrow dies at the
destructuring `let`. `alloc_in` scans **upwards** from fd 3: a `.rposition()` returns 7 and passes
every "did I get a descriptor" check. `do_write` gains `Ok(Fd::File { .. }) => EROFS`, defined and
refused rather than folded into the unused case — the `BDEV_WRITE` precedent and 5.10's one-line
landing site.

**VFS's `bdev_demo` / `bdev_denials` retired**: MFS is the real BDEV client, so the ten-probe
battery relocated to `[diag mfs]` verbatim and VFS lost its `rootfs::*` / `BDEV_*` / `GET_RAMDISK`
imports. **DS ordering is a chain now** — `ds < tty < memory < mfs < vfs`, satisfied only by array
position in `kernel/build.rs`; both new lookups have `boot_endpoint` fallbacks with distinguishable
diag lines, and `bdev.ds ok` / `fs.ds ok` are what pin them. __`kernel-shared::rootfs` gained the
image's *contents*__ (`ROOTFS_MOTD`, `ROOTFS_PATTERN_*`, `ROOTFS_HELLO_PATH`, `rootfs_pattern_byte`)
so `build_rootfs` lost its literals and MFS's read proof is a *check* rather than a transcription.

**The landmine slice 5.8 had to defuse.** init's `("no-such", VFS_WRITE + 1, …, ENOSYS)` probe
became `VFS_OPEN` the moment the band grew — a real request answering `EINVAL`, silently retiring
`vfs.deny ok n=4` while every other marker stayed green. Band-relative (`VFS_RQ_BASE + NR_VFS_MSGS
as i32`) is the only spelling that survives a growing band.

(The standing rules are in [abi.md](../conventions/abi.md#request-band-allocation),
[servers-and-drivers.md](../conventions/servers-and-drivers.md#the-one-page-stack),
[servers-and-drivers.md](../conventions/servers-and-drivers.md#error-relay) and
[testing-and-markers.md](../conventions/testing-and-markers.md#markers).)

### Slice 5.9: exec-from-FS — **milestone B, Phase 5 complete** ✓ shipped (PR #52, merged 2026-08-04)

**Goal:** D6 + D12 — `PM_EXEC("/bin/hello")` end-to-end from the MFS root.

**Scope as planned:** `PM_EXEC` payload gains a path form; PM asks VFS to stage the binary (VFS
reads it from MFS into a static exec buffer — capped, asserted against hello's size — and
direct-grants it to PM's flow); `SYS_EXEC` grant form `(target, granter, grant_id, len)`; `elf.rs`
chunked-source refactor (boot-slice source + grant source) + `p_memsz` cap hardening; rollback on
every staging failure (the 4.6 rollback discipline). The name form and worker stay as boot-embedded
regression. init execs `/bin/hello`.

**Proof (milestone B):** full-stack trace — `[ksys SYS_EXEC]` grant form, MFS/BDEV read traffic,
then hello's printf on serial. `docs/plan.md` Phase 5 milestone flips; Phase 5 complete.

#### As built

Landed as two boot-green commits: **5.9a** the kernel half, **5.9b** the VFS/PM/init half plus the
`kernel/build.rs` drop.

**The boot-archive `hello` module is gone.** Keeping it would have made `minix.rs hello: Hello from
C!` unable to distinguish a boot-archive exec from a filesystem one — the same bytes reach the
console either way, the 5.5/5.6 "byte-identical markers" trap. `/bin/hello` in the MinixFS image is
now the only copy, so the five C markers *are* the exec-from-FS proof. `worker` stays boot-embedded
as the name-form regression, which is why `EXEC_ONLY_PROC_NR` is still in use.

**`argv[0]` is the path's basename, never the path.** That one choice left `EXEC_NAME_LEN`,
`PROC_NAME_LEN`, `execstack::INITIAL_STACK_MAX`, `worker`'s `ARGV0_NAMES` set, and the `name=hello`
marker all untouched — and it is what makes the sysroot-absent configuration (where `/bin/hello`
holds the `worker` ELF) boot clean instead of printing the forbidden `worker: stack FAIL`.

**Three things came out differently from the plan:**

1. **`rd_name` cannot express the NUL rule.** It reports "no NUL anywhere" and "a full-width name"
   identically, and those are `ENAMETOOLONG` and `EINVAL`. PM's `path::parse` therefore takes the
   payload's whole fixed-width field as raw bytes. Caught by the `too-long` probe on the first boot
   of 5.9b, not by review.
2. **VFS's boot selftest stages `/etc/pattern`, not `/bin/hello`.** Two reasons, both discovered by
   measurement: `hello`'s size is configuration-dependent (SDK ~46 KB, musl ~200 KB, fallback ~15
   KB) so a count keyed on it is vacuous in two configurations out of three, while `/etc/pattern` is
   40 KiB in all of them and the marker can assert `n=40960`; and it is ~5× cheaper, which matters
   because VFS's whole prologue runs before its receive loop and every client waits behind it.
3. **`qemu-smoke`'s boot budget went 45 s → 120 s.** exec-from-FS makes the boot genuinely longer —
   `hello` is no longer a memcpy out of the boot archive but ~200 KB read off the ramdisk through
   MFS and VFS, one `FS_MAX_IO` round at a time. Measured locally on the musl flavour: the last C
   marker moved from ~27% to ~71% of a 45 s run. CI's TCG is slower than local, so the budget was
   raised with real headroom rather than trimmed to what passes here.

**The fd-table debt was *not* forced, contrary to `fd.rs`'s prediction.** PM stages on the child's
behalf and VFS resolves the binary by inode with no descriptor at all, so no forked child ever holds
an fd. The comment is re-pointed rather than the debt closed; the first thing that will force it is
a program that opens a file *after* being exec'd.

#### Mutation table

Applied to an uncommitted tree, observed, restored from the scratchpad. Every run was checked for
`error[E` first — a mutation that fails to compile is indistinguishable from one that worked.

| mutation                                             | predicted                              | observed                                                                                                                                                                           |
| ---------------------------------------------------- | -------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| drop VFS's `m_source != PM` guard                    | `exec.deny FAIL not-pm`                | `exec.deny FAIL stage-not-pm` ✓                                                                                                                                                    |
| VFS grants PM `CPF_WRITE` not `CPF_READ`             | C markers vanish                       | C markers **and** `name=hello src=grant` vanish, plus `exec.deny FAIL not-elf` — louder than predicted, because the `/etc/motd` probe now gets `EPERM` where it expected `ENOEXEC` |
| drop the `off` advance in VFS's stage loop           | C markers vanish                       | VFS **wedges** — the loop never converges, so `exec.stage` never prints and every VFS client blocks behind it. Loud, but as a hang rather than a marker change                     |
| break the chunked source's `p_offset + off`          | `src=grant` present, C markers vanish  | *every* marker vanishes: `ElfSource::read` is shared, so the boot **servers** fail to load too. Less targeted than planned, and inherently so                                      |
| `load_exec_image` folds every `ElfError` to `ENOMEM` | `exec.deny FAIL not-elf`               | `exec.deny FAIL not-elf` ✓                                                                                                                                                         |
| PM ignores the leading `/`                           | C markers vanish                       | C markers and `src=grant` vanish, `name=worker src=name` only, plus `exec.deny FAIL is-dir` ✓                                                                                      |
| PM passes the whole path as `argv[0]`                | `name=hello src=grant entry=` vanishes | vanishes; the trace reads `name=/bin/hello src=grant`, and the C program still runs ✓                                                                                              |

**Expected non-results, recorded rather than chased:** the `p_memsz` / `MAX_PHNUM` caps and the
`size > VFS_EXEC_MAX` refusal move nothing, because no file in the image is large enough to trip
them — they are covered by the `execimage` and `stage` host tests instead. Likewise stubbing the
brand scan to `Ok` moves nothing, since every binary in the tree is branded.

**Boot matrix, all four rows.** (1) default/SDK: 85/85. (2) `MINIXRS_SDK=/nonexistent`, the in-tree
musl flavour CI builds: 85/85 at 45 s after the selftest was made cheaper (it needed 70 s before).
(3) `--no-default-features`: every 5.9 marker present, nothing forbidden; the only misses are the
stub markers the expected file requires and that configuration has no stubs, as designed. (4) musl
sysroot moved aside, so `/bin/hello` holds the `worker` ELF: nothing forbidden, boot does not hang,
`/bin/hello` execs from the filesystem with `argv[0] = "hello"`.

#### Design record

`SYS_EXEC` gained a **source selector** (`EXEC_SRC_OFF`, 0 invalid — the `SAFECOPY_*` convention)
plus a grant triple, so one call number covers both "a boot-archive module" and "the file VFS
staged". `4..20` stays `argv[0]` *and* the proc name in **both** forms; only where the bytes come
from changes.

The loader reads through `boot_image::elf::ElfSource` — an **enum, not a `dyn` trait** (no vtable
per header field) with `Bytes(&[u8])` and `UserGrant { ttbr0_pa, va, len }`, the granted arm
delegating to slice 5.1's `copy_from_user_as`, so there is **no new copy machinery**; the per-page
segment copy stays a single copy in both forms. The grant is validated by
`do_safecopy::verify_grant` (widened to `pub(super)`), *not* a second copy of its eleven checks, and
the read completes **before the point of no return**, so a granted buffer that is not an ELF leaves
the target on its old image.

`load_exec_image` takes an `ElfSource` and returns `Result<_, i32>`, ending the
fold-everything-into-`ENOMEM` stance: `Map(OutOfMemory)` → `ENOMEM`, `Source` → `EFAULT`, else
**`ENOEXEC`** — and without that distinction init's "stage `/etc/motd` and refuse it" probe would
prove nothing.

**`kernel/build.rs`'s pack-time `scan_brand` cannot reach a file**, so the runtime scan is the only
gate: `brand::scan_note_segment` is split out (pure, host-tested; `scan_brand` delegates) and the
loader stages each `PT_NOTE` through a bounded stack buffer — a brand past `MAX_NOTE_BYTES` is not
found, which is impossible for anything this repo's `user.ld` rule produces. `p_memsz` and `e_phnum`
are now *input* rather than build output, so `kernel-shared::execimage` carries `MAX_PHNUM`, a
**cumulative** `PageBudget` (per-segment checking misses a hundred reasonable-looking segments), and
`segment_end`; all pure and host-tested, the standing rule for predicates `kernel/src/` cannot test.

**The boot-archive `hello` module is dropped.** Keeping it would leave the five C markers unable to
distinguish a boot-archive exec from an FS one — the 5.5/5.6 byte-identical-markers trap — so
`name=hello src=grant entry=` deliberately keeps `name=` and `src=` **adjacent**: one substring
proving program *and* form. `worker` stays boot-embedded as the name-form regression.

**A leading `/` is `PM_EXEC`'s only discriminator** — one field, because two can disagree, and it is
already the FS band's rule. That also settles `com::ROOTFS_MODULE_NAME`'s warning: module names and
paths are disjoint namespaces, so nothing resolving a *path* can name the `rootfs` blob.

**`argv[0]` is the path's basename, never the path.** That single choice left `EXEC_NAME_LEN`,
`PROC_NAME_LEN`, `execstack::INITIAL_STACK_MAX`, `worker`'s `ARGV0_NAMES` set, and the `name=hello`
marker all untouched — and it is why the sysroot-absent config, where `/bin/hello` *is* the `worker`
ELF, boots clean instead of printing the forbidden `worker: stack FAIL`.

**`server-rt::rd_name` cannot express a NUL-padded field's rules**: it reports "no NUL anywhere" and
"a full-width name" identically, and those are `ENAMETOOLONG` and `EINVAL`. So PM's `path::parse`
takes the payload's whole fixed-width field as **raw bytes**. Caught by a boot probe, not by review;
apply it to every fixed-width inline field.

VFS's 256 KiB staging buffer is a `.bss` static for MFS's `BlockBuf` reason (a one-page server
stack), but needs **no capability token and no borrow discipline** — VFS never dereferences the
staged bytes, so the buffer's address is all it wants, and the crate keeps zero `unsafe` blocks.
Nothing releases the stage grant: re-granting per request bumps the sequence and kills the previous
id, and PM serialises exec.

**`VFS_EXEC_STAGE` carries the path inline** (its client is PM, which already holds it inline — no
`SYS_COPY`, and no source process to misname), and **a short stream is `EIO`, not a short stage**:
an ELF cannot be loaded in pieces by a loader with no filesystem, the one place in VFS where a
partial transfer is not a legitimate answer.

**The fd-table debt `fd.rs:48-60` predicted 5.9 would force was not forced** — PM stages on the
child's behalf and VFS resolves the binary by inode with no descriptor at all, so no forked child
holds an fd. The comment is re-pointed; the first thing that will force it is a program that opens a
file *after* being exec'd.

init's `exec_denials` battery (8 probes, all init exec'ing **itself**) is safe precisely because a
failed exec leaves the caller on its old image — so the battery *is* the rollback proof.

(The standing rules are in [kernel.md](../conventions/kernel.md#exec-from-a-filesystem) and
[servers-and-drivers.md](../conventions/servers-and-drivers.md#control-plane-travels-inline-data-travels-by-grant).)

### Slice 5.10 (stretch): MFS write path

The five-line sketch this section used to carry covered four separable things, so 5.10 is **split
into 5.10a and 5.10b** the way 5.9 was split into 5.9a/5.9b. The full design — nine numbered
decisions, the per-component breakdown, the error taxonomy, and the mutation/boot-matrix plan —
lives in
[`docs/superpowers/specs/2026-08-18-mfs-write-path-design.md`](../superpowers/specs/2026-08-18-mfs-write-path-design.md)
and is not duplicated here.

#### Slice 5.10a: the write path ✓ shipped (PR #53, merged 2026-08-20)

**Scope:** `BDEV_WRITE` becomes a real store in `drivers/memory`; one new FS request, `FS_WRITE`
(`FS_READ`'s payload verbatim, and a short write is normal — `CDEV_WRITE`'s stance, not BDEV's); MFS
gains a zone allocator covering direct **and** single-indirect zones, symmetric with the read path;
`VFS_WRITE` on an `Fd::File` stops answering `EROFS` and loops to MFS. A new zero-length
`/etc/scratch` enters the root image, because create does not exist yet and the target must already
be there. The RAM-backed image makes writes durable for the boot's lifetime (D3).

**Proof:** init writes 32 KiB to `/etc/scratch` in 4016-byte chunks — deliberately not a multiple of
the 4096-byte block, so partial-block splicing and boundary-crossing short writes are on the marker
— then re-opens and reads **every byte** back, comparing each against the generator. Reported
through fd 1, the path under test: `fs.write ok n=32768 v=32768`.

The plan said three 512-byte windows and `v=3`; the task-7 review widened it, and the widening is
the point rather than thoroughness for its own sake. There is no `lseek`, so reaching any offset
already read every byte before it — the windowed version issued the same messages and then discarded
most of the comparisons, leaving offsets 4096..28671 read but never checked. A mutation corrupting
one byte in the second direct zone prints `ok` under the windows and `fs.write FAIL verify off=4096`
under the full compare.

**The landmine it must defuse:** `open_denials`' eighth probe writes into a descriptor on
`/etc/motd` expecting `EROFS`. After 5.10a that write *succeeds* and overwrites the first bytes of
the file `fs.selfcheck` exists to verify — active corruption of a read proof, not merely a silently
retired probe. The probe is retired and the marker becomes `open.deny ok n=7`, so the change is a
visible diff rather than a count that quietly means something else. The 5.8 `VFS_WRITE + 1` lesson,
second occurrence.

**Three results from the verification pass worth keeping, because two of them contradict what this
plan predicted:**

- **The boot-time budget had to double.** Measured the repo's way — `grep -abo` the last required
  marker, divide by `wc -c` — on the **musl** flavour at a fixed 120 s: `hello: errno ok` sits at
  **27.98%** of the log at the merge base and **56.20%** with the slice, so the boot roughly doubled
  (~34 s → ~67 s wall locally). That cut the safety factor over local from 3.6x to 1.8x, which is
  inside the plausible range for how much slower the shared `ubuntu-24.04-arm` TCG runner is, so
  `qemu-smoke` went 120 s → **240 s**. Note `git stash` does not produce the "before" once the work
  is committed on a branch — detach to the merge base, stash only the doc edits, and build before
  the timed run.

- **The `dirty` half of the inode write-back condition has no boot probe.** Dropping it (keying the
  write-back on `grown != node.size` alone) moved **no marker at all**. init writes strictly
  sequentially, so every zone assignment also grows the size; the case the condition exists for —
  filling a hole in the middle of an existing file, which assigns `zone[i]` without moving `size` —
  is unreachable until there is an `lseek` or a truncate. The invariant is correct and stays; it is
  *unproven*, not covered, and 5.10b should probe it.

- **The `memory` driver's copy direction is guarded twice, independently.** Inverting it
  (`SAFECOPY_TO` for `SAFECOPY_FROM` in the driver's `do_write`) moved `fs.write FAIL read off=0`
  *and* `bdev.deny FAIL wr-dir rc=32` — the denial battery's write-direction probe catches it
  without any of the write path being involved.

- **The mid-write zone leak is a reachable denial of service, accepted and deferred to 5.10b**
  (review of PR #53). `do_write` allocates at step 3 and copies at step 4, and the copy is
  client-controlled: VFS's `rw::validate` range-checks the caller's buffer but cannot check that it
  is *mapped* — the kernel's page-table walk is the gate, per D5 — so `write(fd, unmapped_va, 4096)`
  arrives with a well-formed magic grant and fails after the zone is bitmap-marked. Looping it
  exhausts the image's **185 free zones** (measured, musl flavour) in 93–185 calls, after which
  every write including a legitimate one answers `ENOSPC` for the rest of the boot. Nothing in the
  shipped boot reaches it, which is why it was not fixed under review. Two things the fixer needs
  and should not have to re-derive, both recorded in `do_write`'s docstring: **clearing the bit on
  the error path is wrong in one of the three cases** (indirect slot with a pre-existing indirect
  block — the block on disk still names the zone, so freeing the bit hands it out twice, the exact
  corruption the ordering exists to prevent), and the cheap fix is not a rollback at all but a
  second staging buffer filled *before* the allocation, so that no client-controlled failure can
  occur after one. That costs a page of `.bss`; the one-page limit in MFS is the *stack*, not
  `.bss`.

##### Design record

`FS_WRITE = FS_RQ_BASE + 3` (`NR_FS_MSGS` 3→4) **reuses `FS_READ`'s payload field for field** —
inode, grant id, byte count, position — because it is the same question in the other direction, so
one wire codec and one clamp serve both; only the grant's direction bit differs (`CPF_READ` here,
`CPF_WRITE` there, checked by the kernel's `verify_grant` and re-implemented by no server).

**A short `FS_WRITE` is normal, and that is not an inconsistency with `BDEV_READ`'s
refuse-or-nothing:** BDEV refuses because *its* client is a filesystem that cannot interpret a
fraction of a block, while this request's client is VFS, whose whole job is hiding staging from
POSIX — the same reasoning that made `CDEV_WRITE` short-write and `FS_READ` clamp. There is still
**no granter field and no grant-offset field**, so **VFS re-grants per round** (a fresh magic grant
over exactly the bytes that round will move, revoked after), where the CDEV loop instead advances a
payload `offset` against one standing grant. Do not add an offset field to buy the other shape:
re-granting is what keeps the confused-deputy surface at zero, and costs one kernel call a round.

**What the single block buffer forces.** MFS has exactly one 4 KiB `.bss` block buffer (slice 5.8's
`Blocks` capability), and that constraint — not taste — fixes `do_write`'s step order: read the
inode, clamp, compute the grown size, *place the zone*, then read-modify-write the data block, then
write the inode back. Nothing may hold a block across the next fetch, so every intermediate is a
`Copy` scalar and `place_zone` must finish with the buffer free. `Blocks` gained `buf_mut` and
`write` for this, and its standing grant to the `memory` driver widened to **`CPF_READ |
CPF_WRITE`** — one buffer, both directions, one static address, so `ensure_registered` still never
re-fires. Widening the flags does **not** widen what any one call may do (the kernel checks the
direction per call), but it *does* re-arm anything that used `blocks.gid` as a
deliberately-insufficient grant: MFS's `bdev.deny` battery had a probe doing exactly that, and it
needed its own `CPF_WRITE`-only grant aimed at block 1 (the block `START_BLOCK = 2` leaves spare) in
the same change, or the probe would have written MFS's block buffer over the superblock.

**Two ordering rules in the allocator — the first mutation-proved, the second UNPROVEN.** A zone's
**bitmap bit is set before its number is stored**, so a mid-write failure leaks a zone rather than
letting two files share one — the recoverable failure over the corrupting one. That one is
mutation-proved. The second is not, and the difference matters. The inode write-back is keyed on
**"a zone was assigned *or* the size grew"**, never on the size alone: filling a hole in the middle
of an existing file assigns `zone[i]` without moving `size` at all, and keying on size would drop
that pointer while leaving its bitmap bit set — the bitmap and the inode disagreeing about a live
zone, corruption rather than a leak. **Know that the second half of that condition has no boot
probe**: init writes strictly sequentially, so every assignment also grows the size, and dropping
`dirty` from the condition moved *no* marker under mutation. It is a correct invariant guarding a
case nothing can reach until there is an `lseek` or a truncate — treat it as unproven, not as
covered, and probe it in the slice that adds one. (5.10b's `/etc/holey` is what finally reached it.)

**`/etc/scratch` and the marker that has to fail two ways.** `mkfs-mfs` ships `/etc/scratch`
**empty**, so every zone the file ends up with — the indirect block included — was allocated at
runtime. init writes `ROOTFS_SCRATCH_LEN` = 32 KiB to it (eight blocks: seven direct zones plus one
indirect, crossing the seam at 28672, the `/etc/pattern` reasoning one layer up), closes,
**re-opens** (a fresh `FS_LOOKUP`, so a size that never reached the inode is a short read rather
than something a descriptor remembered — nothing on this path caches a size), and compares **every
byte**. The marker is `minix.rs init: fs.write ok n=32768 v=32768`: `n=` is what the write path
reported, `v=` what read back correct, and they fail independently.

Verify the **whole file**, not windows. There is no `lseek`, so reaching any offset already read
every byte before it, and the three-window version this slice started with left offsets 4096..28671
read but never compared; a mutation corrupting one byte in the second direct zone printed `ok` under
the windows and `fs.write FAIL verify off=4096` under the full compare. Both numbers are literals —
init cannot format an integer — pinned to the constant by a `const _`.

**The 5.8 lesson, second occurrence.** `open_denials`' `write-file` probe asserted `EROFS` from
`write()` on a file descriptor — and the moment the write path became real, that probe stopped being
a denial and became a **successful overwrite of `/etc/motd`**, silently, with every other marker
still green. Retired, and `open.deny ok n=8` → `n=7` so the retirement is a visible diff rather than
a deletion nobody reviews. Same shape as 5.8's `VFS_WRITE + 1` probe becoming a real `VFS_OPEN`.

(The standing rules — write every denial probe so a growing capability makes it fail loudly, and the
mutation-testing method — are in
[testing-and-markers.md](../conventions/testing-and-markers.md#markers) and
[testing-and-markers.md](../conventions/testing-and-markers.md#mutation-testing).)

#### Slice 5.10b: create and truncate ✓ shipped (PR #54, merged 2026-09-02)

Full design — decisions, the per-component breakdown, the error taxonomy, and the
mutation/boot-matrix plan — lives in
[`docs/superpowers/specs/2026-08-25-mfs-create-truncate-design.md`](../superpowers/specs/2026-08-25-mfs-create-truncate-design.md)
and is not duplicated here.

**Scope, as shipped:** `FS_CREATE` (`FS_RQ_BASE + 4`, reusing `FS_LOOKUP`'s wire codec verbatim) and
`FS_TRUNC` (`FS_RQ_BASE + 5`); `NR_FS_MSGS` 4 → 6. MFS gained an inode allocator, directory-entry
insertion that grows a directory through the same `place_zone` a file's data uses, and a
zone-freeing path for truncate. `VFS_OPEN` gained a `flags` field (`VFS_FLAGS_OFF`) honouring
`O_CREAT` and `O_TRUNC` from the new `kernel-shared/src/fcntl.rs`; a flag bit outside `O_KNOWN` is
`EINVAL`, and the access-mode bits are accepted and ignored (no uid/gid/permission check anywhere in
the tree). The image gained sparse-file support, 128 inodes (was 64), a `/full` directory holding
exactly one block of entries with no free slot, and `/etc/holey`, `/deny/file` (in its own `/deny`
directory, so that the destructive `FS_TRUNC`-on-a-directory probe has nothing but its own sibling
to lose), plus paths for the create/dirgrow/leak probes.

Also closes the item 5.10a deferred into it: the **mid-write zone leak**. `do_write` now stages the
client's bytes into a second `.bss` buffer *before* anything is allocated, so no client-controlled
failure occurs after an allocation — a restaging, not a rollback, because clearing the bitmap bit on
the error path would be actively wrong for an indirect slot whose indirect block already existed
(the block on disk still names the zone, so freeing it there would hand one zone to two files).

`create` gets the same invariant, which review of this slice found it did not originally have:
`reserve_slot` places the directory's slot — including the zone its growth may need, the only
`ENOSPC` a client can provoke on that path — before `alloc_inode` claims anything, so a full image
answers a clean `ENOSPC` instead of orphaning one of the 128 inodes per attempt. That is the
strictly worse half of the same defect, because `do_trunc` can hand a leaked zone back and nothing
can recover an orphaned inode.

**Two corrections to 5.10a's hand-off, found while implementing this slice:**

- **`FS_TRUNC` does not make the `dirty` write-back case reachable.** 5.10a's hand-off predicted
  that a truncate followed by a write into the hole would exercise it; it does not, because a
  truncate zeroes `size` back to 0 and the next write starts at position 0 growing `size` again — an
  ordinary append, not a hole. Only a hole strictly **below** the file's end-of-file reaches the
  `dirty` case (a zone assigned with `size` unchanged), which is why the image ships `/etc/holey` —
  a file whose first block is a hole below its nominal size — rather than relying on truncate for
  it.
- **The truncate ordering (write the zeroed inode back, then free the zones) is itself unproven, for
  the same class of reason the `dirty` condition was.** Reversing it needs a failure between the two
  steps that nothing this slice can induce; the invariant is correct and stays, but no boot probe
  covers it. `do_trunc`'s docstring says so directly rather than letting a passing boot imply
  otherwise.

**Proof — five new init markers, in prologue order:** `fs.trunc ok n=0` (empty after truncating
`/etc/scratch`, the file the previous slice just filled); `fs.create ok n=25` (`/etc/new`, found
again through a lookup with no `O_CREAT`, proving the directory entry — not just the inode — reached
the device); `fs.dirgrow ok n=25` (`/full`'s 65th entry, forcing a second directory zone — the
append arm no other boot probe reaches); `fs.hole ok` (`/etc/holey`, the `dirty` write-back proof,
verified over the *whole* file rather than sampled windows); `fs.leak ok n=256` (256 writes at an
unmapped buffer, all `EFAULT`, followed by one real write that still succeeds — the zone-leak
closure). `fs.deny` grew 10 → 14, sent straight at the FS band (`EEXIST` with the target re-resolved
unchanged, create under a file parent → `ENOTDIR`, truncate on a directory → `EISDIR`, truncate on
inode 0 → `EINVAL`), and `open.deny` grew 7 → 11, sent through `VFS_OPEN`'s flags field (`O_CREAT`
naming a missing parent → `ENOENT`, `O_CREAT` naming an existing directory → `EISDIR`, `O_TRUNC`
naming a directory → `EISDIR`, and a flag bit outside `O_KNOWN` → `EINVAL`, spelled relative to
`O_UNKNOWN_BIT` rather than as a literal so a flag becoming real fails loudly instead of passing
vacuously).

##### Design record

`FS_CREATE = FS_RQ_BASE + 4`, `FS_TRUNC = FS_RQ_BASE + 5` (`NR_FS_MSGS` 4→6). `FS_CREATE` **reuses
`FS_LOOKUP`'s wire codec verbatim, request and reply alike** — the same question, with the file
brought into existence first — so it still has no granter field and no grant-offset field.
`VFS_OPEN` gained `VFS_FLAGS_OFF` (an i32, `fcntl`'s values) without moving `NR_VFS_MSGS`: a new
*field* on an existing request, not a new request. `O_CREAT` on a lookup miss dispatches to
`FS_CREATE`; `O_TRUNC` on a lookup hit dispatches to `FS_TRUNC`; `O_CREAT | O_TRUNC` on a miss takes
the create arm and stops, because a fresh file is already empty — so the truncate, when it runs at
all, runs before the descriptor is installed, and a failure never leaves a descriptor onto a
half-truncated file.

**The staging-buffer invariant, and why it is not a rollback.** `do_write`'s create path now copies
the client's bytes into a second 4 KiB `.bss` buffer *before anything is allocated*, so no
client-controlled failure can occur after an allocation. Clearing the bitmap bit on that error path
would have been actively *wrong* for an indirect slot whose indirect block already existed on disk —
the block still names the zone, so freeing the bit hands one zone to two files, corruption rather
than the leak. This is the fix for the reachable denial of service 5.10a named and deferred.

**`find_free_slot` must let `Occupied` beat `Free` across *every* block of a directory, not just
within one.** Scanning block-by-block and returning the first free slot found is backwards: a name
that already exists in a later block gets shadowed by a duplicate insert into an earlier block's
free slot — silently, no error, no marker. The fix scans every block for the name first, and only
then reuses a free slot.

**Two orderings this slice cannot prove, named rather than omitted.** The mutation matrix confirmed
both are unreachable by anything this system can induce: reversing `do_trunc`'s
free-before-write-back order moves no marker, and inserting the dirent before `write_inode` in
`create` moves no marker either — both need a failure *between* two steps that nothing here can
trigger. This is the 5.10a `dirty`-condition lesson applied to two new rules rather than repeated by
omission. Contrast `bitmap_clear`'s off-by-one, which also moves no boot marker but *is* caught by
two host tests in `fs/mfs/src/write.rs` — a real proof, just not at the boot-log layer.

**`/full` and `/etc/holey` exist because two arms are otherwise unreachable in *both* boot
configurations.** `/` holds 5 entries and `/etc` 7, against 64 slots per block, so without a
directory that is exactly full no create would ever grow a directory; and with no `lseek`, init
writes strictly forward, so only a pre-existing hole below EOF reaches the "zone assigned but size
unchanged" half of 5.10a's write-back condition. `/etc/holey` is what finally proves that invariant.

**A denial probe's flag must be spelled relative to `O_KNOWN`, not as a literal.**
`fcntl::O_UNKNOWN_BIT` is *derived* from `O_KNOWN`, so a flag becoming real makes the probe fail
loudly instead of passing vacuously — the same lesson as spelling unknown-request probes
band-relative.

**The boot budget moved a third time: 240 s → 600 s.** Measured the same way as before — the last
required marker's byte position as a fraction of a fixed-timeout log, on the musl flavour, against
the merge base: **26.90% → 61.61%**, a **2.29× jump**, leaving a **1.62× safety factor** at the old
budget (below the 1.78× that forced the previous raise). The 256-probe leak battery, each a doomed
grant plus a BDEV round trip, is the dominant cost. One methodology note worth keeping:
`MINIXRS_SDK` does **not** persist across separate shell invocations, so a measurement that sets it
in one command and boots in another silently measures the SDK flavour — check the embedded `hello`
size (~200 KB musl, ~47 KB SDK, ~15 KB fallback) rather than trusting the build warning.

Also worth a line: the image went to **128 inodes**, `mkfs-mfs` learned **sparse files**
(`Manifest::add_sparse`), and `kernel/build.rs` now asserts free-inode headroom against the
**built** image beside the existing free-zone assert.

(The standing rules are in
[servers-and-drivers.md](../conventions/servers-and-drivers.md#open-flags-and-dispatch) and
[ci.md](../conventions/ci.md).)

### Slice 5.11 (stretch): `/dev/null` + `/dev/zero` + `CDEV_READ` ◀ ready (branch `feature/slice-5.11-dev-null-zero`, pending merge)

Full design — decisions `Z1…Z10`, the per-component breakdown, the error taxonomy, and the mutation
plan — lives in
[`docs/superpowers/specs/2026-09-05-dev-null-zero-design.md`](../superpowers/specs/2026-09-05-dev-null-zero-design.md)
and is not duplicated here.

**Scope, as shipped:** `CDEV_READ` (`CDEV_RQ_BASE + 1`, `NR_CDEV_MSGS` 1 → 2; `CDEV_WRITE`'s payload
with the copy reversed, `0` is EOF, short reads legal); `CDEV_MINOR_NULL = 3` / `CDEV_MINOR_ZERO =
5` (MINIX 3's values) served by the memory driver with no clamp; the CDEV request codec lifted into
`server-rt`; VFS's `Fd::CharDev` names its driver and a three-row device-node table intercepts
`/dev/console`, `/dev/null`, `/dev/zero` after the path copy and before the mount; a console
`read()` now reaches TTY and hears `ENOSYS` from its unknown-request arm. **The 5.3 note that 5.11
would be "new minors, not new requests" was wrong for reading `/dev/zero`**, and is corrected
wherever it was copied.

**Proof:** `dev.zero ok n=64` (64 bytes, all zero, a second read not EOF), `dev.null ok n=35` (whole
count accepted, read is EOF and touches nothing), `dev.console ok` (written *through* the
`/dev/console` descriptor), `mem.ds ok`, `mem.deny ok n=5`, `open.deny` 11 → 12 (`/dev/nope` →
`ENOENT`).

---

#### Design record

`CDEV_READ = CDEV_RQ_BASE + 1` (`NR_CDEV_MSGS` 1 → 2) is `CDEV_WRITE`'s payload with the copy
reversed: the grant carries `CPF_WRITE`, the driver pushes with `SAFECOPY_TO`, **`0` is EOF and a
short read is legal**, so VFS sends one request and never loops — the `FS_READ` stance. The 5.3 plan
text saying 5.11 would be "minors, not requests" was wrong for *reading* zero; four in-tree copies
of it were corrected.

`/dev/null` and `/dev/zero` are **CDEV minors 3 and 5 of the memory driver** (MINIX 3's `NULL_DEV` /
`ZERO_DEV`), and **minors are a per-driver namespace**: the same driver's ramdisk is BDEV minor 0,
the request band tells them apart, and nothing asserts `CDEV_MINOR_*` against `BDEV_MINOR_*`.

The memory driver **never clamps** — `CDEV_MAX_IO` protects TTY's stack staging buffer and there is
no staging here — so a null or zero write answers the whole count with **no copy at all** (an
unmapped buffer *succeeds*, Linux's behaviour: never aim a `bad-buf` probe at `/dev/null`), and a
zero read fills the whole request from a 256-byte static in `CDEV_MAX_IO` steps, reporting partial
progress on a mid-way failure (5.4's rule).

The four-field CDEV parse lives in **`server-rt::cdev`** now that two drivers decode it; validation
stays per driver. VFS's `Fd::CharDev { dev: CharDriver, minor }` names its driver — an enum, not an
`Endpoint`, because `DEFAULT_ROW` is a `const` — and `servers/vfs/src/dev.rs` is a three-row
**device-node table** consulted **after the path copy and before the mount**: a device open needs no
filesystem, `O_CREAT` / `O_TRUNC` are ignored on a hit, the match is exact byte-for-byte, there is
no `/dev` on the image, and `/dev/other` falls through to MFS's `ENOENT`.

**A console `read()` is now a real `CDEV_READ` to TTY**, which answers `ENOSYS` from its
unknown-request arm until Phase 6 — the same errno init's `read-console` probe always expected, now
the driver's answer rather than VFS's guess, so Phase 6 adds one TTY arm and touches VFS not at all.

The memory driver's validator is probed from VFS's prologue (`mem.deny ok n=5`, last, after
`fs.deny`), including the first `CPF_WRITE`-required refusal on the CDEV band (MFS's `bdev.deny`
battery already exercises the same kernel check on a `BDEV_READ`). init's `dev.console ok` is
written **through** the `/dev/console` descriptor, never fd 1: that is the only thing that proves
the table row points at TTY. Paths are `callnr::DEV_*_PATH` so init and VFS cannot drift.

(The standing rules are in
[servers-and-drivers.md](../conventions/servers-and-drivers.md#minors-are-a-per-driver-namespace),
[servers-and-drivers.md](../conventions/servers-and-drivers.md#the-device-node-table) and
[servers-and-drivers.md](../conventions/servers-and-drivers.md#dont-answer-for-a-driver).)

## Boot-budget history

Kept here in full because it is a *measurement series*, not a rule: the numbers below cannot be
re-derived from the code, and each raise is evidence for the next. (The standing instruction — how
to measure, and that "it passes locally" is not the check — is in [ci.md](../conventions/ci.md).)

**A slice can break the boot-timing budget, and "it passes locally" is not the check.** The budget
was 45 s until slice 5.9, 120 s until 5.10a, and 240 s until 5.10b; **every raise has had the same
cause and the same evidence**, and the number will move again.

- **5.9:** `hello` stopped being a memcpy out of the boot archive and became ~200 KB read off the
  ramdisk through MFS and VFS, one `FS_MAX_IO` round at a time.
- **5.10a:** init writes 32 KiB to `/etc/scratch` and reads every byte back — ~130 more device round
  trips, plus the zone-allocation and inode write-back traffic under them.
- **5.10b:** init's leak battery issues 256 doomed writes before one good one, each a grant plus a
  BDEV round trip.

Measure a marker's *position as a fraction of a fixed-timeout log* (`grep -abo` the marker, divide
by `wc -c`) on the **musl** flavour — the one CI builds — and compare it against the same number at
the merge base.

- 5.9 moved the last C marker from **~27% to ~71% of a 45 s run**.
- 5.10a moved it from **27.98% to 56.20% of a 120 s run**, i.e. the boot roughly *doubled*, which
  cut the safety factor over local from **3.6x to 1.8x** and is why the budget went to 240 s.
- 5.10b moved it again, **26.90% to 61.61% of a 240 s run**, a **2.29x jump leaving a 1.62x
  factor**, and the budget went to 600 s.

CI's TCG is slower than local, so raise with real headroom rather than trimming to what passes here
— think in the *ratio*, not the wall-clock seconds, which are a property of the dev machine.

Two mechanical notes. `git stash` does **not** give you the "before" once the slice is committed on
a branch — detach to the merge base, and stash only the doc edits so `target/` and
`target/musl-sysroot` survive and the two boots differ in nothing but the code. And build (`cargo
build`) before the timed `cargo run`, or the rebuild lands inside the timeout and skews the
fraction.

A boot-time selftest that reads a **configuration-dependent** file is the usual culprit: prefer
`/etc/pattern` (40 KiB in every config, so its count is assertable too) over `/bin/hello`, which is
~46 KB with the SDK, ~200 KB with in-tree musl, and ~15 KB in the sysroot-absent fallback.

For iteration, the stub-free config is dramatically cheaper: 5.10b measured the `fs.*` markers
landing at **~0.14% of a 60 s stub-free log**, so `timeout 30` is ample there.

## Non-goals for Phase 5

PFS/pipes (Phase 7 — the shell is the first consumer); TTY input/IRQs + `SYS_IRQCTL` and any virtio
work (Phase 6); indirect grants; SENDA; signal handlers beyond the existing three-signal kill path;
threads/real futex; dynamic linking; malloc-backed C programs; x86_64 (Phase 8); mounts beyond the
single MFS root.
