# Phase 5: musl Fork + File Systems — design + slice plan

Produced by the chunk-6 design session (`phase-5-prep.md`), 2026-07-24. Every
design decision below is **locked** (decided with rationale, alternatives
recorded); the slice list is the working decomposition. Markers follow the
`docs/plan.md` convention: `◀ next` (unstarted), `◀ ready (branch …, pending
merge)`, `✓ shipped (PR #N, merged YYYY-MM-DD)`. Flip markers in each slice's
own PR — here and in `docs/plan.md`'s Phase 5 table.

**Milestone:** init execs `/bin/hello` — a C program compiled against the musl
fork — from an MFS root image: `PM_EXEC("/bin/hello")` → VFS lookup/read from
MFS-on-ramdisk → kernel grant-sourced ELF load → musl `printf` → VFS → TTY →
serial. An earlier intermediate milestone (A, slice 5.6) proves the musl half
with a boot-embedded hello before any filesystem exists.

---

## Where Phase 4 left the ground

Facts the design rests on (verified against source at session time):

- `SYS_DIAGCTL`, `SYS_COPY`, `SYS_SAFECOPY`, `SYS_SETGRANT`, `SYS_TIMES`,
  `SYS_IRQCTL` are already numbered (`0x605`–`0x612` range), granted to every
  `SRV_T` priv by the existing `k_call_mask` fill, and routed to the
  caller-local dispatch arm — they are one-line `ENOSYS` macros in
  `kernel/src/system/stubs.rs`. **Phase 5 adds zero new kernel-call numbers**
  (`NR_KERN_CALLS_PHASE4` stays 18); it fills in bodies.
- `Priv.grant_table` / `grant_entries` exist since slice 2.2, unused.
- Proc slots VFS 1, MEM 3, TTY 4, MFS 6, PFS 8 already have `BootEntry` rows,
  priv slots, and `SRV_T` `ipc_to`/`k_call_mask` wiring in
  `kernel/src/proc/table.rs` — they are simply never loaded because
  `kernel/build.rs` packs no ELF for them. Loading each is: crate + `user.ld`
  + a `servers` array row + a `qemu-boot.expected` line.
- Request bands `0x800`, `0x900`, `0xA00`, `0xB00` are free (between PM
  `0x700` and VM `0xC00`, all below `NOTIFY_MESSAGE = 0x1000`).
- The MXBI archive already supports non-ELF blobs: the boot loader skips
  negative `proc_nr` records and `BootImage::module_by_name` returns raw
  bytes with no ELF validation.
- `boot_image/elf.rs::load_into` takes a plain `&[u8]` (source-agnostic) but
  requires page-aligned `p_offset` per PT_LOAD and enforces W^X.
- IPC message copies are raw `read_volatile`/`write_volatile` through the
  active TTBR0; an in-range-but-unmapped user pointer is a **kernel panic**
  (the EL1 same-EL vector slots dump registers and panic; there is no fixup).
- The musl fork (`musl-minix`, sibling repo) is pristine: v1.2.5 + 102
  upstream commits, MIT-clean, zero MINIX changes yet. A static musl
  hello-world needs only `writev`, `exit`/`exit_group`, `set_tid_address`,
  and a benign `ioctl(TIOCGWINSZ)` at runtime — **no malloc, no brk/mmap** —
  and aarch64's thread-pointer setup is syscall-free (`msr tpidr_el0`).
- `kernel-shared/src/error.rs` values are bespoke (its "matches MINIX 3"
  comment is wrong): classic book-era MINIX numbering (EPERM 1 … EINVAL 22,
  EDEADLK 35, ENOSYS 38) is *identical* to Linux/musl numbering for the POSIX
  block; modern MINIX uses NetBSD numbering with MINIX-specific IPC errnos
  above 200.

## Locked design decisions

### D1. Console/stdio sink: minimal TTY server (TX-only), now

