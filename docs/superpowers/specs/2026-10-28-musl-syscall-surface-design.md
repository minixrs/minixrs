# Pre-Phase-6 chunk 5 — the musl syscall surface: file I/O, lseek, and malloc from C

**Date:** 2026-10-28 **Status:** design, pending review **Branch:** `feature/musl-syscall-surface`
**Predecessor:** pre-Phase-6 chunk 4 (Phase 6 tracker, PR #62) **Tracker:**
[`docs/plans/phase-6-prep.md`](../../plans/phase-6-prep.md) chunk 5

Everything slices 5.8–5.10b built — open, read, write, close, create, truncate, the `/dev` nodes —
is reachable only from Rust. The musl fork's `src/minixrs/_syscall.c` dispatches six Linux call
numbers (`writev`, `write`, `exit`, `exit_group`, `set_tid_address`, `ioctl`) and answers `-ENOSYS`
to the rest, so a C program can print but cannot read a file, and `malloc` has nothing under it.
This chunk closes that gap far enough for **one C program to read a MinixFS file twice over — once
through raw descriptors, once through `fopen` and malloc — and to seek inside it.**

Decisions are labelled `M1…M19`: chunk-local, distinct from the phase-level `D1…D13` (which stay
locked) and from chunk 2's `V*` and chunk 3's `I*`.

---

## 1. Intent

- **Why now.** The Phase 6 disk root invites disk-resident C programs, and the first one beyond
  `hello` needs file I/O and malloc. Landing the shim before virtio means a later disk bring-up
  failure is never also a libc failure.
- **What success is.** The proof below appears in a boot log produced by CI's musl flavour, every
  marker is mutation-tested, and no request a user process can now send VM lets it act on a *third*
  process (the confused-deputy class). **Resource exhaustion is explicitly not covered** — user's
  ruling: one process can still exhaust guest memory; M17 only stops that from wedging VM, and a
  per-process cap is deferred (§4).
- **Scope set by the user.** The proof and only the proof — no further C programs. The stdio/malloc
  variant and `lseek` are in, at the user's request. `SEEK_END` answers `EINVAL` (user's ruling) and
  does not get a new FS request.

### Goals

1. Shim gains `openat`, `read`, `readv`, `close`, `lseek`, `brk`, `mmap`, `munmap`.
2. A new VFS request, `VFS_LSEEK`.
3. USER's privilege reaches VM, and VM is safe to be reached.
4. One C program, `/bin/fileio`, proving all of the above with self-checking boot markers.

### Non-goals

- `SEEK_END`, `fstat`/`stat`, or any request that reports a file's size (user's ruling: `EINVAL`
  until a stat request has a consumer).
- `O_CLOEXEC`, `fcntl`, `dup`, pipes, a cwd or relative paths.
- File-backed, shared or fixed-address `mmap`; `mprotect`/`PROT_NONE`; `madvise`; `mremap`.
- Threads, futexes, `VM_EXIT` teardown.
- `fork` duplicating the parent's descriptors (POSIX requires it; `fd.rs` already records the debt,
  and C has no `fork` arm yet).
- The cleanups §4 lists — this chunk takes the cheap fix in each case, by the user's ruling.
- Any C program other than `fileio`; changing `hello`.
- Closing the two Phase 5 write-back orderings (see M13 — `lseek` cannot reach them).

### Anti-goals — would be failure even if the markers pass

- **Faking a file size in VFS** (caching the `FS_LOOKUP` size at open) to make `SEEK_END` work. It
  goes stale on the first write through any other descriptor.
- **Faking `brk(0)` in the shim** rather than asking VM. The marker would then prove the shim, not
  the heap origin chunk 2 built.
- **A marker that prints an address or a size.** Image end differs between the SDK and musl
  flavours; a marker carrying it passes in one and fails in the other (the 5.5/5.6 trap). Every new
  marker is a self-checked verdict.
- **Encoding `lseek` inside an existing request** (a magic `VFS_READ` length, say) to dodge the D8
  bump. The bump is the honest cost.

---

## 2. What exists, and what the chunk changes

