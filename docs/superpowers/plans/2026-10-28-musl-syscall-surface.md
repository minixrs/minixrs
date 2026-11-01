# musl Syscall Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a C program open, read, seek and close a MinixFS file and use malloc, proved by
`/bin/fileio`'s self-checking boot markers.

**Architecture:** Most requests already exist. VFS gains `VFS_LSEEK` and a PM-only `VFS_PM_EXIT`; VM
learns `brk(0)`, keeps the exact break, authenticates PM-only requests and stops wedging on a failed
page map; the kernel opens USER→VM. The musl fork's `_syscall.c` maps eight more Linux call numbers
onto those requests, pre-faulting every buffer it hands VFS. One new C program, built by
`kernel/build.rs` exactly like `hello`, carries the proof.

**Tech Stack:** Rust (`no_std` kernel + EL0 servers), C (musl v1.2.6 fork, `external/musl`),
aarch64, QEMU, `dprint`.

**Spec:**
[`../specs/2026-10-28-musl-syscall-surface-design.md`](../specs/2026-10-28-musl-syscall-surface-design.md)
— decisions `M1…M19`. Read it before Task 1; every task argues from it. Where this plan and the spec
disagree, **the spec wins** and the plan gets corrected.

## Global Constraints

- **SPDX header** on every new `.rs`/`.S`/`.c` file before any other content — see
  [`rust-style.md`](../../conventions/rust-style.md); `userland/hello/hello.c` shows the C form.
- **Every commit `--signoff`ed and GPG-signed.** Never `--no-gpg-sign`, never `--no-verify`.
- **Branch `feature/musl-syscall-surface`** (exists; the spec is its first commit). Never commit to
  `main`.
- **Do not push, open a PR, or trigger CI** — in either repo. Stop at the push and surface both
  branches.
- **D8:** every ABI change in this plan (`VFS_LSEEK`, `VFS_PM_EXIT`, `VM_BRK(0)`) and the
  `external/musl` gitlink bump land in **one PR**.
- **Markdown wraps at 100 columns**; `~/.dprint/bin/dprint fmt` + `check` and `python3
  tools/check-md-links.py` on anything edited.
- **The kernel is never host-built**; QEMU is its verification path. Iterate and mutation-test with
  `timeout 60 cargo run -p minixrs-kernel --target aarch64-unknown-none --release
  --no-default-features > log`; final verification with `timeout 300 … --release` (default features)
  and `tools/check-boot-log.sh log`.
- **Musl flavour by default:** run boots with `MINIXRS_SDK=/nonexistent` — it is what CI builds.
- **Mutation discipline** per
  [`testing-and-markers.md`](../../conventions/testing-and-markers.md#mutation-testing): snapshot to
  the scratchpad before mutating (including files this plan *adds*), restore from the snapshot,
  `diff -q` back, finish with `grep -rn MUTATION` clean; check `grep -a 'error\[E' log` before
  recording any observation. Record each mutation and what moved in the task's commit message.
- **Never copy a count into a marker from this plan** — recompute from the constant.

## Review Focus

1. **A bad buffer pointer now kills the C process** rather than returning `EFAULT`, because M15's
   pre-fault touches it first. Expected: documented as a deviation in the shim comment and spec §4;
   VFS's own `EFAULT` still answers for a non-writable destination (pre-fault reads, the copy checks
   write permission). Task 6 adds the comment; Task 7's `read` probe of `.rodata` pins the second
   half.
2. **Zero-length `read`/`write`/iovec** must not pre-fault anything, so `read(fd, NULL, 0)` returns
   0. Task 7 asserts it.
3. **`readv` across a short read mid-file** (not at EOF): the count stops at the short read and
   later iovecs are untouched. Task 7's `readv ok` probe pins it against `/etc/pattern`.
4. **`brk` below the heap origin** returns the unchanged current break, never an errno or a moved
   break. Task 8's `brk ok` asserts it via `syscall(SYS_brk, 4096)`.
5. **A process killed by a signal** gets its descriptor row reset just like one that called `exit`.
   No boot marker reaches it (Task 3 says so); Task 3's code puts both exit paths through one helper
   so the reviewer can see it, and the commit records it as unproven by marker.

---

### Task 1: ABI — `VFS_LSEEK`, `VFS_PM_EXIT`, `SEEK_*`, `VM_BRK(0)` doc

**Files:**

- Modify: `kernel-shared/src/callnr.rs` (VFS block ~`:548–720`; `VM_BRK` doc ~`:1237`)
- Modify: `kernel-shared/src/fcntl.rs`
- Modify: `tools/gen-c-headers/src/callnr_h.rs` (VFS band `:93–101`; VFS offsets `:358–360`; the
  offsets test `:545–554`)