A real user-space TTY driver lands in Phase 5 — not a kernel printf shim as
the only console. Scope is deliberately minimal: **TX only, polling PL011**
(write `DR`, poll `FR.TXFF`), no interrupts. The kernel **pre-maps the UART
MMIO page (phys `0x0900_0000`) into TTY's address space at boot** with
Device-nGnRE attributes — TTY is a boot server, so this is a one-off boot
step like stack setup, and `AddrSpace` grows a device-memory mapping mode.
VFS routes fd 1/2 over a new CDEV band to TTY; TTY reads the payload via
grant (`SYS_SAFECOPY`) and writes the UART. Kernel messages keep using the
kernel's own writer; interleaving is acceptable (raw tick bytes already
interleave).

*Rejected:* `SYS_DIAGCTL` as the **only** console (no user-space driver ever
exercised in Phase 5 — but see D2, it still lands as the debug channel);
VM-mediated `VMCTL_MAP_PHYS` device mapping (more MINIX-authentic, revisit
for virtio-mmio in Phase 6); TX+RX with IRQ routing (`SYS_IRQCTL`, GIC → EL0
notification — nothing consumes input until the Phase 7 shell; deferred to
Phase 6 alongside virtio-console).

### D2. `SYS_DIAGCTL` becomes real, early — as the debug channel

Servers currently cannot print at all; every later slice needs observability
while TTY/VFS/grants are still under construction. `SYS_DIAGCTL` (MINIX 3
pedigree: `kernel/system/do_diagctl.c`) gets a body in slice 5.0 with an
**inline-payload** form — length + up to ~90 text bytes inside the 96-byte
message payload — so it needs *zero* user-copy machinery. A `server-rt`
helper loops longer strings. TTY/CDEV is the real stdio path; DIAGCTL is for
server bring-up debugging, exactly like MINIX's kernel message path.

### D3. Root image: MFS image in the MXBI archive + `memory` ramdisk over BDEV

The root filesystem is an **MFS-formatted image** built at compile time by a
new host tool (`tools/mkfs-mfs`), packed into the MXBI archive as a non-ELF
blob (`proc_nr = -1`, name `rootfs`). At boot the kernel **copies the blob
into freshly allocated RAM frames and maps them RW into the `memory`
driver's address space** (the archive copy in kernel `.rodata` stays
pristine; the RAM copy makes the 5.10 write path natural). MEM discovers
`(va, len)` via a new `SYS_GETINFO` selector. MEM serves the image over a
minimal **BDEV band** to MFS. This is the MINIX boot shape (`memory` driver
ramdisk) — Phase 6 swaps virtio-blk in under an unchanged MFS.

`tools/mkfs-mfs` is a host Rust binary sharing on-disk structs with the
`fs/mfs` library half, so the format logic is host-tested round-trip
(mkfs writes, mfs reader reads).

*Rejected:* MFS reading the image directly without BDEV (one fewer protocol
now, MFS rework when real block devices arrive); a cpio-style initramfs
unpacked by VFS (avoids MFS entirely — but MFS *is* the Phase 5
deliverable).

### D4. Grant model: real MINIX-style grants — direct + magic — plus `SYS_COPY`

Full MINIX shape, not an interim: each granting process keeps a **grant
table in its own address space** (`GrantEntry` array); `SYS_SETGRANT`
records `(addr, entries)` in the caller's `Priv` (the fields already exist);
`SYS_SAFECOPY` resolves a grant id, reads the entry from the *granter's*
address space, validates (kind, `CPF_READ`/`CPF_WRITE` access, grantee
endpoint, range, idx+seq staleness — MINIX `GRANT_SHIFT` packing), then
copies. Two grant kinds in Phase 5:

- **direct** — granter grants a range of its own memory to a grantee
  (server↔server, e.g. PM→TTY console writes);
