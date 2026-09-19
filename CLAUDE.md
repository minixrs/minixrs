# minix.rs

minix.rs — "MINIX 3, in Rust, for the 64-bit era" — is a 64-bit-only reimplementation of MINIX 3 in
Rust, preserving the original ABI. It is a learning OS built around a greenfield Rust microkernel.

## Project Overview

- **Kernel:** Rust (no_std, no_main), greenfield microkernel
- **Architecture:** aarch64 primary (Apple Silicon / QEMU virt), x86_64 secondary
- **Servers:** PM, VFS, VM, RS, DS, SCHED as user-space Rust processes
- **Drivers:** TTY (PL011 console, TX-only — live as of slice 5.3) and VirtIO (MMIO for aarch64, PCI
  for x86_64 — Phase 6) as user-space processes
- **C library:** musl-libc fork with MINIX IPC syscall wrappers
- **License:** BSD-3-Clause only (no GPL code)
- **Platform:** QEMU as primary target

## Reference Codebase

minix.rs's architecture is based on MINIX 3. When the docs reference "MINIX 3 source", they mean
paths within the MINIX 3 source tree (e.g., `kernel/proc.c` means the `kernel/proc.c` file in a
MINIX 3 checkout). Key reference files:

- `kernel/proc.c` -- IPC implementation (mini_send, mini_receive, deadlock detection)
- `kernel/proc.h`, `priv.h` -- Process and privilege structures
- `kernel/system.c` -- Kernel call dispatch
- `include/minix/ipc.h` -- Message structure definitions
- `include/minix/com.h`, `callnr.h` -- Server endpoints, call numbers
- `sys/sys/errno.h` -- errno values. Modern MINIX 3 keeps the MINIX-specific band (200+) here under
  the NetBSD layout, *not* in `include/errno.h`; the classic book-era tree has everything in
  `include/errno.h`
- `lib/libc/sys/*.c` -- POSIX syscall wrappers (template for musl adaptation)
- `lib/libsys/sef.c` -- SEF framework (template for server-rt)

The MINIX 3 source is available at https://github.com/Stichting-MINIX-Research-Foundation/minix

## Architecture

See the [Architecture chapter](book/src/architecture/overview.md) in `book/` for the full system
design. Key concepts:

- **Microkernel:** Only IPC, scheduling, interrupt dispatch, and memory protection in kernel
- **Message passing:** MINIX 3's 6 IPC primitives — 5 live (SEND, RECEIVE, SENDREC, NOTIFY, SENDNB);
  SENDA still an `ENOSYS` stub
- **User-space servers:** All OS services (file system, process management, memory management) run
  as separate processes communicating via IPC
- **Privilege model:** Fine-grained bitmaps control which processes can communicate and what kernel
  calls they can make

## Non-negotiables

These bind every task regardless of area. Everything else lives in
[`docs/conventions/`](docs/conventions/README.md).

- **Read the relevant [`docs/conventions/`](docs/conventions/README.md) file before working in that
  area.** The rules there are load-bearing and most were paid for by a defect.
- **Every new `.rs`/`.S` source file begins with the SPDX + copyright header**, before any other
  content — see [`rust-style.md`](docs/conventions/rust-style.md) for the exact two forms.
- **Every commit is `--signoff`ed and GPG-signed.** Never `--no-gpg-sign`, never `--no-verify`. The
  DCO trailer and the cryptographic signature are orthogonal and both are required.
- **Never commit to `main`; branch first.** Committing in auto mode is fine; **pushing, opening a
  PR, or triggering CI needs explicit approval** — that is where work leaves the machine.
- **A PR marks its own work complete** — check the slice/chunk box in the same PR that implements
  it, in `docs/plan.md` and in the matching `docs/plans/` detail file when one exists.
- **Markdown prose wraps at 100 columns**; run `~/.dprint/bin/dprint fmt` (and `check`) on anything
  you edit. Both `dprint check` and `tools/check-md-links.py` **block in CI** — a broken relative
  link or `#anchor` fails the PR.
- **The D8 ABI freeze:** `Message` layout, call numbers, endpoints and errnos change only via a
  deliberate ABI-bump PR touching both repos — there is C in another repository depending on all
  four.
- **The kernel is bare-metal only and never host-built**; `cargo check`/`clippy`/`test`
  cross-compile it, there is no `#[cfg(test)]` under `kernel/src/`, and **QEMU is the verification
  path** for kernel behaviour.
- **Verify before claiming.** Run the command, read its output, *then* say it passes. A marker that
  was not observed moving has not been proved.

## Where things are documented

Working rules — read the area file before touching that area:

| Area                                                                | Covers                                                                                                |
| ------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| [`build-and-boot.md`](docs/conventions/build-and-boot.md)           | build commands, the QEMU boot budget, the musl/SDK toolchain flavours, ELF and stack-frame inspection |
| [`ci.md`](docs/conventions/ci.md)                                   | the eleven CI gates, which block, and what to run locally before pushing                              |
| [`git-and-prs.md`](docs/conventions/git-and-prs.md)                 | branching, signing, DCO sign-off, merge policy, and what a PR owes the plan trackers                  |
| [`kernel.md`](docs/conventions/kernel.md)                           | `kernel/src/` — crate shape, `unsafe` and static-table discipline, IPC, scheduling, memory, grants    |
| [`servers-and-drivers.md`](docs/conventions/servers-and-drivers.md) | `servers/`, `drivers/`, `fs/`, `userland/` — crate build, SEF, DS discovery, per-band contracts       |
| [`abi.md`](docs/conventions/abi.md)                                 | `kernel-shared` — message layouts, request bands, errno bands, generated C headers, the D8 freeze     |
| [`rust-style.md`](docs/conventions/rust-style.md)                   | SPDX headers, overflow-safe arithmetic, `Display` width, clippy traps in `no_std` crates              |
| [`testing-and-markers.md`](docs/conventions/testing-and-markers.md) | trace samplers, the boot-marker files, and the mutation-testing discipline                            |
| [`docs-and-workflow.md`](docs/conventions/docs-and-workflow.md)     | which documentation tree owns what, the superpowers slice workflow, and the review sweeps             |

Everything else:

| Tree                                                    | Holds                                                                                                                                             |
| ------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`book/`](book/src/SUMMARY.md)                          | **Canonical** how-the-system-works documentation (mdBook, published to GitHub Pages). Write new documentation here, derived from source.          |
| [`docs/plan.md`](docs/plan.md)                          | The lean live tracker: phase status plus one summary line per slice.                                                                              |
| [`docs/plans/`](docs/plans/phase-6-prep.md)             | Full per-phase slice histories, the phase design documents, and the inter-phase prep trackers. Read the phase file before starting a slice in it. |
| `docs/superpowers/specs/` and `docs/superpowers/plans/` | The reasoning behind one slice — decisions considered and rejected, per-task steps, verification plan.                                            |