**Interfaces:**

- Produces: `VFS_LSEEK: i32 = VFS_RQ_BASE + 5`, `VFS_PM_EXIT: i32 = VFS_RQ_BASE + 6`, `NR_VFS_MSGS =
  7`; `VFS_WHENCE_OFF: usize = 4`, `VFS_OFFSET_OFF: usize = 8` (request), `VFS_POS_OFF: usize = 0`
  (reply); `VFS_PM_EXIT_PROC_OFF: usize = 0`; `fcntl::{SEEK_SET = 0, SEEK_CUR = 1, SEEK_END = 2}:
  i32`. C headers emit `VFS_LSEEK`, `VFS_PM_EXIT`, `VFS_WHENCE_OFF`, `VFS_OFFSET_OFF`,
  `VFS_POS_OFF`.

- [ ] **Step 1: Failing tests.** In `callnr.rs`'s test module: `assert_eq!(VFS_LSEEK, 0x805)`,
      `assert_eq!(VFS_PM_EXIT, 0x806)`, `assert_eq!(NR_VFS_MSGS, 7)`. In `fcntl.rs`: a test that
      `SEEK_SET/CUR/END` are `0/1/2`. In `callnr_h.rs`'s offsets test, add `("VFS_WHENCE_OFF", 4)`,
      `("VFS_OFFSET_OFF", 8)`, `("VFS_POS_OFF", 0)` for a `VFS_LSEEK` request row.
- [ ] **Step 2:** `cargo test -p minixrs-kernel-shared -p minixrs-gen-c-headers` → FAIL (names
      undefined).
- [ ] **Step 3: Implement.** Doc comments state M2's payload, reply and errno list
      (`EBADF`/`ESPIPE`/`EINVAL`, `SEEK_END` → `EINVAL` "until a stat request has a consumer") and
      M16's contract (PM-only, `EPERM` otherwise, resets the row to `DEFAULT_ROW`). `const _` guards
      in the existing style: `VFS_FD_OFF + 4 <= VFS_WHENCE_OFF`, `VFS_WHENCE_OFF + 4 <=
      VFS_OFFSET_OFF`, `VFS_OFFSET_OFF + 8 <= 96`, `(8 + VFS_OFFSET_OFF).is_multiple_of(8)`, `(8 +
      VFS_POS_OFF).is_multiple_of(8)`. `SEEK_*` in `fcntl.rs` pinned by literal `const _`s and
      **not** emitted, per that file's header. Rewrite `VM_BRK`'s doc: origin is the per-proc heap
      origin `VM_EXEC` records; `new_break == 0` replies `OK` with the current break; replies carry
      the **exact** break; a refused request changes nothing.
- [ ] **Step 4:** `cargo test -p minixrs-kernel-shared -p minixrs-gen-c-headers` → PASS (including
      `every_band_member_list_matches_its_count`); `cargo gen-c-headers --stdout | grep -E
      'VFS_(LSEEK|PM_EXIT|WHENCE_OFF|OFFSET_OFF|POS_OFF)'` shows all five.
- [ ] **Step 5: Commit** `abi: VFS_LSEEK, VFS_PM_EXIT, SEEK_*; VM_BRK(0) is a query`.

### Task 2: VFS — `do_lseek`

**Files:**

- Modify: `servers/vfs/src/fd.rs`, `servers/vfs/src/main.rs` (dispatch `:350–366`)

**Interfaces:**

- Consumes: Task 1's constants.
- Produces: `fd::seek_in(rows: &mut [FdRow], proc_nr: i32, fd: i32, whence: i32, offset: i64) ->
  Result<u64, i32>` and live wrapper `fd::seek(proc_nr, fd, whence, offset) -> Result<u64, i32>`.

- [ ] **Step 1: Failing host tests in `fd.rs`**, against a local fixture as the existing tests do:
  - `seek_set_moves_a_file_and_returns_the_position`: `Fd::File{ino:5,pos:0}` at fd 3, `seek_in(…,
    3, SEEK_SET, 31) == Ok(31)`, then `resolve_in` shows `pos: 31`.
  - `seek_cur_is_relative`: from `pos: 10`, `SEEK_CUR, -4` → `Ok(6)`.
  - `seek_end_is_einval`: → `Err(EINVAL)`, position unchanged.
  - `unknown_whence_is_einval`: whence `3` → `Err(EINVAL)`.
  - `negative_result_is_einval`: from `pos: 2`, `SEEK_CUR, -3` → `Err(EINVAL)`, unchanged.
  - `overflow_is_einval_not_a_panic`: from `pos: i64::MAX as u64`, `SEEK_CUR, 1` → `Err(EINVAL)`;
    `SEEK_CUR, i64::MIN` from `pos: 0` → `Err(EINVAL)`.
  - `past_eof_is_legal`: `SEEK_SET, 1 << 40` → `Ok(1 << 40)`.
  - `console_is_espipe`: fd 1 of a default row → `Err(ESPIPE)`; a `CharDriver::Memory` row →
    `Err(ESPIPE)`.
  - `closed_fd_is_ebadf`: fd 4 unused → `Err(EBADF)`.