- **magic** — a server-grade granter (VFS) grants one process's memory to
  another (TTY/MFS reading a *user's* buffer) — the real single-copy
  read/write data path. Gated on server-grade priv.

**Indirect grants are deferred** (documented `EINVAL` arm) — nothing in
Phase 5 re-grants a received grant.

`SYS_COPY` (raw privileged endpoint+addr copy, MINIX `sys_datacopy`) is
implemented on the same engine — VFS uses it for small control-plane reads
(e.g. fetching a path string from a caller).

**Copy engine + inherent fault safety:** all cross-address-space access goes
through **explicit page-table walks via the HHDM** — the kernel never
dereferences a user VA through the live TTBR0 for grant work. An unmapped
page is a walk miss, returned as `EFAULT`; no exception-fixup machinery
exists or is needed. Page-at-a-time: walk source frame, walk destination
frame, `memcpy` through HHDM aliases. (`AddrSpace` already promises all
table access is HHDM-based and works on non-active address spaces.)

`GrantEntry` is `#[repr(C)]` in `kernel-shared` (flat struct — flags, seq,
grantee, third-party endpoint, addr, len — no union needed for
direct+magic), with the CPF flag constants and id packing helpers,
host-tested.

*Rejected:* direct-only grants (double copy through VFS for every FS byte,
magic retrofitted later anyway); `SYS_COPY`-only interim (ungoverned byte
movement; the chunk-6 brief explicitly prefers real grants).

### D5. Fault-safe user copy: PT-walk replaces `read/write_volatile`

The chunk-6-mandated opening slice. The same walk-via-HHDM technique from D4
replaces the two message-copy functions (`copy_msg_from_user` /
`copy_msg_to_user`): a bad user message pointer becomes **`EFAULT` in the
caller's `x0`**, never a panic. The deferred receive-side flush
(`flush_deliver_msg`, currently `let _ =` with a comment admitting the error
is dropped) surfaces failure as `EFAULT` in the *receiver's* parked `x0` —
the receiver asked for delivery to a bad buffer; the message is consumed.
`user_va_ok` stays as the cheap range/alignment pre-gate.

*Rejected:* a real `el1h_sync` handler + ELR fixup table (Linux-style
`extable`) — strictly more machinery for the same observable behavior, and
the PT-walk engine has to exist for grants anyway. Revisit only if message
round-trip cost ever matters (walks can be cached per dispatch).

### D6. ELF authority for FS-backed exec: the kernel keeps it

`boot_image/elf.rs` stays the single ELF loader. It is refactored over a
small **chunked-source abstraction** — read header/phdrs into stack buffers,
copy segment pages — with two sources: the existing boot-image byte slice,
and a **granted user-space buffer** (cross-AS page reads on the D4 engine).
`SYS_EXEC` gains a second payload form carrying `(granter, grant_id, len)`
alongside the existing name form (the name form stays for boot-embedded
regression). PM/VFS stage the file: VFS reads the whole binary from MFS into
a static exec buffer (capped; a compile-time assert covers `/bin/hello`),
direct-grants it to PM's exec flow. No kernel heap, no kernel staging.

The loader stays strict (page-aligned `p_offset`, W^X): user binaries are
linked to comply (see D13). Frame-exhaustion hardening (a `p_memsz` cap)
lands with the grant-source form, since exec input stops being
build-produced.

*Rejected:* a user-space loader in VFS/PM/VM (MINIX-authentic `libexec`
direction — needs map-into-third-party-AS surface through `SYS_VMCTL` and a
second loader implementation for the same milestone; revisit if Phase 7+
wants PIE/interpreters).

### D7. Errno ABI: classic-MINIX values (≡ Linux/musl), MINIX extras above 200

`error.rs` is renumbered once, before any C exists (slice 5.4):

- **POSIX block:** classic book-era MINIX values, which are identical to
  Linux/musl numbering for everything Phase 5 needs (EPERM 1 … EACCES 13,
  EFAULT 14, EINVAL 22, EDEADLK 35, ENOSYS 38 — negated in-kernel as today).
  Where classic MINIX and Linux ever diverge, **musl's value wins** (the
  point is that musl's stock `bits/errno.h` and `syscall_ret.c`'s
  `r > -4096UL` convention work unmodified).
- **MINIX-specific IPC errnos** (`EDEADSRCDST`, `EDONTREPLY`, `EGENERIC`,
  `ELOCKED`, `EBADCALL`, …): modern-MINIX 200-band values (202, 203, 204,
  208, 209, …) — clear of Linux's entire range, so they can never collide
  with a musl-visible errno.
- The missing FS errnos (EEXIST, ENOTDIR, EISDIR, ENOTTY, EMFILE, ENFILE,
  ENOSPC, EROFS, ESPIPE, EPIPE, EBUSY, ENODEV, ENAMETOOLONG, ENOTEMPTY,
  EXDEV, …) are added in the same renumber at their Linux values.
