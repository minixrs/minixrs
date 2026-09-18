# Rust Style Conventions

Rust/assembly source conventions for minix.rs: the SPDX header, overflow-safe arithmetic in
release-profile crates, `Display` width handling, forward-declaration allows, the clippy traps that
only show up when a `no_std` crate is linted under its `std` test config, and one doc-comment
formatting trap.

## SPDX header

Every new `.rs`/`.S` source file must begin with the SPDX + copyright header before any other
content.

Rust (line-comment form):

```rust
// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
```

Assembly `.S` (block-comment form):

```c
/* SPDX-License-Identifier: BSD-3-Clause */
/* Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors */
```

Update the year as needed. `.toml`/`.ld`/`.conf` files get no header.

## `checked_add`, not `+`

Offset/length arithmetic in `server-rt` and in **any** server/driver/fs crate (`servers/`,
`drivers/`, `fs/`, `userland/`) must use `checked_add`, not `+`: `[profile.release]` sets
`overflow-checks = false`, so `off + 4` on a large offset *wraps* in the shipped binary (safe only
by accident, via a `None` from `get(a..b)`) while panicking under `cargo test`. Being explicit makes
both profiles agree.

This bites a crate's **own** payload accessors too, not just `server-rt`'s — slice 5.8 shipped
`off + 4` in `fs/mfs/src/proto.rs` and only a `usize::MAX` unit test caught it. Give every new
accessor that test.

## `Display` impls must render through `f.pad`

Custom `Display` impls that must honor `{:<width$}` render through a stack buffer
(`arrayvec::ArrayString<N>`) and call `f.pad(s)` — `write!(f, ...)` from inside `Display::fmt`
ignores the outer width spec.

## Forward declarations

Forward declarations intended for later slices (constants, fields, re-exports) get module-level
`#![allow(dead_code)]` with a one-line comment naming the consuming slice.

## `no_std` crates linted under their `std` test config

`no_std` library crates that host-test via `#![cfg_attr(not(test), no_std)]` get linted in their std
test-config too (`clippy --all-targets`). That surfaces a specific set of traps:

- A const-only `assert!(A > B)` trips `assertions_on_constants` — use a module-level `const _: () =
  assert!(…)` like `callnr.rs`.
- A bare `loop {}` in a function present under `test` trips `empty_loop` — use `loop {
  core::hint::spin_loop() }`. The `#[cfg(not(test))]` panic handler's `loop {}` is exempt because
  it's absent under test.
- Inside a `const _: () = { … }` block the const evaluator has no iterators (`for`, `.iter()`,
  `.windows()` are all unavailable) — use `while` + slice indexing + `<[T]>::len()`.
- Inside that same const context, `assert!` takes a **literal** message only (`assert_eq!` and
  `assert!(c, "{x}")` are both rejected).
- In a `#[test]` fn the reverse holds: iterate freely, but use `assert_eq!` rather than a bare
  `assert!(CONST op CONST)`, which trips the same `assertions_on_constants`.
- For an *ordering* comparison, where there is no `assert_eq!` form, write `assert_eq!(a.min(b), a)`
  (slice 5.3 needed this in `uspace.rs`, `callnr.rs`, and `vm/region.rs`) — or restructure into a
  loop over a `[(name, value, width)]` array so the operands stop being compile-time constants.
- A `const _: () = assert!(…)` may not reference a `static` (constants cannot refer to statics) —
  name the capacity as a `const` first and assert on that, the `vm/region.rs` `MAX_CLIENTS` shape.
- A `chunks_exact(N)` with a *const* `N` trips `chunks_exact_to_as_chunks` — use
  `as_chunks::<N>().0.iter()`, which also drops the trailing partial chunk rather than silently
  half-decoding it.
- `free >= X + 1` trips `int_plus_one` — write `free > X`.
- `N % M == 0` trips `manual_is_multiple_of` — write `N.is_multiple_of(M)`. This one fires **inside
  `const _: () = assert!(…)`** too, since `is_multiple_of` is const-callable, so a const guard is
  not an escape from it.

Both found in slice 5.10a: `int_plus_one` and `manual_is_multiple_of`.

## `doc_lazy_continuation`

The blocking `clippy --workspace` gate runs `-D warnings`: a doc-comment line starting with `+ ` (or
`- `/`* `) parses as a markdown bullet and trips `doc_lazy_continuation` on the following lines —
reword so continuation lines don't begin with a list marker.