- [ ] **Step 2:** `cargo test -p minixrs-vfs` → FAIL.
- [ ] **Step 3: Implement `seek_in`** (arithmetic with `checked_add` on `i64`; a position above
      `i64::MAX` is unreachable but treated as overflow) and `seek`. Then `do_lseek(caller_e:
      Endpoint, msg: &mut Message) -> i32` in `main.rs`: reads fd/whence/offset at the Task 1
      offsets, on success writes the position as u64 at `VFS_POS_OFF` and returns `OK`. Dispatch arm
      `VFS_LSEEK => do_lseek(caller_e, &mut msg)`. `reply()` overwrites only `m_type`/`m_source`, so
      the payload survives.
- [ ] **Step 4:** host tests PASS; `cargo clippy --workspace --all-targets -- -D warnings` clean;
      stub-free boot: every marker that passed before still passes (init's `vfs.deny ok n=4`
      `no-such` probe is band-relative, so it now aims at `0x807` and must still answer `ENOSYS`).
- [ ] **Step 5: Commit** `vfs: VFS_LSEEK`.

### Task 3: VFS + PM — `VFS_PM_EXIT`

**Files:**

- Modify: `servers/vfs/src/fd.rs` (module note "What this table still does not know about" `:49–68`;
  new reset), `servers/vfs/src/main.rs`
- Modify: `servers/pm/src/main.rs` (`handle_exit` `:291`, `drain_ksigs` `:676`, dispatch `:168`)

**Interfaces:**

- Consumes: `VFS_PM_EXIT`, `VFS_PM_EXIT_PROC_OFF`.
- Produces: `fd::reset_in(rows: &mut [FdRow], proc_nr: i32) -> usize` (number of descriptors that
  were open beyond `DEFAULT_ROW`, i.e. `Fd::File`s plus any non-default entries closed), live
  `fd::reset(proc_nr) -> usize`; VFS diag line `[diag vfs] pm-exit reset closed=<n>` emitted **only
  when `n > 0`**; PM helper `vfs_pm_exit(vfs: Endpoint, target_e: Endpoint)`.

- [ ] **Step 1: Failing host tests in `fd.rs`:** `reset_restores_the_default_row_and_counts` (a row
      with files at 3 and 5 → `reset_in == 2`, row equals `DEFAULT_ROW`);
      `reset_of_a_default_row_counts_zero`; `reset_of_an_out_of_range_proc_is_zero_and_harmless`
      (proc `-2` and `NR_SERVED_PROCS as i32`).