- The wrong "values match MINIX 3" comment is fixed to state the actual
  policy above.

*Rejected:* keeping bespoke values + a translation table in the musl wrapper
(permanent two-numbering-systems tax); NetBSD/modern-MINIX numbering (most
faithful to the modern reference tree, but every errno diverges from
Linux muscle memory and musl needs a full `bits/errno.h` override).

### D8. C header bridge: hand-rolled generator, frozen when the first C lands

A small host tool (`tools/gen-c-headers`) depends on `kernel-shared` as a
normal Rust dependency and **prints** the C headers (`minix/ipc.h` —
`message` struct + IPC primitive numbers; `minix/com.h` — endpoints;
`minix/callnr.h` — call numbers + payload offsets; errno values for
`bits/errno.h` verification). Values are read from the live Rust constants,
so they are correct by construction — no const-eval limits, no drift
*possible* because the headers are **generated at build time into the musl
sysroot, never committed**. The CI musl job regenerates them and compiles
them (`clang -fsyntax-only`) so breakage fails fast. **ABI freeze point:
slice 5.6** (first C file) — after it, `Message` layout, call numbers,
endpoints, and errnos are frozen; changes require a deliberate ABI-bump PR
touching both repos.

*Rejected:* cbindgen (cannot const-eval the `ProcNr::new()` endpoint
constants — would need mirror consts for everything plus config to track);
hand-written headers + drift test (manual upkeep as the ABI grows).

### D9. musl vendoring: git submodule at `external/musl`

The fork enters the build as a **submodule pinned to `musl-minix`'s `main`**.
Rationale: keeps 1537 C files out of this repo's history; `external/**` is
already Sonar-excluded; cargo-audit/deny/geiger only walk the cargo graph so
a C tree is invisible to them either way; MINIX changes land as reviewable
PRs in the fork repo and are pinned here by submodule bumps. The kernel
build **presence-checks the submodule** and, when uninitialized, skips
packing hello with a `cargo::warning` and falls back gracefully (see 5.6) —
plain `cargo` workflows never break for contributors who haven't run
`git submodule update --init`. musl license attribution (MIT) is recorded in
the repo's license notes since cargo-deny cannot see it.

*Rejected:* vendoring the tree in-repo (~11 MB + 1537 C files of history,
manual upstream sync); build-time fetch at a pinned SHA (network-dependent
builds, pin outside git metadata).

### D10. C toolchain: clang `--target` + llvm-ar + rust-lld

`CC="clang --target=aarch64-unknown-linux-musl"` with clang's integrated
assembler compiles musl on both macOS (Xcode/Homebrew clang) and ubuntu CI
with the same flags; `llvm-ar` archives `libc.a`; **final links use
`rust-lld`** (ships with the pinned Rust toolchain, ld.lld-compatible, GNU
flavor) so no platform linker is involved. musl is configured
`--disable-shared` (no ldso — Phase 5 is static-only). One build script
(`tools/build-musl.sh`) drives configure/make into a cached sysroot under
`target/`.

*Rejected:* a dedicated cross-gcc (per-developer install, different names
per platform); zig cc (hermetic, but a third-party toolchain dependency).

### D11. Scope fences

**In (post-milestone stretch):** MFS write path (5.10); `/dev/null` +
`/dev/zero` via the memory driver's CDEV minors (5.11).