| Component                                    | Today                                                    | After this chunk                                                                                      |
| -------------------------------------------- | -------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| fork `src/minixrs/_syscall.c`                | 6 Linux numbers; rest `-ENOSYS`                          | `+ openat, read, readv, close, lseek, brk, mmap, munmap`                                              |
| `kernel-shared/src/callnr.rs`                | VFS `0x800..=0x804`, `NR_VFS_MSGS = 5`                   | `+ VFS_LSEEK = 0x805`, `NR_VFS_MSGS = 6`, `+ VFS_WHENCE_OFF`, `VFS_OFFSET_OFF`, `VFS_POS_OFF`         |
| `kernel-shared/src/callnr.rs` `VM_BRK` doc   | `new_break < origin` is `EINVAL`; doc names `HEAP_BASE`  | `new_break == 0` is a query; doc names the per-proc heap origin                                       |
| `kernel-shared/src/fcntl.rs`                 | `O_*` only                                               | `+ SEEK_SET/SEEK_CUR/SEEK_END` (Linux values, pinned by `const _`, not emitted — the `O_*` precedent) |
| `tools/gen-c-headers/src/callnr_h.rs`        | VFS band rows `0..=4`                                    | `+ VFS_LSEEK` row and the three offsets                                                               |
| `servers/vfs/src/{main,fd}.rs`               | no seek; `pos` advanced only by reads/writes             | `+ do_lseek`; `fd.rs` gains a set-position helper                                                     |
| `servers/vm/src/{main,region}.rs`            | no sender checks; unknown requests dropped               | M7–M9: PM-only `VM_EXEC`/`VM_FORK`, `ENOSYS` for unknown, `brk(0)` query, exact break kept            |
| `servers/vm/src/main.rs` fault path          | a failed `VMCTL_PT_MAP` is ignored and the fault cleared | the faulter is killed instead (M17)                                                                   |
| `kernel-shared/src/callnr.rs` VFS band       | —                                                        | `+ VFS_PM_EXIT = 0x806`, `NR_VFS_MSGS = 7` (M16)                                                      |
| `servers/pm/src/main.rs`                     | VFS is told nothing about exit                           | sends `VFS_PM_EXIT` on every exit path (M16)                                                          |
| `kernel/src/proc/table.rs`                   | `USER_IPC_TO = [PM, VFS]`                                | `[PM, VFS, VM]`                                                                                       |
| `userland/fileio/`                           | —                                                        | new: `fileio.c` (+ linker script per the `hello` pattern)                                             |
| `kernel/build.rs`                            | builds one C program (`hello`) in three flavours         | builds both C programs under one flavour decision; packs `/bin/fileio`                                |
| `kernel-shared/src/rootfs.rs`                | runtime-zone budget for init's probes                    | `+ ROOTFS_FILEIO_PATH`, `+` fileio's seek-hole file; `ROOTFS_RUNTIME_ZONES` grows by its zones        |
| `userland/init/src/main.rs`                  | `EXEC_TARGETS = [worker, /bin/hello, bigprog]`           | `+ /bin/fileio`                                                                                       |
| `tests/qemu-boot.{expected,forbidden}`       | —                                                        | `+` fileio markers (M11) and their `FAIL` forms                                                       |
| `external/musl` submodule                    | `f1db256f` (slice 5.6a)                                  | the new fork commit — **same PR** (D8)                                                                |
| tooling repo                                 | SDK sysroot built from the 5.6a fork commit              | hand-off: rebuild the SDK sysroot from the new fork commit (M12)                                      |
| `book/src/libc/`                             | documents the six-call shim                              | documents the new surface, derived from source                                                        |
| `docs/plans/phase-6-prep.md`, `docs/plan.md` | chunk 5 unchecked                                        | checked in this PR; "Carried from Phase 5" corrected per M13                                          |

---

## 3. Decisions

### M1 — the shim's mapping table

Every new arm is a translation onto a request that already exists, except `lseek` (M2):

