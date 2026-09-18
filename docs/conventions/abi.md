# ABI Conventions

Rules for `kernel-shared` and the cross-repo ABI it defines: message types, the `NR_SERVED_PROCS`
capacity ceiling, the errno bands, the generated C headers, the request-band allocation, the grant
ABI, the address-only `uspace` ABI, and the D8 freeze. For kernel-internal implementation details
(IPC primitives, scheduling, page tables) see [kernel.md](./kernel.md); for how servers and drivers
consume these bands over IPC see [servers-and-drivers.md](./servers-and-drivers.md).

## `kernel-shared`: the single ABI crate

- `Message` types are defined in `kernel-shared` and shared across all crates — never duplicate a
  message layout in a consuming crate.
- `kernel-shared` carries **zero `unsafe`** — keep it that way (geiger measures per-package).
  Byte-level ABI helpers (e.g. `GrantEntry::from_ne_bytes`) decode field-by-field, and tests tie the
  codec to the real layout via `offset_of!` rather than reading the struct's memory image.
- `kernel-shared` is unconditionally `no_std` (no `cfg_attr(not(test))`), but a `#[cfg(test)]`
  module may declare `extern crate std;` locally when fixed-size arrays are impractical — libtest
  links std anyway.

## `NR_SERVED_PROCS`: the one capacity ceiling

`kernel-shared::com::NR_SERVED_PROCS` is the exclusive proc-nr ceiling the user-space servers track.
All three per-process server tables derive their size from it — PM `mproc`, VM `ClientRegions`,
SCHED `policy` — and each carries a `const _: () = assert!(… >= NR_SERVED_PROCS)` guard so an
under-sized local edit fails at compile time. Never reintroduce an independent capacity literal.

## Errno bands (D7)

`kernel-shared/src/error.rs` has two bands, and nothing may land outside them:

- **POSIX block, magnitudes `1..=40`** — classic book-era MINIX values, identical to Linux/musl's
  (the point: musl's stock `bits/errno.h` and `syscall_ret.c`'s `r > -4096UL` work unmodified).
- **MINIX-specific IPC band, magnitudes `>= 200`** — modern MINIX 3 `sys/sys/errno.h` values.
- The **`41..=199` gap is forbidden** — that range is where musl defines errnos minix.rs has not
  adopted.

Constants are stored **negated**. An `errnos!` macro takes the positive magnitude and emits both the
`pub const` and `pub const ALL: &[(&str, i32)]` — `ALL` is the single source of truth for the header
generator, the `const _` band/distinctness guards, and the host tests. **Add an errno by adding one
line to the `errnos!` invocation, never by hand-editing a second list.**

(slice 5.0 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md))

## Generated C headers

`tools/gen-c-headers` (package `minixrs-gen-c-headers`) prints `include/minixrs/{ipc,com,callnr,
errno}.h` from the live Rust constants. The headers are a **build artifact under `target/` and are
never committed** — drift is impossible by construction, and CI asserts nothing was written into the
tree.

Preserve these when extending the generator:

- Generated C uses **C11 keywords only** (`_Static_assert`, `_Alignas` on the struct's first member,
  `_Alignof`) — never GNU attributes.
- `minixrs/ipc.h` **includes nothing**; `offsetof` comes from `__builtin_offsetof` under the private
  name `_MINIXRS_OFFSETOF`.
- Every process gets **both** `<NAME>_PROC_NR` and `<NAME>_EP` — they differ for kernel tasks (e.g.
  `SYSTEM_PROC_NR` is −2, `SYSTEM_EP` is 32766) — and the header `_Static_assert`s the C decode
  macro against the Rust-computed endpoints.
- `minixrs/errno.h` **asserts but never defines** the POSIX block, behind `#ifdef
  MINIXRS_ABI_CHECK_POSIX_ERRNO` — CI has no musl sysroot and a host `<errno.h>` disagrees.

**Adding a request number means editing `tools/gen-c-headers/src/callnr_h.rs` too.** Unlike
`error.rs`'s `ALL`, the per-band `members` lists in `bands()` are **hand-maintained**, so bumping an
`NR_*_MSGS` constant without adding the row leaves it silently absent from the generated header —
the `c-headers` CI gate still passes, because it compiles a header that simply never mentions it.
Run `cargo test -p minixrs-gen-c-headers` after any band change, not just `cargo gen-c-headers` —
the crate's own `every_band_member_list_matches_its_count` test is what catches this.

(slice 5.0 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md))

## Request-band allocation

Each server's request numbers live in a dedicated 0x100-wide band below `NOTIFY_MESSAGE`:

| Band  | Base    |
| ----- | ------- |
| PM    | `0x700` |
| VFS   | `0x800` |
| FS    | `0x900` |
| BDEV  | `0xA00` |
| CDEV  | `0xB00` |
| VM    | `0xC00` |
| SEF   | `0xD00` |
| DS    | `0xE00` |
| SCHED | `0xF00` |

Only **ascending order** is load-bearing — `callnr_h.rs`'s `bands_are_in_ascending_numeric_order`
test enforces it, not the specific gaps between bands. `0x700..0xC00` is now **fully allocated**
(PM/VFS/FS/BDEV/CDEV); a tenth band needs a home outside that span. The BDEV/FS pairing was recorded
backwards in four in-tree comments across slices 5.3–5.6 before being corrected in 5.7 — verify a
band number against the constant's own definition, not against a neighboring comment.

(slices 4.5, 4.3, 5.7, 5.4, 5.3, 5.8 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md) and
[phase-4-servers.md](../plans/phase-4-servers.md))

## Grant ABI (D4)

`kernel-shared/src/grant.rs` defines the cross-address-space copy ABI: a flat `#[repr(C)]`
`GrantEntry` (flags/seq/who_to/who_from/addr/len, 32 bytes, layout pinned by `offset_of!` asserts
because the kernel decodes it from raw bytes), MINIX's CPF flag values, and **`GRANT_SHIFT = 20`**
id packing (`grant_id`/`grant_idx`/`grant_seq`).

A granting process keeps its `GrantEntry` table in its **own** address space and registers `(addr,
entries)` with `SYS_SETGRANT`; `SYS_SAFECOPY` reads the entry back out of the granter's address
space on every call, so a granter revokes by writing its own memory and the kernel caches nothing
that could go stale.

A server that holds `SYS_COPY`/`SYS_SAFECOPY` and serves clients that do not must take the **granter
from the kernel-stamped `m_source`, never from the payload** — a caller-supplied granter endpoint
turns that server into a confused deputy, aiming a privileged cross-AS copy wherever the caller
points. Apply this to every grant-id-carrying request.

(slice 5.2 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md))

## `uspace`: the address-only ABI

`kernel-shared/src/uspace.rs` is an **address** ABI, not a message one — device/ramdisk window bases
and sizes. It is deliberately **not** emitted in the generated C headers.

(slice 5.3 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md))

## D8: the ABI freeze

Past slice 5.6, `Message` layout, call numbers, endpoints, and errnos change **only** via a
deliberate ABI-bump PR touching both repos — there is C in another repository (the musl fork)
depending on all four.

Since the musl fork's port branch is **force-pushed** on rebase, a fork rebase and the
`external/musl` submodule bump must land in the **same PR** as any ABI change here, or this repo
pins an orphaned commit.

(slice 5.6 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md))