**Out — deferred with owners:** PFS/pipes → **Phase 7** (first consumer is
the shell; `plan.md`'s old Phase 5 bullet moves out); TTY RX/IRQs +
`SYS_IRQCTL` → Phase 6; indirect grants → when a re-granting consumer
exists; SENDA → unchanged non-goal; more signals/handlers → Phase 5.x+ as
needed (musl hello needs none); threads (real futex, `syscall_cp` porting) →
far future; dynamic linking/ldso → far future; malloc-backed C programs
(the `mmap` wrapper is a link-satisfying stub; hello provably pulls in no
malloc) → revisit when a real program needs it, likely via `VM_MMAP` +
opening USER `ipc_to` to VM.

### D12. Milestone bar

Phase 5 closes on **exec-from-FS** (slice 5.9): every subsystem — grants,
TTY/CDEV, VFS, BDEV/ramdisk, MFS, musl, grant-sourced kernel ELF load — in
one QEMU trace. The boot-embedded musl hello (5.6) is intermediate
milestone A, deliberately de-risking musl independently of the FS stack.

### D13. Derived decisions (recorded so slices don't re-litigate)

- **Request bands:** VFS `VFS_RQ_BASE = 0x800`; VFS↔FS protocol
  `FS_RQ_BASE = 0x900`; block devices `BDEV_RQ_BASE = 0xA00`; character
  devices `CDEV_RQ_BASE = 0xB00`. All below `NOTIFY_MESSAGE`, each with the
  conventional `const _` ordering guards and host tests.
- **USER priv:** `populate_user_priv`'s `ipc_to` widens `{PM}` →
  `{PM, VFS}` (POSIX shape: user procs talk to PM and VFS, nothing else).
- **exec initial stack:** the kernel builds a **Linux-SysV frame** —
  `[argc][argv…][NULL][envp…][NULL][auxv pairs][AT_NULL]` with minimal auxv
  (`AT_PAGESZ`, `AT_PHDR`/`AT_PHNUM`/`AT_PHENT` when the first PT_LOAD maps
  the headers, `AT_NULL`) — so musl's crt/`__libc_start_main`/`__init_tls`
  run **unpatched**. Keep-the-musl-diff-minimal is the standing principle:
  the fork's delta stays `arch/aarch64/syscall_arch.h` (gutted) +
  `src/minix/` (new) + build glue.
- **User-binary link contract:** `-z max-page-size=4096
  -z separate-loadable-segments` (page-aligned `p_offset` — the kernel
  loader stays strict rather than learning offset slack).
- **musl wrapper set for milestone A:** real `writev`/`write` (→
  `VFS_WRITE`), `exit`/`exit_group` (→ `PM_EXIT`), `set_tid_address`
  (constant tid), `ioctl` (→ `-ENOTTY`, harmlessly forcing full buffering);
  link-satisfying stubs for `close`, `lseek`, `ppoll`, `openat`, `mmap`,
  `futex`. Real `open`/`read`/`close` wrappers follow in 5.8+.
- **`fs/mfs` shape:** library half (`no_std` on-disk structs + pure
  superblock/inode/dirent/zone logic, host-tested, consumed by
  `tools/mkfs-mfs`) + server bin half (SEF/IPC glue) — the measured-submodule
  Sonar convention (`fs/**/src/main.rs` joins `sonar.coverage.exclusions`;
  the lib joins the CI miri list).
- **Path resolution simplification:** VFS sends MFS whole paths; MFS
  resolves internally. MINIX's component-at-a-time `REQ_LOOKUP` protocol is
  deliberately simplified for a single-FS, root-only world; revisit when
  mounts/multiple FSes arrive (Phase 6+).
- **FS request subset:** READSUPER, LOOKUP, PUTNODE, READ, STAT-lite;
  GETDENTS only if free. Write-side requests arrive with 5.10.

---

## Slice decomposition

Ordering rationale: copy-safety first (everything after touches user
memory); grants second (TTY, CDEV, BDEV, FS, exec all consume them); console
third (every later slice gains visible EL0 output); ABI + musl **before**
the FS slices (the root image must contain `/bin/hello`, so musl must build
before `mkfs-mfs` packs an image); FS next; exec-from-FS closes the
milestone; stretch slices after.

### Slice 5.0: fault-safe user copy + real `SYS_DIAGCTL` ◀ next

**Goal:** no user pointer can panic the kernel (D5), and servers can print
(D2) — the observability + safety floor for everything after.

**Scope:** replace `copy_msg_from_user`/`copy_msg_to_user` with PT-walk +
HHDM copies against the caller's (resp. receiver's) address space; `EFAULT`
to the caller's `x0` on send/kernel-call request/reply, `EFAULT` to the
receiver's parked `x0` when the deferred `flush_deliver_msg` hits a bad
deliver buffer (replacing the silent `let _ =`). `SYS_DIAGCTL` body:
inline-payload text (len + bytes in the 96-byte payload), kernel writes it
to the UART with a `[diag <name>]`-style prefix; `server-rt` gains a
`diag_print` helper that chunks longer strings. Drive-bys nominated by
`phase-5-prep.md` for the first PR touching these files: stale era comments
in `kernel/src/ipc/{message.rs,senda.rs,mod.rs}`, and `ipc_const.rs`'s wrong
"`x16`" trap-ABI comment (the real register is `x1`).