- [ ] **Step 2:** host tests FAIL.
- [ ] **Step 3: Implement.** `reset_in`/`reset`. `do_pm_exit(caller_e, msg) -> i32` in VFS: `EPERM`
      unless `caller_e == boot_endpoint(PM_PROC_NR)` (the `do_exec_stage` check), else
      `fd::reset(endpoint_proc(target).get())`, diag when non-zero, `OK`. In PM, `vfs_pm_exit` does
      a SENDREC and ignores the result (a diagnostic, like `vm_exec`'s); call it after a
      **successful** `sys_exit` in both `handle_exit` and `drain_ksigs`'s `Terminate` arm — pass
      `vfs` into both (thread it from `main`, as `handle_exec` already receives it). Before wiring,
      confirm no deadlock: VFS's only send to PM outside replies is `grant_test` at VFS startup
      (`main.rs:~1513`), which runs before any user process can exit — write that reasoning into
      `vfs_pm_exit`'s doc comment. Rewrite `fd.rs`'s module note: exit is handled by `VFS_PM_EXIT`;
      `fork` not duplicating descriptors remains open (spec §4).
- [ ] **Step 4:** host tests PASS; clippy clean; stub-free boot: all prior markers pass and **no**
      `pm-exit reset` line appears (nothing leaves a file open yet — the line's absence is the
      expected result here; Task 8 makes it appear).
- [ ] **Step 5: Commit** `vfs, pm: VFS_PM_EXIT resets a dead process's descriptors`. The message
      records the kill path as unproven by marker (Review Focus 5).

### Task 4: VM — exact break, `brk(0)`, PM gate, unknown replies, failed-map kill

**Files:**

- Modify: `servers/vm/src/region.rs` (`ClientRegions` `:155`, `set_brk` `:184`, `exec` `:306`)
- Modify: `servers/vm/src/main.rs` (dispatch `:110–119`, `handle_pagefault` `:153`, `handle_fork`,
  `handle_exec`)

**Interfaces:**

- Produces: `ClientRegions` gains `brk: u64` (exact break; `0` = never set). `set_brk(nr, 0)`
  returns `Ok(current)` where current is `brk` if set else `heap_origin`; a successful `set_brk(nr,
  b)` returns `Ok(b)`, not the aligned end. `exec` and `EMPTY` reset `brk` to 0; `fork` copies it
  (whole-struct copy already does).

- [ ] **Step 1: Failing host tests in `region.rs`:** `brk_zero_before_any_set_returns_the_origin`;
      `brk_zero_after_a_set_returns_the_exact_break` (set `origin + 0x1234` → query returns
      `origin + 0x1234`, `contains(origin + 0x1FFF)` true — region end is aligned);
      `set_brk_replies_the_exact_unaligned_break`; `refused_brk_leaves_the_break_unchanged` (below
      origin → `Err(EINVAL)`, then query returns the prior break); `exec_resets_the_break`. Update
      any existing test that asserted the aligned return value and say why in the commit.
- [ ] **Step 2:** `cargo test -p minixrs-vm` → FAIL.
- [ ] **Step 3: Implement `region.rs`.** Then `main.rs`:
  - `handle_fork`/`handle_exec`: if `msg.m_source != boot_endpoint(PM_PROC_NR)` reply `EPERM` and
    return before reading the payload.
  - Unknown arm: reply `ENOSYS` via `ipc_send` to `m_source` (mirror VFS's arm). Confirm
    `sef.receive` already strips notifications so nothing non-SENDREC reaches it; if it does not,
    reply only when `m_type != NOTIFY_MESSAGE`.
  - `VM_PAGEFAULT` stays ungated. Add the M7 reasoning to the dispatch comment, after verifying in
    `kernel/src/proc` that `rts_unset(p, RTS_PAGEFAULT)` on a proc without that bit does not make a
    `SENDREC`-blocked proc runnable. If it would, stop and report — that changes the spec.
  - `handle_pagefault`: check the `VMCTL_PT_MAP` trap rc **and** reply `m_type`; on failure send the
    same `SYS_KILL`/`SIGSEGV` as the out-of-region arm and return **without** clearing the fault.
    Comment cites M17 and spec §4.
  - Correct `handle_exec`'s doc sentence claiming no user process can reach `VM_BRK`.
- [ ] **Step 4:** host tests PASS; clippy clean; **default-features** boot (stub D lives there):
      stub D's brk/fault markers and `[diag vm] exec nr=` unchanged, `check-boot-log.sh` PASS.
- [ ] **Step 5: Commit** `vm: exact break and brk(0); PM-only EXEC/FORK; reply to unknown requests;
      kill on failed map`.

### Task 5: Kernel — USER reaches VM; init's `vm.deny` probes

**Files:**

- Modify: `kernel/src/proc/table.rs` (`USER_IPC_TO` `:394` and comments `:280`, `:389–404`, `:481`)
- Modify: `userland/init/src/main.rs` (beside `deny_probes` `:365`; module doc)
- Modify: `tests/qemu-boot.expected` (the `[diag vm] exec` comment block `:745–758`; new marker),
  `tests/qemu-boot.forbidden`

**Interfaces:**

- Produces: `USER_IPC_TO: [ProcNr; 3] = [PM_PROC_NR, VFS_PROC_NR, VM_PROC_NR]`; marker `minix.rs
  init: vm.deny ok n=3`, forbidden `vm.deny FAIL`.

The probes live in **init, not `fileio`** — the spec's M9 permits this with a reason, and the reason
is that the fork's IPC trap `__minixrs_ipc` is `hidden` libc-internal API a user program should not
declare. init shares the USER privilege, so the sender is still an ordinary user process; the PR
description states the move.

- [ ] **Step 1: Add the probes first** (they are the failing test). `vm_deny_probes()` in init,
      called beside `deny_probes`, SENDRECs VM by boot endpoint (as init reaches PM/VFS) with:
  - `fork-gate`: `VM_FORK`, parent = child = `NONE` → want `EPERM`;
  - `exec-gate`: `VM_EXEC`, target `NONE`, image_end `0x10_0000` → want `EPERM`;
  - `no-such`: `m_type = VM_RQ_BASE + 0xFF` → want `ENOSYS`.

  **Targets are deliberately invalid**: if a gate is missing VM answers `EINVAL` (distinguishable
  from `EPERM`) and touches no real process. VM has no `NR_VM_MSGS`, so `no-such` uses the top of
  the band; say so in a comment. On mismatch report `vm.deny FAIL <name>` the way `deny_probes`
  does. Add the expected/forbidden lines with house-style comments, and rewrite the stale `[diag vm]
  exec` comment.
- [ ] **Step 2:** stub-free boot → the trap is refused by the kernel (no `ipc_to` bit), so the
      probes report `vm.deny FAIL fork-gate`. Record the observed line.
- [ ] **Step 3: Implement** the `USER_IPC_TO` edit (the existing loop opens the reverse bit) and
      correct the comments that say USER cannot reach VM.
- [ ] **Step 4:** stub-free boot → `minix.rs init: vm.deny ok n=3`. **Mutations:** (a) drop
      `handle_fork`'s gate → `vm.deny FAIL fork-gate`; (b) drop `handle_exec`'s → `FAIL exec-gate`;
      (c) restore VM's drop-unknown arm → init hangs at `no-such` (the marker never appears and
      later init markers vanish) — record what moved for each. Then default-features boot,
      `check-boot-log.sh` PASS; `cargo clippy -p minixrs-kernel --target aarch64-unknown-none -- -D
      warnings` with and without `--no-default-features`.
- [ ] **Step 5: Commit** `kernel: USER reaches VM; init: vm.deny probes`.

### Task 6: musl fork — the shim

Runs **in `external/musl`** (the fork, `minixrs/musl-minixrs`). Create branch
`feature/chunk5-syscall-surface` there off `minixrs`; commits are signed and signed off exactly as
here. If the session owner prefers a separate session for the fork (their cross-repo rule), this
task is self-contained: hand it the spec, this task, and the generated headers.

**Files:**

- Modify: `external/musl/src/minixrs/_syscall.c`
- Modify (main repo): the `external/musl` gitlink

**Interfaces:**

- Consumes: generated `<minixrs/callnr.h>` (`VFS_LSEEK`, `VFS_WHENCE_OFF`, `VFS_OFFSET_OFF`,
  `VFS_POS_OFF`, `VFS_PATH_OFF`, `VFS_PATH_LEN_OFF`, `VFS_FLAGS_OFF`, `VM_BRK`, `VM_MMAP`,
  `VM_MUNMAP`), `<minixrs/com.h>` (`VM_EP`, `VFS_EP`). Check each name exists in `cargo
  gen-c-headers --stdout`; any that is missing is a Task 1 gap — fix it there.
- Produces: `static void prefault(const void *buf, size_t len)` (one volatile byte load per page in
  `[buf, buf+len)`, nothing when `len == 0`); `minixrs_openat`, `minixrs_read`, `minixrs_readv`,
  `minixrs_close`, `minixrs_lseek`, `minixrs_brk`, `minixrs_mmap`, `minixrs_munmap`, all `static
  long`, all returning a value or a negative errno for `syscall_ret`.

- [ ] **Step 1: Re-verify the v1.2.6 facts the spec relies on**, reading the fork source, and write
      each as a header-comment bullet in the 5.6a style: `mprotect` failure with `ENOSYS` is
      tolerated by mallocng; `madvise` failure is ignored; `mremap` failure falls back in `realloc`;
      mallocng only `munmap`s whole mappings it created; `__sys_open` ORs `O_LARGEFILE` (`0400000`);
      `__stdio_read` uses `readv` when the FILE has a buffer; `fflush`/`fclose` on a partly read
      stream seek with `SEEK_CUR`; startup still issues exactly one syscall. **A fact that does not
      hold is a stop-and-report** — the spec depends on it.
- [ ] **Step 2: Implement** the arms per spec M1/M3/M4/M5/M15:
  - `openat(dirfd, path, flags, mode)`: bounded length scan (stop at `FS_PATH_MAX + 1`, pass that
    length and let VFS answer `ENAMETOOLONG`); `flags & ~O_LARGEFILE`; `dirfd`/`mode` ignored.
  - `read`: clamp to `INT32_MAX`, `prefault`, `VFS_READ`. Also add `prefault` to `minixrs_write`.
  - `readv`: validation as `minixrs_writev`; per non-empty iovec `prefault` + read; **stop at the
    first short read**; error after progress returns progress.
  - `close`, `lseek` (position from `VFS_POS_OFF` on `OK`).
  - `brk(n)`: `n == 0` → query; else `VM_BRK(n)`, and on a negative reply return a fresh query.
    Never returns an errno.
  - `mmap`: forward only `MAP_PRIVATE|MAP_ANONYMOUS` (name any inert bits accepted), `fd == -1`,
    `off == 0`, no `MAP_FIXED`; else `-ENOSYS`. `prot` ignored. `munmap` → `VM_MUNMAP`.
  - Header comment: the mapped set; M15 as a kernel-gap workaround naming spec §4 and the
    bad-pointer deviation (Review Focus 1); M4's "guard pages do not guard"; the 16-region ceiling.
- [ ] **Step 3:** `tools/build-musl.sh --force` builds; `cargo clippy` unaffected; stub-free boot
      with the rebuilt sysroot: all five `hello` markers still pass (regression only — nothing calls
      the new arms yet).
- [ ] **Step 4: Commit in the fork** `minixrs: openat/read/readv/close/lseek/brk/mmap/munmap`; then
      in the main repo `git add external/musl` and commit `musl: bump fork for chunk 5 syscall
      surface`. The fork commit is reachable only locally until pushed — note that in the PR
      checklist (D8: it must be on the fork's remote before this PR merges).

### Task 7: `/bin/fileio` — build plumbing and the raw-descriptor proof

**Files:**

- Create: `userland/fileio/fileio.c`, `userland/fileio/fileio.ld` (copy of `hello.ld` unless
  something differs — say what)
- Modify: `kernel/build.rs` (`HelloFlavor` `:110`, hello block `:261–330`, `build_hello*`
  `:532–700`, rootfs manifest `:390–444`)
- Modify: `kernel-shared/src/rootfs.rs` (paths; `ROOTFS_RUNTIME_ZONES` `:315`;
  `ROOTFS_RUNTIME_INODES` `:319`)
- Modify: `userland/init/src/main.rs` (`EXEC_TARGETS` `:174`)
- Modify: `tests/qemu-boot.expected`, `tests/qemu-boot.forbidden`

**Interfaces:**

- Produces: `rootfs::ROOTFS_FILEIO_PATH = "/bin/fileio"`, `rootfs::ROOTFS_SEEKHOLE_PATH =
  "/etc/seekhole"`; `build_c_program(workspace, name: &str) -> Option<(HelloFlavor, Vec<u8>)>` (the
  three `build_hello*` functions parameterised by program name; output under `target/<name>/`);
  `EXEC_TARGETS = ["worker", ROOTFS_HELLO_PATH, "bigprog", ROOTFS_FILEIO_PATH]`.

- [ ] **Step 1: Build plumbing.** Parameterise `build_hello*` by name; build `hello` then `fileio`;
      `assert!` both got the same `HelloFlavor` variant (selection is presence-based, so a mismatch
      is a bug). `Worker` packs `worker` under `/bin/fileio` too. Apply the brand gate and the
      `VFS_EXEC_MAX` assert to `fileio`. Add it to the manifest. `ROOTFS_RUNTIME_INODES += 1` and
      `ROOTFS_RUNTIME_ZONES += 1` (the seek-hole file's one data zone — its first block is a hole),
      updating both doc comments. If the free-zone assert trips, raise `ROOTFS_IMAGE_BLOCKS` and fix
      the layout tests it moves; say so in the commit.
- [ ] **Step 2: Write the raw section of `fileio.c`.** Everything reports via `write(1, …)` directly
      — no stdio before the stdio section. Each check prints `minix.rs fileio: <name> ok` or
      `minix.rs fileio: <name> FAIL <detail>`:
  - `read` — open `/etc/motd`, read into a 64-byte local, write `minix.rs fileio: read ` + the
    bytes. Also asserts `read(fd, NULL, 0) == 0` (Review Focus 2). The motd text lives in C as a
    literal with a comment naming `ROOTFS_MOTD`.
  - `lseek` — `lseek(fd,0,SEEK_CUR)` equals the motd literal's length; `lseek(fd,0,SEEK_SET) == 0`
    then re-read equals the first read; `lseek(fd, 1<<20, SEEK_SET)` then `read == 0`.
  - `lseek deny` — `SEEK_END`, whence 3 and a negative result each `-1/EINVAL`; `lseek(1,…)` →
    `ESPIPE`; a closed fd → `EBADF`. Prints `lseek deny ok n=5`.
  - `readv` — `/etc/pattern`, iov[0] 5000 bytes, iov[1] 100 bytes pre-filled `0xAA`: returns the
    first transfer's count (expected 4096; **measure** what VFS returns, do not assume), iov[0]
    holds `i % 251` for that many bytes, iov[1] still all `0xAA`. Pattern formula commented as
    `rootfs_pattern_byte`'s.
  - `fresh-buf` — `mmap` one anonymous page, never touched, `read` motd into it: bytes match. Also
    `read` into a `.rodata` buffer → `-1/EFAULT` (Review Focus 1).
  - `seek-hole` — `open("/etc/seekhole", O_RDWR|O_CREAT|O_TRUNC)`, `lseek(4096)`, write 8 known
    bytes, `lseek(0)`, read 4104: 4096 zeros then the 8 bytes; close.
- [ ] **Step 3: Markers.** Add `name=fileio src=grant entry=` and each line above to
      `qemu-boot.expected` with house-style comments (the `read` line's comment explains why the
      prefix is required — init prints motd verbatim and matching is first-occurrence); add
      `minix.rs fileio:` + `FAIL` patterns to `qemu-boot.forbidden`.
- [ ] **Step 4: Verify.** Stub-free boot: all new markers present. **Mutations** (shim, in the fork
      tree, then `build-musl.sh --force`): `lseek` ignoring `whence` → `lseek FAIL`; `readv` not
      stopping at a short read → `readv FAIL`; remove `prefault` from `read` → `fresh-buf FAIL`;
      shim not stripping `O_LARGEFILE` → `read FAIL`. Then default-features boot,
      `check-boot-log.sh` PASS, and three-boot-matrix row 3 (`target/musl-sysroot` moved aside): no
      fileio markers, every other marker passes.
- [ ] **Step 5: Commit** (main repo; fork commits for any shim fix go in the fork and re-bump the
      gitlink) `fileio: raw-descriptor proof — read, lseek, readv, fresh-buf, seek-hole`.

### Task 7a: Kernel — save and restore `TPIDR_EL0` per process (spec M18)

Added during execution: Task 7's default-features boot showed concurrent C processes sharing one
thread pointer. Its report (`.superpowers/sdd/2026-10-28-musl-syscall-surface/task-7-report.md`,
"Blocker") holds the diagnosis and the reverted experiment that fixed it.

**Files:**

- Modify: `kernel/src/arch/aarch64/context.rs` (`ArchRegisterFrame`, `EMPTY`, size assert)
- Modify: `kernel/src/arch/aarch64/trap.S`, `kernel/src/arch/aarch64/interrupt.S` (entry saves;
  `el1_return_to_user` restore)

**Interfaces:**

- Produces: `ArchRegisterFrame.tpidr_el0: u64` at a fixed offset named by a constant the assembly
  uses (follow how the existing frame offsets are shared between Rust and `.S`).

- [ ] **Step 1:** Default-features musl-flavour `timeout 300` boot at the current HEAD: record the
      `[pf] … far=0x120514`-style fault and missing fileio markers (the RED).
- [ ] **Step 2: Implement** the field, the two entry-stub saves and the return-path restore. Confirm
      `do_exec` resets and `do_fork` copies it with no change.
- [ ] **Step 3:** Same boot → `check-boot-log.sh` PASS with every fileio marker present and no
      cross-process TLS faults. Stub-free boot unchanged. Kernel clippy both feature sets.
- [ ] **Step 4: Mutation:** drop the restore → the fault and missing markers return. Record it, and
      that the proof depends on the stubs overlapping two C programs.
- [ ] **Step 5: Commit** `kernel: save and restore TPIDR_EL0 per process`.

### Task 8a: Kernel — save and restore FP/SIMD state per process (spec M19)

Added during execution, **before Task 8 finishes**: Task 8's stub-free boot showed vector registers
clobbered across a page fault. Task 8's report
(`.superpowers/sdd/2026-10-28-musl-syscall-surface/task-8-report.md`, "Blocker") holds the diagnosis
and the reverted experiment. Task 8's uncommitted `fileio.c`, `*.ld` and marker-file edits are in
the working tree — **do not stage or modify them**; this task commits kernel files only.

**Files:**

- Modify: `kernel/src/arch/aarch64/context.rs`, `kernel/src/arch/aarch64/trap.S`,
  `kernel/src/arch/aarch64/interrupt.S`

**Interfaces:**

- Produces: `ArchRegisterFrame` gains the FP/SIMD state (`q0`–`q31` as `[u128; 32]` or equivalent,
  `fpcr`, `fpsr`) at named offsets mirrored as `.equ`s, with `offset_of!` guards, the 7a pattern.

- [ ] **Step 1:** Stub-free boot at HEAD with Task 8's working tree: record `mmap FAIL verify rc=32`
      / `malloc FAIL large-verify` (the RED).
- [ ] **Step 2: Implement** the save in both entry stubs before any Rust runs, the restore in
      `el1_return_to_user`. Check: 16-byte alignment of the `q` save area; `CPACR_EL1.FPEN` permits
      EL1 FP access at those points; whether kernel Rust itself uses FP/SIMD (state why the save
      placement covers it either way); `do_exec` resets and `do_fork` copies with no change.
- [ ] **Step 3:** Stub-free boot → `mmap ok` and `malloc ok` present, no `FAIL`. Default-features
      `timeout 300` boot with Task 8's tree → `check-boot-log.sh` PASS. Kernel clippy both feature
      sets.
- [ ] **Step 4: Mutation:** drop the `q` restore → `mmap FAIL verify` returns.
- [ ] **Step 5: Commit** (kernel files only) `kernel: save and restore FP/SIMD state per process`.

### Task 8: `/bin/fileio` — stdio, brk, mmap, malloc, exit reset

**Files:**

- Modify: `userland/fileio/fileio.c`, `tests/qemu-boot.expected`, `tests/qemu-boot.forbidden`

**Interfaces:**

- Consumes: Task 3's `[diag vfs] pm-exit reset closed=<n>`; Task 4's brk semantics.

- [ ] **Step 1: Write the section.** The `brk` origin reading is **the first statement of `main`**,
      before the raw section, kept in a variable and judged here:
  - `brk` — first `sbrk(0)` equals `((uintptr_t)&_end + 4095) & ~4095` (confirm `_end` is defined in
    both the musl-flavour and SDK links with `llvm-readobj --symbols`); `syscall(SYS_brk, 4096)`
    returns that same value unchanged (Review Focus 4); after the first `malloc`, `sbrk(0)` is
    greater; then `syscall(SYS_brk, cur + 0x123)` returns exactly `cur + 0x123` and a write to that
    last byte succeeds. The unaligned request is what tells VM's exact break from its aligned region
    end — mallocng only ever asks for page-aligned breaks.
  - `stdio` — `fopen("/etc/motd","r")`, `fgets` returns the motd line, `fclose == 0`. Printed with
    `printf`.
  - `mmap` — anonymous 4-page `mmap`, fill and verify, `munmap == 0`, second `munmap` of the same
    base `-1/EINVAL`; `mmap` with `MAP_FIXED` → `-1/ENOSYS`.
  - `malloc` — a 64-byte and a 256 KiB allocation (above mallocng's mmap threshold), filled and
    verified, freed.
  - **Exit-reset leak, last:** open `/etc/motd` and exit without closing it.
- [ ] **Step 2: Markers** for `brk ok`, `stdio ok`, `mmap ok`, `malloc ok`, and `[diag vfs] pm-exit
      reset closed=1`, with comments and forbidden forms.
- [ ] **Step 3: Verify.** Stub-free boot: all present. Readelf: `fileio` has **no `PT_TLS`** (record
      in the fork's header comment if not already). **Mutations:** VM replying the aligned break →
      `brk FAIL` (the unaligned probe sees the rounded value); VM `brk(0)` answering `EINVAL` → `brk
      FAIL`; shim `munmap` returning 0 without sending → `mmap FAIL`; PM not sending `VFS_PM_EXIT` →
      the `pm-exit reset` line vanishes; M17's kill-on-failed-map has no practical mutation — record
      it unproven.
- [ ] **Step 4: Boot budget.** Default-features musl-flavour boot with `timeout 600` at this commit
      and at the merge base; compare the last required marker's byte fraction per
      [`ci.md`](../../conventions/ci.md). Raise `qemu-smoke`'s budget only if the ratio says so, in
      this task, with the numbers in the commit.
- [ ] **Step 5: Commit** `fileio: stdio, brk, mmap and malloc proof; exit-reset leak`.

### Task 9: Docs, trackers, tooling hand-off

**Files:**

- Modify: `book/src/libc/overview.md`, `docs/conventions/abi.md` (pinned-number list),
  `docs/plans/phase-6-prep.md` (chunk 5 box; chunk 5 section links the spec and this plan; "Carried
  from Phase 5" corrected per M13; Deferred table gains spec §4's six rows), `docs/plan.md` (chunk 5
  box `:615`)
- Create: `docs/superpowers/plans/2026-10-28-musl-syscall-surface-tooling.md` (modelled on
  `2026-09-19-user-va-map-tooling.md`)

- [ ] **Step 1:** Write the book section from the shim source (mapped set, pre-fault and its
      deviation, brk/mmap limits, what stays `-ENOSYS`), the `abi.md` rows, the tracker edits.
- [ ] **Step 2:** Tooling hand-off: rebuild the SDK sysroot from the new fork commit; then run
      three-boot-matrix row 1 (`$MINIXRS_SDK` set) and expect the same fileio markers. Check whether
      tooling pins a fork revision anywhere and include that edit if so.
- [ ] **Step 3:** `dprint fmt` + `check`, `check-md-links.py`, and the full pre-push gate list in
      [`ci.md`](../../conventions/ci.md) (`cargo fmt --all --check`, workspace clippy, MFS server
      clippy, kernel clippy both feature sets, `cargo test --workspace --exclude minixrs-kernel`).
      Final default-features boot `check-boot-log.sh` PASS; rows 2 and 3 of the three-boot matrix.
      The PR description lists which rows ran.
- [ ] **Step 4: Commit** `docs: chunk 5 — book, abi, trackers, tooling hand-off`.