| Linux syscall | Request        | Notes                                                                                           |
| ------------- | -------------- | ----------------------------------------------------------------------------------------------- |
| `openat`      | `VFS_OPEN`     | M5                                                                                              |
| `read`        | `VFS_READ`     | Payload is `VFS_WRITE`'s; clamp `len` to `INT32_MAX` exactly as `minixrs_write` does            |
| `readv`       | `VFS_READ` × n | **Required, not optional:** musl's `__stdio_read` issues `readv` whenever the FILE has a buffer |
| `close`       | `VFS_CLOSE`    |                                                                                                 |
| `lseek`       | `VFS_LSEEK`    | M2                                                                                              |
| `brk`         | `VM_BRK`       | M3                                                                                              |
| `mmap`        | `VM_MMAP`      | M4                                                                                              |
| `munmap`      | `VM_MUNMAP`    | M4                                                                                              |

`readv` loops one `VFS_READ` per non-empty iovec and **stops at the first short read**, including a
zero (end of file). A short read is legal and VFS does not hide it (`VFS_READ`'s contract); carrying
on would read past a boundary the caller has not seen yet. An error after progress returns the
progress, the rule `minixrs_writev` already follows. Argument validation (`iovcnt` range, NULL
vector) mirrors `minixrs_writev`.

Everything else stays `-ENOSYS`. In particular `mprotect`, `madvise` and `mremap` stay `-ENOSYS`;
the builder **verifies against the v1.2.6 source** that mallocng tolerates each (it is believed to:
`mprotect` failure with `ENOSYS` is accepted, `madvise` is advisory, `mremap` failure falls back to
allocate-copy-free) and records the result in the shim's header comment, as 5.6a did for the startup
path.

### M2 — `VFS_LSEEK = VFS_RQ_BASE + 5` (`0x805`)

- **Request payload:** fd at `VFS_FD_OFF` (`0`, i32 — the existing constant), whence at
  `VFS_WHENCE_OFF` (`4`, i32), offset at `VFS_OFFSET_OFF` (`8`, i64, 8-aligned for the reason
  `VFS_BUF_OFF` gives).
- **Reply:** `m_type = OK` and the new position at `VFS_POS_OFF` (`0`, u64). The position does not
  ride in `m_type` because it can exceed `i32` — the asymmetry with `VFS_READ`'s count is
  deliberate. Errors in `m_type`, position unchanged:
  - `EBADF` — descriptor not open;
  - `ESPIPE` — descriptor is not a file (the console and every `/dev` node);
  - `EINVAL` — whence is `SEEK_END` (**user's ruling**), or anything other than `SEEK_SET`/
    `SEEK_CUR`/`SEEK_END`, or the result would be negative or overflow `i64`.
- **Seeking past end of file is legal.** VFS knows no size, so it could not refuse one if it wanted
  to; a later read there returns 0 and a later write leaves a hole. That newly reachable write path
  is what M11's `seek-hole` probe exists to prove.
- `SEEK_*` values are Linux's (0/1/2) and live in `kernel-shared/src/fcntl.rs`, not emitted in C
  headers — musl defines them — and pinned by `const _`.
- `do_lseek` reuses `fd.rs`'s resolve logic and needs only a set-position helper beside
  `advance_in`; host tests cover each whence, the overflow arm, and `ESPIPE` on a console row.
- The usual `const _` ordering/overlap/alignment guards, `NR_VFS_MSGS` 5→6, and the `callnr_h.rs`
  row. Run `cargo test -p minixrs-gen-c-headers`; its member-count test is what catches a missing
  row (see [`abi.md`](../../conventions/abi.md)).

### M3 — `brk` has Linux semantics, and VM answers `brk(0)`

mallocng opens with `brk(0)` to learn the current break and then asks for `brk + 2 pages`; it treats
any reply other than exactly the value it asked for as failure. Linux's `brk` returns the new break
on success and the **unchanged current break** on failure — never an errno.

- **VM change (my call):** `VM_BRK` with `new_break == 0` replies `OK` with the current break: the
  exact break last set, or the heap origin if the process has never set one. No new number, no new
  field — a reply-semantics change to an existing request, documented on `VM_BRK` and packaged in
  the same D8 bump because the fork depends on it.
- **VM change (my call):** VM records the **exact** requested break and replies with it; the heap
  region's `end` stays page-aligned. Today `set_brk` replies with the page-aligned end, which would
  make any unaligned request look like a failure to musl. Stub D only ever passes page-aligned
  breaks (`0x0100_4000`, `0x0100_8000`), so its behaviour is unchanged — the builder confirms this
  against `user_stub.S` and stub D's markers.
- **Shim:** `brk(0)` → query. `brk(n)` → `VM_BRK(n)`; on a negative reply, query and return the
  current break, which is what Linux returns.
- `VM_BRK`'s doc comment still says `[HEAP_BASE, new_break)`; correct it to the per-proc origin
  `VM_EXEC` records.

### M4 — `mmap`/`munmap` accept only what VM can honour

VM's `VM_MMAP` is anonymous, VM-placed and read-write. The shim forwards a call **only** when it is
`MAP_PRIVATE | MAP_ANONYMOUS` (other flag bits the builder judges inert, such as `MAP_NORESERVE`,
may be accepted and named), `fd == -1`, `off == 0`, and not `MAP_FIXED`. An `addr` hint without
`MAP_FIXED` is ignored, as POSIX allows. Anything else is `-ENOSYS` (my call — "not implemented" is
the honest answer, and mallocng's one `MAP_FIXED` call, a `PROT_NONE` guard page, ignores the
result). `prot` is ignored: every region is read-write, so mallocng's guard pages are **not**
guards. That is recorded in the shim comment, not hidden.

`munmap` → `VM_MUNMAP`, whose match rule is "a region starting at `addr`". The builder confirms
against mallocng's source that it only ever unmaps whole mappings it created; if it ever unmaps a
sub-range, that is a finding to bring back, not to paper over.

Region budget: each `mmap` costs one of VM's 16 per-process region slots. `fileio` needs a handful;
note the ceiling in the shim comment.

### M5 — `openat`

- `dirfd` is ignored for an absolute path (POSIX's own rule). A relative path is passed through and
  VFS answers `EINVAL`, as it already does — there is no cwd.
- **Strip `O_LARGEFILE` in the shim.** musl ORs it (`0400000` on aarch64) into *every* open via
  `__sys_open`, and VFS's `O_KNOWN` would refuse it with `EINVAL`. It means nothing on LP64. My call
  to strip it in the shim rather than widen `O_KNOWN`: it is a libc artefact, and keeping it out of
  `kernel-shared` keeps the change out of the ABI.
- Every other bit passes through unchanged, so `O_CLOEXEC` still answers `EINVAL` — honest, and a
  non-goal.
- The path's length is computed in the shim and bounded (a NUL-less buffer must not be walked off
  the end); VFS remains the authority on `ENAMETOOLONG`.
- `mode` is ignored (no permission model).

### M6 — USER's `ipc_to` gains VM

`USER_IPC_TO = [PM_PROC_NR, VFS_PROC_NR, VM_PROC_NR]`. The existing loop already opens each reverse
server→USER bit, so the 5.4 "pair of bits" lesson is satisfied by the array edit; the builder
confirms VM's reply reaches a user process with a boot, not by reading. `init` shares the USER priv,
so it gains the edge too. Comments saying USER cannot reach VM go stale and must be corrected:
`table.rs`, VM's `handle_exec` doc, and the `[diag vm] exec` block in `tests/qemu-boot.expected`.

### M7 — VM authenticates the requests that name a third party

Once users reach VM, any request whose **target comes from the payload** becomes a confused-deputy
hole. `VM_EXEC` (target endpoint + heap origin) and `VM_FORK` (parent + child) both do: an unchecked
user could reset another process's regions or clone one over another. **VM accepts them only when
`m_source` is PM's endpoint**, and replies `EPERM` otherwise (MINIX's errno for the case; present in
the POSIX block).

`VM_PAGEFAULT` is **not** gated (my call). A forged one is indistinguishable by `m_source` — the
kernel stamps the faulter's own endpoint on real ones — but its target *is* that `m_source`, so the
worst a forger can do is map a page of its own region, SIGSEGV itself, or block itself forever
waiting for a reply VM never sends. The builder verifies `VMCTL_CLEAR_PAGEFAULT`'s `rts_unset` on a
process that is not `RTS_PAGEFAULT`-blocked is a no-op for scheduling (it must not make a
`SENDREC`-blocked forger runnable), and writes the reasoning into VM's dispatch comment. A real gate
needs a kernel "from kernel" IPC flag — MINIX's `IPC_FLG_MSG_FROM_KERNEL` — and is out of scope.

`VM_BRK`/`VM_MMAP`/`VM_MUNMAP` act on `m_source` only, so they need no gate.

### M8 — VM answers unknown requests

VM's unknown-request arm drops the message, which blocks a `SENDREC`ing user forever. Reply
`ENOSYS`, mirroring VFS's arm. Only reply to what can be a `SENDREC` — notifications must still be
ignored, which `sef.receive` already filters; the builder confirms that.

### M9 — denial probes for M7/M8

The PM-only gate is a guard, and per
[`testing-and-markers.md`](../../conventions/testing-and-markers.md#mutation-testing) a guard's
mutation moves the marker of the probe that exists for it. So `fileio` (a plain user process) sends
`VM_FORK` and `VM_EXEC` naming a target and expects `EPERM`, and an out-of-band `m_type` and expects
`ENOSYS`. These go in `fileio` rather than `init`'s denial battery so the probe's sender is
unambiguously an ordinary exec'd C process; the builder may move them to `init` if that proves
cleaner, with a reason. Requires the generated `<minixrs/*.h>` headers and the fork's
`__minixrs_ipc` in the C program; if exposing those to a userland program is awkward, `init` is the
fallback.

### M10 — one program, `/bin/fileio`, raw section first

One new C program rather than one per proof (my call). Two musl-flavour static binaries at ~200 KB
each would cost ~100 of the image's free zones; one costs ~50 and one `EXEC_TARGETS` slot. The two
halves stay independently diagnosable:

1. **Raw section** — `open`/`read`/`write`/`lseek`/`close`, reporting through **`write(2)`
   directly**, so its markers are on the console before stdio or malloc runs at all. A crash in the
   stdio half cannot swallow them.
2. **Stdio section** — `fopen`/`fgets`/`printf`/`fclose`, then the malloc probes.

Built exactly like `hello` in all three flavours: one flavour decision per build applied to both C
programs; under `Worker`, `worker` is packed under `/bin/fileio` too, and its markers go missing the
way `hello`'s do. `fileio` must fit `VFS_EXEC_MAX` (256 KiB) and the image must keep
`ROOTFS_RUNTIME_ZONES` free — `kernel/build.rs` asserts both. If either trips, raise the one
constant that bounds it (`VFS_EXEC_MAX` is VFS `.bss`; `ROOTFS_IMAGE_BLOCKS` moves every zone number
and the layout tests with it) and say so in the PR.

### M11 — the proof

All lines prefixed `minix.rs fileio: `, each printed only when its check passed, each with a `FAIL`
form in `tests/qemu-boot.forbidden`. Plus `name=fileio src=grant entry=` for the exec form, matching
`hello`'s. Expected-file comments explain each line in the existing house style.

| Marker                 | What it proves                                                                                                                                                                                                                                                                                                                                                                                  |
| ---------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `read <motd contents>` | `openat`+`read` of `/etc/motd`, written to fd 1. The prefix is required: init already prints `ROOTFS_MOTD` verbatim, and markers match first occurrences, so a bare motd line would be satisfied by init                                                                                                                                                                                        |
| `lseek ok`             | `SEEK_CUR` returns the position the read left; `SEEK_SET 0` + re-read returns identical bytes; a seek past EOF then read returns 0                                                                                                                                                                                                                                                              |
| `lseek deny ok`        | `SEEK_END` → `EINVAL`; bad whence → `EINVAL`; negative result → `EINVAL`; fd 1 → `ESPIPE`; closed fd → `EBADF` — all via `errno`, end to end                                                                                                                                                                                                                                                    |
| `seek-hole ok`         | `O_CREAT\|O_TRUNC` a scratch file, seek past one block, write, seek 0, read: the leading block is zeros and the tail is the data. First C — and first anything — to write past EOF through `lseek`; also proves `O_CREAT`/`O_TRUNC` pass the shim untouched. Re-runs every `fileio` exec, so it truncates first                                                                                 |
| `stdio ok`             | `fopen("/etc/motd","r")` + `fgets` returns the motd line, `fclose` returns 0. `fclose` on a partly consumed stream seeks back (`SEEK_CUR`, negative) — stdio reaches `lseek` without being asked to                                                                                                                                                                                             |
| `fresh-buf ok`         | `read()` into a freshly `mmap`ed page the program has never touched returns the file's bytes. The mutation is removing M15's pre-fault: the read then answers `EFAULT`. Without this row, M15 is a workaround no marker depends on                                                                                                                                                              |
| `brk ok`               | Taken as the **first statement of `main`**, before `fopen` or anything else can malloc: `sbrk(0)` equals the image end rounded up to a page (from the linker's `_end`) — the heap origin chunk 2 built, now observed from C. After the first malloc, `sbrk(0)` has advanced — mallocng took the `brk` path, not its `mmap` fallback. The builder confirms `_end` exists in both flavours' links |
| `mmap ok`              | `mmap` an anonymous multi-page region, fill and verify it, `munmap` returns 0, and a second `munmap` of the same base returns `EINVAL` — proving the first one reached VM                                                                                                                                                                                                                       |
| `malloc ok`            | A small and a large (above mallocng's mmap threshold) allocation, written and read back, then freed                                                                                                                                                                                                                                                                                             |
| `vm deny ok`           | M9's probes                                                                                                                                                                                                                                                                                                                                                                                     |
| VFS exit-reset diag    | M16's proof: `fileio` exits with one descriptor deliberately left open, and VFS reports a reset that closed exactly one. Not `fileio:`-prefixed, because VFS prints it                                                                                                                                                                                                                          |

The seek-hole file's zones join `ROOTFS_RUNTIME_ZONES`, computed from constants, never copied from a
plan. Each marker's mutation (e.g. shim ignoring `whence`, `readv` continuing past a short read, VM
replying the aligned break, dropping the PM gate) is listed in the plan and run per the mutation
discipline; any marker with no mutation that moves it is recorded as such.

Also verify, and record in the shim comment as 5.6a did: `fileio` has **no `PT_TLS`** (so
`__init_tls` still uses `builtin_tls` and never mmaps), and nothing in startup now reaches a newly
live syscall (startup must still issue exactly one, `set_tid_address`).

### M12 — the D8 bump and the cross-repo legs

- One PR in this repo carries the `kernel-shared` change, the `callnr_h.rs` row, and the
  `external/musl` submodule bump to the new fork commit. The fork commit sits on top of the
  `minixrs` branch (no rebase is needed, so no force-push); it must be reachable on the fork's
  remote before this PR merges or the submodule pins an orphan.
- **Tooling hand-off:** the SDK sysroot is built from the fork, so three-boot-matrix row 1 needs it
  rebuilt from the new fork commit. Write the hand-off as a plan file beside chunk 2's
  `2026-09-19-user-va-map-tooling.md`. Rows 2 and 3 run in this repo; row 1 runs after the hand-off,
  and the PR says which rows were run.

### M13 — `lseek` does not close the Phase 5 orderings

The two orderings 5.10b left unproven — `do_trunc`'s write-back-then-free and `create`'s
inode-before-dirent — need a failure *between* two steps, and no seek induces one. Correct
`phase-6-prep.md`'s "Carried from Phase 5" section to say so rather than leaving the next reader to
expect `lseek` to probe them.

### M14 — documentation

The `book/src/libc/` chapter gains the new surface, derived from the shim source; `abi.md`'s
Phase-4/5 pinned-number list gains `VFS_LSEEK` and `VFS_PM_EXIT`; chunk 5's box is checked in both
trackers in this PR, and §4's items are added to `phase-6-prep.md`'s "Deferred" table so the cleanup
has a home.

### M15 — the shim pre-faults every buffer it hands to VFS (user's ruling)

VM backs heap and mmap pages lazily, on the owner's first touch. But VFS reaches a caller's buffer
through the kernel copy engine, and `copy_between_as` (`kernel/src/mm/uaccess.rs`) answers `EFAULT`
for any unmapped page rather than asking VM to map it. So a `read()` into memory malloc just handed
out fails or succeeds by page-layout luck — mallocng does not touch fresh mmap memory, `calloc`
included, because it is already zero.

The shim therefore does **one volatile read per page** of each buffer before the request that names
it: `read`, each `readv`/`writev` iovec, and `write`. A read fault is enough — VM maps every fault
read-write — and a read leaves the contents alone, so the same helper serves both directions. The
path buffer `openat` passes needs nothing: computing its length already reads it.

This is a **workaround for a kernel gap, in libc**, and the shim comment says so and names §4. Rust
callers are unaffected because they use `.bss` and the stack, which are mapped eagerly.

### M16 — `VFS_PM_EXIT`: PM tells VFS a process is gone (user's ruling)

`fd.rs` records that nothing resets a descriptor row when a process exits, while proc numbers are
recycled every init cycle — and names "a program that opens a file *after* being exec'd" as the case
that will force it. `fileio` is that case, and a `fileio` that dies mid-run (every mutation run that
crashes it) would leak open descriptors to whatever lands in its slot next.

- `VFS_PM_EXIT = VFS_RQ_BASE + 6` (`0x806`), `NR_VFS_MSGS` 6→7, in the same D8 bump as M2. Payload:
  the exiting endpoint at offset `0` (i32). Reply `OK`. Not used by C, but the `callnr_h.rs` row is
  still required by the band's member-count test.
- VFS accepts it **only from PM**, by the same kernel-stamped `m_source` check `VFS_EXEC_STAGE`
  uses, and resets that proc's row to the state a fresh process starts with (0/1/2 on the console,
  everything else closed).
- PM sends it on **every** path that ends a process — `PM_EXIT` *and* the signal/kill path, since
  the crash case is the reason this exists — after the process can issue no further request and
  before PM can hand its proc number to a `fork`. The builder verifies PM→VFS `SENDREC` cannot
  deadlock against anything VFS sends PM.
- **Proof:** `fileio` leaves exactly one descriptor open on purpose when it exits, and VFS emits a
  diag line only when a reset closed something (so the ordinary exits every cycle stay silent),
  naming the count but not the proc number. The mutation is PM not sending the request. The kill
  path has no cheap probe; if none is found, record it as unproven rather than omit it.
- Rewrite `fd.rs`'s "What this table still does not know about" note: exit is handled; `fork` not
  duplicating descriptors is still open.

### M17 — a failed page map kills the faulter (user's ruling)

`handle_pagefault` ignores `VMCTL_PT_MAP`'s result and clears the fault regardless. When frames run
out, the process re-faults forever and single-threaded VM serves nothing else. With users now able
to `mmap`/`brk` freely that is one loop away. On a `PT_MAP` failure VM instead raises `SIGSEGV`
toward PM exactly as its out-of-region arm does, and leaves the fault uncleared. No ABI change. No
boot probe is practical — exhausting 256 MiB under TCG would blow the boot budget — so record it as
unproven by marker, and host-test any helper the decision is factored into.

### M18 — the kernel saves and restores `TPIDR_EL0` per process (found during execution)

Added after the design was approved, when Task 7's default-features boot failed. The kernel's
`ArchRegisterFrame` has no slot for `TPIDR_EL0`, and neither the exception entry stubs nor
`el1_return_to_user` touch it, while musl's `__set_thread_area` writes it directly. So every C
process shares one thread pointer and the last to start wins. Stub-free boots ran one C program at a
time and hid this; with the boot stubs on, `hello` and `fileio` overlap, and `fileio`'s next `errno`
write lands in `hello`'s TLS address — unmapped in `fileio`'s address space — and it takes
`SIGSEGV`. **A pre-existing kernel bug, not a chunk-5 regression**, but chunk 5 is the first boot
that makes two C programs overlap, so it lands here: without it the chunk is red on `qemu-smoke`.

The frame gains `tpidr_el0`; both entry stubs save it and the return path restores it. `do_exec`
already resets the frame to `EMPTY` (thread pointer 0) and `do_fork` already copies the whole frame
(the child inherits it), so neither changes. Proof: the default-features fileio markers, plus a
mutation that drops the restore and brings back the `SIGSEGV` — scheduler-dependent, so the commit
says so.

### M19 — the kernel saves and restores FP/SIMD state per process (found during execution)

Added when Task 8's stub-free boot failed. The saved frame carries no `v0`–`v31`, `FPCR` or `FPSR`,
so any EL0 process's vector registers are whatever the last runner left. `fileio`'s clang-vectorised
fill loop keeps loop-carried values in `v0`–`v4` across the page fault its first store takes; VM —
Rust built with `+neon`, whose `Message` copies use `q` registers — runs in between, and every byte
past the first 32 comes back wrong (`mmap FAIL verify rc=32`). Like M18, **a pre-existing kernel
bug** that every EL0 process has (the Rust servers included) and that chunk 5 is merely the first to
observe.

**Eager save/restore** (my call): both entry stubs store the 32 `q` registers plus `FPCR`/`FPSR`
into the frame before any kernel Rust runs, and `el1_return_to_user` reloads them. Lazy switching
(trapping first FP use through `CPACR_EL1`) is the cheaper design and is deferred (§4); eager is the
one that is obviously correct and is what makes this chunk green. Cost: ~528 more bytes moved per
trap, which the boot-budget measurement in Task 8 must see. `do_exec` resets the frame and `do_fork`
copies it, so a fresh image starts with zeroed FP state and a child inherits its parent's. Proof:
`fileio`'s `mmap ok` / `malloc ok` stub-free — deterministic, since the fault is self-inflicted —
plus a mutation dropping the restore.

---

## 4. Deferred cleanup — a later slice owes these

The user's ruling was to take the cheap fix now and clean up later. Each item below goes into
`phase-6-prep.md`'s "Deferred" table in this PR:

| Cheap fix here                            | Proper fix later                                                                                                                    |
| ----------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| M15: shim pre-faults buffers              | The kernel copy engine asks VM to map a missing page and retries (MINIX's VM-request path); then remove the pre-fault from the shim |
| M17: kill the faulter on `PT_MAP` failure | A per-process memory cap in VM, so one process cannot exhaust guest memory for everyone                                             |
| M7: `VM_PAGEFAULT` left ungated           | A kernel "from kernel" IPC status flag (MINIX's `IPC_FLG_MSG_FROM_KERNEL`) so VM can refuse a forged fault                          |
| M4: `prot` ignored, `MAP_FIXED` refused   | `PROT_NONE`/`mprotect`, so mallocng's guard pages guard                                                                             |
| `SEEK_END` → `EINVAL`                     | An FS-band size/stat request, with `fstat` as its consumer                                                                          |
| M19: eager FP/SIMD save on every trap     | Lazy FP switching (trap first FP use via `CPACR_EL1`), so a process that never touches FP pays nothing                              |
| M16: rows reset on exit                   | `fork` duplicates the parent's descriptors (POSIX)                                                                                  |

---

## 5. Verification

- Host: `cargo test` across the workspace (VFS `fd.rs` lseek and row-reset tests, VM `region.rs`
  brk-query and exact-break tests, `gen-c-headers`), `clippy`, `dprint check`,
  `tools/check-md-links.py`.
- Boot: `tools/check-boot-log.sh` PASS in the default and `--no-default-features` configs, musl
  flavour; three-boot matrix rows 2 and 3 (row 1 after M12's hand-off).
- **Boot budget:** fileio adds an exec of a ~200 KB image plus its reads to every third-or-fourth
  init cycle. Measure the last required marker's position as a fraction of a fixed-timeout log
  against the merge base, per [`ci.md`](../../conventions/ci.md), before deciding whether 600 s
  still holds.
- Mutation pass per M11, run stub-free.

## 6. Left to the builder

Exact marker wording beyond the table; the seek-hole file's name and offsets; the C program's
internal structure; whether M9's probes live in `fileio` or `init`; which inert `mmap` flag bits to
accept; the task order in the plan (a natural order is ABI + VFS → VM → kernel edge → shim → fileio
→ markers → docs, each ending at a host-testable or boot-testable checkpoint).