**Proof:** a one-shot deliberate bad-pointer IPC from the stub battery
(boot-stubs-gated) traces `result=-15` (`EFAULT`; becomes `-14` after 5.4)
with no panic; a server's `diag_print` line appears in the boot log and
joins `tests/qemu-boot.expected`.

### Slice 5.1: grant table + `SYS_SETGRANT` / `SYS_SAFECOPY` / `SYS_COPY`

**Goal:** the D4 grant model, live end-to-end between two boot servers.

**Scope:** `kernel-shared`: `GrantEntry` (`#[repr(C)]`), CPF flags, idx+seq
grant-id packing, host tests. `server-rt`: static grant table
(`UnsafeCell` newtype, the `vm/region.rs` pattern), `cpf_grant_direct` /
`cpf_grant_magic` / `cpf_revoke`, lazy one-time `SYS_SETGRANT` registration.
Kernel: `do_setgrant` (record after `user_va_ok`), `verify_grant` (read the
entry from the granter's AS via PT walk; validate kind/access/grantee/range/
seq; magic gated on server-grade priv), the page-at-a-time cross-AS copy
engine, `do_safecopy` (direction flag in the payload — one call number
covers from/to), `do_copy` (raw privileged copy on the same engine).
Indirect grants: documented `EINVAL`.

**Proof:** VFS direct-grants a checksummed buffer at init and publishes the
grant id through DS (`grant.test` key — DS is already a name→i32 registry);
PM retrieves it, `SYS_SAFECOPY`s the buffer, and `diag_print`s the checksum.
Marker line in `qemu-boot.expected`; `[ksys]` traces show the new calls.

### Slice 5.2: TTY server (TX-only, premapped PL011) + CDEV band

**Goal:** D1 — first user-space driver; EL0-originated text on the serial
console.

**Scope:** `AddrSpace` grows a Device-nGnRE mapping mode; boot pre-maps the
UART page into TTY's AS at a `kernel-shared` const VA. New `drivers/tty`
crate (workspace member, `user.ld`, SEF loop, DS publish; polls `FR.TXFF`,
writes `DR`, LF→CRLF like the kernel writer). `CDEV_RQ_BASE = 0xB00` with
`CDEV_WRITE {minor, granter, grant_id, len}` → TTY safecopy-reads and
transmits; replies bytes-written. `kernel/build.rs` `servers` array +1
(proc_nr 4); `qemu-boot.expected` gains the `[as]` line and the demo marker.
`CDEV_READ` is deliberately absent (Phase 6).

**Proof:** PM (or RS) retrieves TTY's endpoint from DS at init and
`CDEV_WRITE`s a banner via direct grant — the banner reaches serial *from
EL0* (no kernel trace prefix), distinguishable from kernel output.

### Slice 5.3: VFS write path — fd 1/2 → CDEV(TTY)

**Goal:** the POSIX write shape: user proc → VFS → TTY, single copy.

**Scope:** `VFS_RQ_BASE = 0x800`; `VFS_WRITE {fd, buf, len}` (SENDREC from
the caller). VFS pre-opens fd 0/1/2 → console (minor 0) in a static
per-proc fd table (`NR_SERVED_PROCS` rows); `write(1/2)` makes a **magic
grant** naming the caller's buffer and forwards it over `CDEV_WRITE` to TTY
(the D4 single-copy data path, first magic-grant consumer); other fds
`EBADF`, other ops `ENOSYS` for now. `populate_user_priv` opens
`ipc_to = {PM, VFS}`. init sends one `VFS_WRITE` banner before its fork
loop (raw `minix-ipc` message — init stays a plain Rust user program).

