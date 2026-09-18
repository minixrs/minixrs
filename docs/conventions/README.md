# Conventions

These files hold minix.rs's working rules — the conventions, invariants and hard-won traps that bind
code and process in this repository. The top-level [`CLAUDE.md`](../../CLAUDE.md) keeps only the
non-negotiables that apply everywhere and links here for everything else. Read the file for the area
you are about to work in *before* you work in it; link across files rather than restating a rule, so
each rule has exactly one home.

| File                                                 | Read this before …                                                                                                                                        |
| ---------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`build-and-boot.md`](./build-and-boot.md)           | building the kernel, booting under QEMU, choosing a `timeout` budget, or inspecting a built ELF, a stack frame, or a boot log                             |
| [`ci.md`](./ci.md)                                   | pushing a branch — what the eleven gates run, which block, what to run locally first, and how the `hello` toolchain flavour differs per job               |
| [`git-and-prs.md`](./git-and-prs.md)                 | branching, committing, signing and signing off, merging a PR, or updating the plan trackers a PR owes                                                     |
| [`kernel.md`](./kernel.md)                           | touching `kernel/src/` — crate shape, `unsafe` and static-table discipline, assembly placement, IPC/scheduling invariants, and the memory/grant contracts |
| [`servers-and-drivers.md`](./servers-and-drivers.md) | touching `servers/`, `drivers/`, `fs/` or `userland/` — crate build and branding, SEF startup, DS discovery, and the per-band request/reply contracts     |
| [`abi.md`](./abi.md)                                 | changing anything in `kernel-shared` — message layouts, request bands, errno bands, grant and `uspace` ABI shapes, the generated C headers, the D8 freeze |
| [`rust-style.md`](./rust-style.md)                   | writing Rust or assembly anywhere — the SPDX header, overflow-safe arithmetic, forward-declaration allows, and the clippy traps a `no_std` crate hits     |
| [`testing-and-markers.md`](./testing-and-markers.md) | verifying a server that cannot print — trace samplers, the boot-marker files, and the mutation-testing discipline                                         |
| [`docs-and-workflow.md`](./docs-and-workflow.md)     | writing documentation or running a slice — which tree owns what, the superpowers workflow, the review sweeps, and markdown formatting                     |