**Proof:** init's banner reaches serial through VFS→TTY; `[ipc]` head
traces show init→VFS SENDREC + VFS→TTY CDEV round-trip.

### Slice 5.4: errno renumber + `tools/gen-c-headers`

**Goal:** D7 + D8 — the ABI is C-ready before any C exists.

**Scope:** renumber `error.rs` per D7 (classic/Linux POSIX block, 200-band
MINIX extras, add the missing FS errnos); sweep the workspace for hardcoded
errno literals (host tests + boot markers are the net); update 5.0's
`result=-15` expected marker to `-14`; fix the errno policy comment. New
`tools/gen-c-headers` host crate (workspace member) emitting the D8 headers
to a target directory; a host test snapshots the generated `message` struct
layout against `Message`'s const asserts. Deliberately mechanical and
isolated, like the chunk-5 toolchain bump.

**Proof:** QEMU boot markers green (values changed, behavior identical);
`cargo run -p gen-c-headers` output compiles under
`clang -std=c11 -fsyntax-only` (checked in CI from 5.6 on).

### Slice 5.5: exec ABI — SysV initial stack + minimal auxv

**Goal:** D13's stack contract — musl's crt runs unpatched.

**Scope:** `do_exec`/`load_exec_image` build the Linux-SysV frame on the
stack page (argc=1, argv[0]=exec name, empty envp, auxv: `AT_PAGESZ`,
`AT_PHDR`/`AT_PHNUM`/`AT_PHENT` when the first PT_LOAD covers the ELF
header, `AT_NULL`), writing through the new AS via HHDM before release; `sp`
points at `argc`. Boot-server loads keep the bare-`sp` path (servers'
`_start` doesn't read the stack). Worker is unaffected by content (it reads
nothing) but proves the frame doesn't break a raw `_start`.

**Proof:** worker cycle unchanged (fork/exec/exit markers green); a new
`[exec]` trace line reports `sp`/argc/auxv count and joins the expected
file.

### Slice 5.6: musl submodule + `src/minix` port + boot-embedded hello — **milestone A**

**Goal:** a C program built against the musl fork runs on minix.rs (exec'd
from the boot archive; the FS comes later).

**Scope (fork repo, as PRs there, pinned here by submodule bump):** commit
the existing `linux-inventory.md`; gut `arch/aarch64/syscall_arch.h`
(replace `svc 0` Linux ABI with calls into the MINIX layer; drop `VDSO_*`);
add `src/minix/`: the IPC trap asm (x0=endpoint, x1=primitive, x2=&msg —
matching `minix-ipc`), `_syscall.c`, and a dispatcher mapping the milestone
syscall set per D13 (`writev`/`write`, `exit`/`exit_group`,
`set_tid_address`, `ioctl`→`-ENOTTY`, stubs). **Scope (this repo):**
submodule at `external/musl`; `tools/build-musl.sh` (D10: configure
`--disable-shared`, clang/llvm-ar, cached sysroot, `gen-c-headers` output
installed); `userland/hello` (`hello.c`); `kernel/build.rs` packs hello as
an exec-only module **when the submodule is initialized, else packs the
worker ELF under the name `hello`** (name-level fallback: init always execs
`hello`, boots are green either way, no feature flags); init's exec target
flips `worker` → `hello`; CI (qemu-smoke + a musl-build step): submodule
checkout, clang/llvm-ar install, sysroot cache keyed on submodule SHA +
toolchain; header `-fsyntax-only` check (5.4) wired in. License note for
musl (MIT) added to the repo's licensing docs.

**Proof (milestone A):** `printf("Hello from C on minix.rs!\n")` output
reaches serial through VFS→TTY from a musl-linked binary exec'd out of the
MXBI archive; marker in `qemu-boot.expected`.

### Slice 5.7: BDEV band + `memory` ramdisk driver + `tools/mkfs-mfs` + rootfs blob

**Goal:** D3 — a block-device story with a real MFS image behind it.

**Scope:** `fs/mfs` library half first: MinixFS v3 on-disk structs
(superblock/inode/dirent, 4 KiB blocks) + pure readers, host-tested.
`tools/mkfs-mfs` host tool builds `rootfs.img` from a manifest
(`/bin/hello` from 5.6's build, `/etc/motd`); round-trip host tests against
the mfs readers. `kernel/build.rs` runs mkfs and packs the image as
(`rootfs`, −1). Kernel boot: copy blob to RAM frames, map RW into MEM's AS;
new `SYS_GETINFO` selector returns `(va, len)`. `drivers/memory` becomes a
real server (roster +1, proc_nr 3): serves `BDEV_RQ_BASE = 0xA00`
`BDEV_READ`/`BDEV_WRITE {minor, block, count, granter, grant_id}` against
the mapped image via safecopy.

**Proof:** MEM's init `diag_print`s image size + superblock-magic check
(`[as]` roster grows to 13); BDEV round-trip is exercised by host tests here
and by live IPC in 5.8.

### Slice 5.8: MFS server (read-only) + FS band + VFS mount/open/read

**Goal:** files readable through the full VFS→MFS→BDEV→ramdisk stack.

**Scope:** `fs/mfs` server half (roster +1, proc_nr 6): SEF loop serving
`FS_RQ_BASE = 0x900` — READSUPER (via BDEV_READ to MEM), LOOKUP (whole-path
per D13), PUTNODE, READ (data moved by magic grant direct to the
requester's buffer), STAT-lite. VFS: lazy root mount on first open;
`VFS_OPEN`/`VFS_READ`/`VFS_CLOSE` (+`VFS_STAT` if free) against the static
fd table; read path forwards a magic grant to MFS. Sonar `fs/**/src/main.rs`
exclusion + miri list addition land here with the code. May split a/b
(MFS half / VFS half) like 4.6 — one slice number.

**Proof:** init opens `/etc/motd`, reads it, writes it to fd 1 — the motd
text crosses BDEV→MFS→VFS→TTY and lands on serial.

### Slice 5.9: exec-from-FS — **milestone B, Phase 5 complete**

**Goal:** D6 + D12 — `PM_EXEC("/bin/hello")` end-to-end from the MFS root.

**Scope:** `PM_EXEC` payload gains a path form; PM asks VFS to stage the
binary (VFS reads it from MFS into a static exec buffer — capped, asserted
against hello's size — and direct-grants it to PM's flow); `SYS_EXEC` grant
form `(target, granter, grant_id, len)`; `elf.rs` chunked-source refactor
(boot-slice source + grant source) + `p_memsz` cap hardening; rollback on
every staging failure (the 4.6 rollback discipline). The name form and
worker stay as boot-embedded regression. init execs `/bin/hello`.

**Proof (milestone B):** full-stack trace — `[ksys SYS_EXEC]` grant form,
MFS/BDEV read traffic, then hello's printf on serial. `docs/plan.md` Phase 5
milestone flips; Phase 5 complete.

### Slice 5.10 (stretch): MFS write path

**Scope:** `BDEV_WRITE` consumer side; MFS write/create/truncate (+ the
write-side FS requests); `VFS_WRITE` to real fds routes to MFS (fd>2), and
`VFS_OPEN` grows `O_CREAT`-lite. The RAM-backed image makes writes durable
for the boot's lifetime (D3).

**Proof:** init writes a file, reads it back, echoes it to fd 1.

### Slice 5.11 (stretch): `/dev/null` + `/dev/zero`

**Scope:** MEM gains CDEV minors for null/zero on the 5.2 band; VFS grows a
static device-node table (`/dev/null`, `/dev/zero`, `/dev/console` →
(driver, minor)) intercepting paths ahead of FS lookup (deliberate
simplification — no on-disk device inodes yet).

**Proof:** reading `/dev/zero` and writing `/dev/null` from init, traced
and echoed.

---

## Non-goals for Phase 5

PFS/pipes (Phase 7 — the shell is the first consumer); TTY input/IRQs +
`SYS_IRQCTL` and any virtio work (Phase 6); indirect grants; SENDA; signal
handlers beyond the existing three-signal kill path; threads/real futex;
dynamic linking; malloc-backed C programs; x86_64 (Phase 8); mounts beyond
the single MFS root.
