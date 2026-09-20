# Servers and Drivers

Conventions for the user-space half of minix.rs: how a server or driver crate is built and branded,
how it reaches its receive loop, how it finds its peers, how PM drives the process lifecycle, and
the request/reply contracts that bind every band. The kernel-side halves of these rules live in
[kernel.md](./kernel.md); request numbers, band bases, payload offsets and the ABI freeze live in
[abi.md](./abi.md); the lint and arithmetic traps that bite server code live in
[rust-style.md](./rust-style.md). Link across, never duplicate.

## Building a user-space server

User-space servers build as freestanding `#![no_std]`/`#![no_main]` ELFs linked with their own
`user.ld` (page-aligned PT_LOADs, base `0x10_0000`; the kernel sets `sp_el0`, so `_start` needs no
stack setup). Since M1 the kernel's `build.rs` builds them for the **custom target
`tools/targets/aarch64-unknown-minixrs.json`** (the pinned nightly's `aarch64-unknown-none` spec
plus `"os": "minixrs"`; regenerate on nightly bumps with `rustc -Zunstable-options --print
target-spec-json --target aarch64-unknown-none` and re-apply the `"os"` line) via
`-Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem`, all into
**one shared** nested `CARGO_TARGET_DIR` (`target/minixrs-user` — separate from the outer `target/`
root, so nested cargo can't deadlock on the kernel build's lock, but shared across the 9 crates so
build-std compiles core/alloc once, not 9×).

The `-T<user.ld>` link arg comes from **each crate's own `build.rs`**, cfg-gated on `target_os =
"minixrs"` (host `check`/`clippy` stay linker-flag-free); `kernel/build.rs` injects no rustflags and
scrubs inherited ones. `boot_image/elf.rs` is the minimal ET_EXEC/AArch64 loader (PT_LOAD →
`alloc_frame` + HHDM copy + map; BSS via zeroed frames). The workspace `[workspace.lints.rust]
unexpected_cfgs` check-cfg shim in the root `Cargo.toml` is what keeps host clippy green on the
`"minixrs"` cfg — delete it at M5 when the real rustc target exists.

`-Zjson-target-spec` must be passed **explicitly** at that call site even if your builds work
without it: a *global* `~/.cargo/config.toml` carrying `[unstable] json-target-spec = true`
(RustRover needs one for `.json` specs) silently satisfies the gate on your machine while CI, which
has no such file, fails the nested build. Same class of trap for any unstable cargo flag — reproduce
a suspected config-masking failure with `CARGO_UNSTABLE_<FLAG>=false cargo …`, which overrides the
config the way a bare CI runner does.

### Branding

Every user-space binary crate invokes `minixrs_abi_note::brand!()` at the crate root, emitting the
28-byte minixrs ELF identity note (owner `"minixrs\0"`, type 1, `[abi_version=1, flags=0]`; spec in
the tooling repo's `docs/abi-note.md`) into `.note.minixrs.ident` — a dedicated PT_NOTE phdr at the
start of the RO PT_LOAD, per the `user.ld` dual-assignment rule (`KEEP()` is mandatory, and the rule
must stay ahead of the `/DISCARD/ *(.note.*)` line).

The kernel **refuses unbranded ELFs**: `kernel-shared::brand::scan_brand` (pure, host-tested) runs
in `elf::load_into` (`ElfError::MissingBrand`/`UnsupportedAbi` → `[brand] reject: …` trace, then
boot panic / exec `ENOMEM`) and again as a pack-time assertion in `kernel/build.rs` — so a new
binary crate that forgets `brand!()` fails the kernel build, not the boot. `[brand]` traces appear
only on failure; a healthy boot has none.

### Watching dependencies

When a boot server gains a new path dependency, add that crate's `src` dir to `kernel/build.rs`'s
server `rerun-if-changed` list (a directory is watched recursively) — otherwise edits to the dep
silently embed a stale server ELF. That list covers *shared* inputs only (`minixrs-ipc`,
`server-rt`, `kernel-shared`, `minixrs-abi-note`, plus the target JSON); a newly added boot crate
needs no entry of its own — `build_server` already watches each crate's
`src`/`user.ld`/`Cargo.toml`/`build.rs`.

### ELF-only attributes, and why `forbid(unsafe_code)` is unusable

ELF-only attributes on server crates (`#[unsafe(link_section = ".text._start")]`, etc.) must be
`#[cfg_attr(target_os = "minixrs", ...)]`-gated (the M1 target; they read `"none"` before that) —
`cargo check --workspace` also builds servers for the Mach-O host, which rejects ELF section
specifiers.

Relatedly, `#![forbid(unsafe_code)]` is unusable on any freestanding binary crate here — edition
2024's `unsafe(no_mangle)` / `unsafe(link_section)` on `_start` trip the `unsafe_code` lint — so a
crate with no `unsafe` *block* (e.g. `drivers/memory`) says so in its crate docs instead of
asserting it with the attribute.

## Verifying server behaviour

A server runs at EL0 and cannot print, so its behaviour is verified from kernel-side traces, not
from server-side logging. The traces, the two samplers' different sampling rules, and the
`SYS_DIAGCTL` diagnostic channel are documented once, in
[`testing-and-markers.md`](./testing-and-markers.md#verifying-server-behaviour) — this file does not
restate them.

## SEF: the receive loop

System servers drive their receive loop through `server-rt`'s SEF (slice 4.1+):

```rust
let sef = sef_startup(SefConfig { init_fresh, signal_handler });
loop {
    if sef.receive(&mut msg) != OK { continue }
    match msg.m_type { /* … */ }
}
```

Callbacks pass via the config struct and are carried in the returned `Sef` handle (no global
`setcb`/static state, so `server-rt` is `#![forbid(unsafe_code)]`). `sef_startup` learns the
server's endpoint and name via `SYS_GETINFO(GET_WHOAMI)` (through `server-rt::sys_getinfo`),
announces the server via `diag_print`, and `sef.receive` filters SEF control messages, returning
only application messages.

The pure classifier lives in `server-rt/src/classify.rs` (host-tested; the IPC glue in `sef.rs` is
coverage-excluded like the server `main.rs`es) and gates each control event on `m_source`, not
`m_type` alone — NOTIFY ping from RS, `SEF_SIGNAL` from PM/RS, `SEF_INIT` from RS — so a client
holding only an `ipc_to` bit to the server can't spoof a signal or init.

Shared client-side helpers belong in `server-rt`, not in per-server copies: `payload.rs`
(`rd_i32`/`wr_i32`/`rd_u64`/`wr_u64`/`buf_addr`), `kcall.rs` (`sys_safecopy`/`sys_copy`),
`diag_fmt`, `sys_getinfo`, `sef_retrieve_from_ds`, and `cdev.rs`'s four-field CDEV parse. Payload
accessors use `checked_add` — see [rust-style.md](./rust-style.md).

## The MXBI archive

Boot servers are packed into a single **MXBI archive** (slice 4.2+): `kernel/build.rs`'s
`build_server(name, dir, …)` builds each server crate into the shared nested `CARGO_TARGET_DIR`
`target/minixrs-user`, `pack_mxbi` concatenates them under a 16-byte header (`magic "MXBI"` / ver /
count / total) plus 32-byte records `{proc_nr:i32, offset:u32, len:u32, name:[u8;20]}` (all LE), and
emits `BOOT_IMAGE_PATH`.

To add a boot server, append a `(crate, dir, proc_nr)` row to the `servers` array (proc_nr from
`kernel-shared/com.rs`) and watch its `src` dir. The `env!("BOOT_IMAGE_PATH")` `include_bytes!`
lives only in `boot_image/mod.rs`, which is `#[cfg(target_os = "none")]` — that gate is what keeps
host `cargo check`/`test` (env unset) compiling, so never reference `BOOT_IMAGE_PATH` from a
host-compiled module. `boot_image::BootImage::iter()` drives `userland::load_boot_server(nr, elf)`
(the generalized `vm_bootstrap`); all servers share one stack range — `uspace::USER_STACK_BASE ..
USER_STACK_TOP`, 16 pages — since each has its own TTBR0.

No new boot priv wiring is needed — `init_boot_image` already grants every boot server `SRV_T`
`ipc_to` over `[0, n_active)`.

**A negative `proc_nr` is the exec-only sentinel.** The `userland.rs` load loop skips any negative
`proc_nr`, so a module packed under `com::EXEC_ONLY_PROC_NR = -1` is resolvable by name through
`BootImage::module_by_name` but **never boot-loaded** — no `[as]` line, no proc slot, no priv slot.
That is how `userland/worker` gets into the archive as an exec target only (slice 4.7 — see
[phase-4-servers.md](../plans/phase-4-servers.md)).

**Array position is load-bearing.** DS ordering is a chain — `ds < tty < memory < mfs < vfs` — and
it is satisfied only by array position in `kernel/build.rs`, so that a server is in its receive loop
before its first client looks it up (slice 5.8 — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

## DS: discovering peers

Servers discover each other via **DS** (the name→endpoint registry, `servers/ds/`): every server
publishes its endpoint at SEF init by setting `init_fresh: Some(...)` to a callback that calls
`server-rt::sef_publish_to_ds(endpoint, name)` (a `DS_PUBLISH` SENDREC; key = 16-byte NUL-padded
name in payload `0..16`, endpoint `i32` in `16..20`).

**DS is the exception** — it seeds its *own* entry in-process in `ds_init` (`registry::publish(name,
endpoint)`), because a SENDREC to itself before reaching its receive loop would deadlock. The
registry is a static `[Entry; 16]` `UnsafeCell` newtype like `vm/region.rs`; its pure
`publish`/`retrieve`/`check` are host-tested, the SEF/IPC `main.rs` is coverage-excluded (add each
new server `main.rs`, glob-covered, plus any new `server-rt` IPC-trap module like `ds.rs`
explicitly, to `sonar.coverage.exclusions`).

**Publish-before-retrieve is not deterministic.** It holds only because of the packing order above,
so every DS lookup must **fall back to `boot_endpoint(…)`** and emit a distinguishable diag line —
that keeps the rest of the proof alive while only the `*.ds ok` marker for that lookup disappears
(slices 5.3 and 5.8 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

## Privilege wiring

`init_boot_image` fills a boot server's `ipc_to` only for active boot priv slots `[0, n_active)`
(~0–15). A hand-installed stub in a higher priv slot (16+) that a server must *reply* to needs the
reverse `ipc_to` bit opened explicitly — see `install_stub_d_priv` opening VM→D after setting D→VM.

The shared user priv slot is the same shape: `populate_user_priv`'s `USER_IPC_TO` widens
`USER_PRIV_ID` (slot 20), and **every entry costs a pair of bits**, because the slot is above
`n_active`. Drop the forward bit and a client's SENDREC is refused and every downstream marker
vanishes; drop the reverse and the request still *happens* and only the reply is refused, so the
client hangs mid-SENDREC and takes the rest of its cycle's markers with it (slice 5.4 — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

## VM: regions and unmapping

VM (`servers/vm/`) tracks per-process memory as a static `[ClientRegions; MAX_CLIENTS]` keyed by
proc number. `MAX_CLIENTS = NR_SERVED_PROCS` (32), so the table covers PM's whole fork pool; each
client holds up to `MAX_REGIONS = 16` regions — the two are not the same number. VM owns no heap
allocator (the kernel owns frames). Each region is a half-open `[start, end)` tagged `Kind::{Heap,
Mmap, Unused}`. A page fault is satisfied only when its address lies inside a region; out-of-region
faults are a silent SIGSEGV (faulter left blocked on `RTS_PAGEFAULT` — real signals are Phase 4).
`VM_BRK`/`VM_MMAP`/`VM_MUNMAP` all ride the single D→VM SENDREC edge, so adding an mmap client needs
no new priv wiring beyond the brk one.

`SYS_VMCTL(VMCTL_PT_UNMAP)` returns `EINVAL` (no panic, no frame freed) when nothing is mapped at
the target VA — so VM's `munmap` can sweep a region page-by-page with `VMCTL_PT_UNMAP` and ignore
the never-faulted pages. Keep the unmap sweep capped at the region's own `end` so an overstated
`len` can't reach a neighbour's frames.

A region base is not enough on its own: two regions grow on request (the mmap bump cursor, and the
heap's end via `set_brk`), so **both** carry a runtime `region::REGION_LIMIT` check (`ENOMEM` past
it). `REGION_LIMIT` is the base of the lowest kernel-owned VA window, which is why adding a window
above it needs no VM edit — see [kernel.md](./kernel.md).

## PM: the process lifecycle

**A user process drives its whole lifecycle through PM, never through the kernel.** That is the
POSIX shape — user → PM for `PM_FORK`/`PM_EXIT`/`PM_WAIT`/`PM_EXEC`, and PM issues the kernel calls
on the caller's behalf. The shared `USER_PRIV_ID` was opened `ipc_to = {PM}` alone for exactly that
reason (widened to `{PM, VFS}` in slice 5.4) (slice 4.6b — see
[phase-4-servers.md](../plans/phase-4-servers.md)).

### `handle_fork`: a fixed order the frozen-child invariant depends on

PM owns the whole tree, in this order and no other:

1. allocate an `mproc` child slot from the fork pool `[FORK_POOL_BASE = NR_BOOT_PROCS +
   NR_STUB_PROCS, NR_MPROCS = 32)` — **the slot index *is* the child's kernel proc-nr**, so the
   pool's base moves with `NR_STUB_PROCS` (see [build-and-boot.md](./build-and-boot.md));
2. `SYS_FORK(parent_e, child_nr)`;
3. `VM_FORK(parent_e, child_e)`;
4. `SCHEDULING_START`;
5. `SYS_PRIVCTL(PRIVCTL_SET_USER)` release;
6. reply to **both** halves of the shared SENDREC — child `m_type = 0`, parent `m_type = child_pid`
   (MINIX fork-returns-twice).

**Why the order is safe:** the kernel's frozen-child rule — `do_fork`'s freeze plus
`sched::rts_unset`'s enqueue-on-last-bit invariant, both in [kernel.md](./kernel.md) — means steps 4
and 5 cannot make the child runnable, so **only step 6, PM's reply, can**. The child therefore
cannot run before its identity, memory and scheduling are built.

**PM rolls back at every step.** `SYS_FORK`, `VM_FORK`, `SCHEDULING_START` and `SYS_PRIVCTL` each
check their result and, on error, `SYS_EXIT` the child plus `mproc::cleanup` the slot before
returning the errno to the parent. The post-`SCHEDULING_START` rollback does `SCHEDULING_STOP`
**first**, while the endpoint is still valid — mirroring `handle_exit`. The last two steps are
boot-server-backed and cannot fail with correct wiring, so their guards are defence in depth against
ever recording an unrunnable child, which would hang the parent's `wait()` forever.

Fork needs **no new priv wiring**: PM↔VM and PM↔SCHED are boot-server `[0, n_active)` edges, and
child↔PM is the `USER_PRIV_ID` edge.

VM's half is `region::fork(parent_nr, child_nr)`, which copies the **whole `ClientRegions`** as a
`Copy` snapshot (slice 4.6b — see [phase-4-servers.md](../plans/phase-4-servers.md)).

### `handle_exit` and `handle_wait`

- **`SCHEDULING_STOP` before `SYS_EXIT`, always** — once `SYS_EXIT` bumps the endpoint generation,
  `okendpt` rejects the endpoint and the stop can no longer be issued.
- PM then marks the `mproc` slot a zombie holding the encoded status (`W_EXITCODE = (status & 0xff)
  << 8`), and **sends the dead child no reply**.
- `handle_wait` either reaps a zombie child (reply pid + status, then `cleanup` the slot) or, with a
  live child, sets `MF_WAITING` and **suspends** the parent with no reply until `handle_exit` wakes
  it directly.
- **There is no async `SIGCHLD`.** The kernel signal path default-*terminates*, which would kill a
  handler-less parent, so parent-notify is the zombie plus wait-reap handshake and nothing else
  (slice 4.6b — see [phase-4-servers.md](../plans/phase-4-servers.md)).

### `mproc` keeps its logic host-testable

`servers/pm/src/mproc.rs` stores a generation-aware `endpoint` per slot (`boot_endpoint(slot)` for
seeded procs, the `SYS_FORK` reply for children) plus `exit_status` and `MF_WAITING`. Put the
free-slot allocator, the zombie marking and the reap in pure `*_in` helpers that take the slot table
as a parameter — that is what makes them host-tested with no IPC (slice 4.6b — see
[phase-4-servers.md](../plans/phase-4-servers.md)).

### Draining kernel signals

`SYS_GETKSIG` **hands off** the pending-signal bitmap, so PM must dispose of every returned
endpoint: `SYS_ENDKSIG` for a survivor, `SYS_EXIT` with **no** `SYS_ENDKSIG` after for a
termination. A full exit zeroes the signal state and frees the slot, so a post-exit acknowledge just
bounces off `okendpt` with `EDEADSRCDST` (slices 4.5 and 4.6a — see
[phase-4-servers.md](../plans/phase-4-servers.md)).

## init: PID 1

`init` (`INIT_PROC_NR = 10`) is a **real boot process**, not a hand-installed stub: it is packed
into the MXBI archive like any server and the ordinary `userland.rs` load loop loads it, clears
`RTS_NO_PRIV` and enqueues it — **no PM hand-release**, unlike a frozen stub's `PRIVCTL_SET_USER`.

**It carries user-grade privilege, which is the point.** Its `BootEntry.trap_mask` is `USR_T`, not
`SRV_T`, and it is pointed at the shared user priv slot rather than a dedicated server-grade one
(the `init_boot_image` special-case is in [kernel.md](./kernel.md)). So init SENDRECs its servers
and **makes no kernel calls at all** — exactly the forked-child profile, which is what makes it a
usable proof of the user-facing paths.

`MF_PRIV_PROC` stays set on init's `mproc` seed (unkillable PID 1). That flag gates only the kill
path — not fork, wait or getpid — so PM still serves init as an ordinary client.

The `userland/init` crate is **freestanding, like `worker` and not like a server**: dependencies are
`minixrs-ipc` and `kernel-shared` only, with **no `server-rt` and no SEF**, its `_start` shim and
panic handler are `not(test)`-gated, and its `user.ld` is `worker`'s verbatim (slice 4.8 — see
[phase-4-servers.md](../plans/phase-4-servers.md)).

## The grant pool: the client side

`server-rt::GrantPool<const N>` is **a value the server owns** — a `main`-frame local, never
`init_fresh`'s frame. That ownership is what keeps `server-rt` `#![forbid(unsafe_code)]`: there is
no static to hand out and no interior mutability to justify.

`ensure_registered` compares the pool's live address against the last registered one and re-issues
`SYS_SETGRANT` if they differ, so a pool that moved is re-registered rather than silently describing
the wrong memory. The kernel half of the grant contract is in [kernel.md](./kernel.md); the
`GrantEntry` ABI is in [abi.md](./abi.md) (slice 5.2 — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

## Request and reply contracts

These bind every band. The band bases and request numbers themselves are in [abi.md](./abi.md).

### Always reply

A server or driver **replies** to an unknown `m_type` — with `ENOSYS` — where DS may drop one. A
driver's and a server's clients all SENDREC, and a dropped request blocks the caller forever (slices
5.3 and 5.4 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Define and refuse, never fold into `ENOSYS`

A request you know about but decline gets its own errno, never `ENOSYS` — because `ENOSYS` is
already the unknown-`m_type` answer, and reusing it makes "doesn't know about this" and "knows and
refuses" indistinguishable. `BDEV_WRITE` was defined this way and answered `EROFS` (it stores for
real as of slice 5.10a), and VFS's `do_write` answered `Ok(Fd::File { .. }) => EROFS` rather than
folding it into the unused case — in both cases the refusal arm was the one-line landing site for
the real implementation (slices 5.7, 5.8 and 5.10a — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### The granter is `m_source`, never a payload field

No request in the CDEV, BDEV or FS bands carries a `granter` field: the server takes it from the
kernel-stamped `m_source`. The general confused-deputy rule this follows from is in
[kernel.md](./kernel.md#grants--the-governed-cross-address-space-copy).

The rule binds the *granting* side too: when VFS issues a `CPF_MAGIC` grant naming a caller's
buffer, the owner is the kernel-stamped `m_source`, never a payload field — VFS holds `SYS_PROC`, so
a caller-supplied owner would let any client aim that copy at a third party. **There must never be a
field for it.** Same for a control-plane `sys_copy(caller_e, …)`: `SYS_COPY` has no per-target
authorization at all, so a payload-supplied source would let any client read any process through the
server (slices 5.4, 5.7 and 5.8 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Short transfers: who may, who may not

Reply `m_type` **is the byte count** (`>= 0`) on `CDEV_WRITE`, `CDEV_READ`, `BDEV_READ`,
`VFS_WRITE`/`VFS_READ`, `FS_READ` and `FS_WRITE`.

- **`CDEV_WRITE` over `CDEV_MAX_IO = 256` is a short write, not a failure** — POSIX `write()`, and
  what lets a driver stage through a `main`-frame buffer with no allocator; the client re-sends with
  `offset` advanced.
- **`CDEV_READ`: `0` is EOF and a short read is legal**, so VFS sends one request and never loops.
- **`FS_READ` is clamped — a short read, not `EINVAL`** — and **a short `FS_WRITE` is normal**: the
  client is VFS, whose job is hiding staging from POSIX, and a read is short at EOF regardless.
- **`BDEV` refuses an over-long or out-of-range request with `EINVAL`, not a short read.** Its
  client is a filesystem that cannot interpret half a block. `EIO` stays reserved for Phase 6's real
  media errors, where the request was well-formed and the *device* failed.
- **`VFS_EXEC_STAGE` is the exception inside VFS: a short stream is `EIO`, not a short stage.** It
  is the one VFS transfer that may not be partial — an ELF cannot be loaded in pieces by a loader
  with no filesystem — so the surrounding "short transfers are normal" contract for
  `FS_READ`/`FS_WRITE` does **not** apply to it. State the exception before writing another staging
  path, or the wrong rule gets copied (slice 5.9 — see
  [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

These are not inconsistent: refuse when the client cannot use a fraction, clamp when it can (slices
5.3, 5.7, 5.8 and 5.10a — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Error relay

A negative `SYS_SAFECOPY` result is relayed **verbatim** — `EPERM` (bad grant) and `EFAULT`
(unmapped buffer) are different client bugs and must stay distinguishable.

A `BDEV_READ` failure inside MFS becomes **`EIO`**, because MFS's client addressed a *file*, not a
block. The two rules read as contradictory and are not: relay verbatim when the caller owns the
object that failed; translate when it does not (slices 5.3 and 5.8 — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Control plane travels inline; data travels by grant

A path, a name or a key rides **inline in the payload**, NUL-padded to a fixed width — `FS_PATH_OFF`
NUL-padded to `FS_PATH_MAX = 64` (so the longest path is 63 bytes, and a field with no NUL is
`ENAMETOOLONG` rather than a truncation that could resolve to another file), the `PM_EXEC` name, the
`DS_PUBLISH` key. Inline costs the callee no staging buffer — its scarcest resource — and deletes
the confused-deputy question, because there is no granter.

Data travels by grant with **no granter field and no grant-offset field**. Two shapes follow from
that, and both are deliberate:

- The CDEV loop advances a payload `offset` against one standing grant.
- VFS **re-grants per round** on `FS_WRITE` — a fresh magic grant over exactly the bytes that round
  will move, revoked after. Do not add an offset field to buy the other shape: re-granting is what
  keeps the confused-deputy surface at zero, and it costs one kernel call a round.

`FS_WRITE` reuses `FS_READ`'s payload field for field (inode, grant id, byte count, position) and
`FS_CREATE` reuses `FS_LOOKUP`'s wire codec verbatim, request and reply alike — one wire codec and
one clamp serve both directions. Only the grant's direction bit differs (`CPF_READ` vs `CPF_WRITE`),
checked by the kernel's `verify_grant` and **re-implemented by no server** (slices 5.8, 5.10a and
5.10b — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Don't stub a request nobody consumes

A request without a consumer is better absent than stubbed. MFS keeps **no per-open state**, so
there is no PUTNODE — which is also why `VFS_CLOSE` sends the FS nothing — and `FS_LOOKUP`'s reply
carries mode and size instead of a separate stat (slice 5.8 — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

## The 64 KiB stack

A server gets exactly `uspace::USER_STACK_BYTES` — 16 pages, all mapped eagerly at image load, with
an unmapped guard page (`uspace::USER_STACK_GUARD_BYTES`) immediately below and
`uspace::USER_REGION_LIMIT` below that. Overrunning the stack now *faults* in the guard page instead
of walking silently into the mmap arena — but a fault is still a crash: VM turns it into a SIGSEGV
that prints nothing `tests/qemu-boot.forbidden` catches, so the failure is still silent in the log.

- **A 4 KiB block buffer is a `main`-frame local again.** Under the old one-page stack it could not
  be — the frame base would have landed below the mapping — so `fs/mfs` kept its block and staging
  buffers in `.bss`. Sixteen pages made a block-sized local plausible again, which is precisely what
  `fs/mfs/src/lib.rs`'s `const _` tripwire was written to prompt. It has **fired and been
  re-aimed**: it now asserts `MFS_BLOCK_SIZE * 4 <= USER_STACK_BYTES`, the margin that keeps two
  block buffers plus ordinary frame overhead comfortably inside a frame. That margin, not the buffer
  count, is what `uspace::USER_STACK_BYTES` is published for — a server author has to be able to
  answer "may this be a local at all?" from a constant rather than from a comment.
- **Bigger buffers still belong in `.bss`.** VFS's 256 KiB exec stage would overflow a 64 KiB stack
  four times over, so it stays a static. The stack grew enough to make a *block* a local, not enough
  to make a quarter-megabyte staging buffer one.
- MFS's move out of `.bss` does **not** weaken TTY's "stage in `main`'s frame, not a static" rule —
  it satisfies it. That rule's load-bearing half is *the buffer outlives every call that names it*,
  and `main` never returns, so a `main`-frame local is as stable as a static: MFS's grant to MEM is
  still issued once at boot and `ensure_registered` still never re-fires. A **helper's** frame would
  compile and then leave the driver safecopying into a dead one; that is the shape to refuse.
- The capability token around MFS's block buffer is independent of where the buffer lives. Its
  load-bearing half is that `Blocks::read(&mut self) -> &[u8; N]` makes "hold a directory block
  across the next fetch" a borrow-check error, and that survived the move from `.bss` to `main`'s
  frame unchanged.
- **Do not grow a server's stack for a demo.** 64 KiB rather than 256 KiB is deliberate: at 256 KiB
  a server author stops thinking about the stack budget at all, which is the discipline this
  constant exists to enforce.

Check the largest stack frame in a built server whenever a handler grows a buffer — the
`llvm-objdump` recipe is in [build-and-boot.md](./build-and-boot.md), and note the warning there
that the obvious one-line version of it under-reports large frames by 7.5x (slices 5.7 and 5.8 — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md); the stack grew to 64 KiB in the user-VA-map slice
— see [phase-6-prep.md](../plans/phase-6-prep.md)).

## Drivers

### Replying, relaying, and register layouts

Beyond the shared contracts above, two driver-specific rules:

- PL011 register offsets are **duplicated** in `kernel/src/arch/aarch64/uart.rs` and
  `drivers/tty/src/pl011.rs` and cannot be shared: the kernel crate is bare-metal-only and pinned by
  `forced-target`, so it can never be a user-space dependency, and a register layout is a hardware
  fact, not a shared ABI.
- `drivers/tty` deliberately does **not** depend on `minixrs-driver-rt` (a 4-line placeholder) —
  Phase 6 makes that move, plus the `kernel/build.rs` watch entry (slice 5.3 — see
  [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Block drivers

- **A block driver must not depend on the filesystem format.** The `memory` driver's init check is
  device-level: `mkfs-mfs` writes a 32-byte **image header** into block 0's unused boot block (bytes
  `0..1024`, which MFS never reads) plus a 32-byte **tail label** in a reserved last zone, and the
  driver verifies those. Format-level facts are verified by a *client* through a real BDEV round
  trip. Phase 6 unwinds this when virtio-blk replaces MEM under an unchanged MFS.
- That header ABI lives in **`kernel-shared::rootfs`** — mkfs writes it, MEM and VFS read it, and
  none may depend on the others; `image.rs` re-exports it. `kernel-shared::rootfs` also holds the
  image's *contents* (`ROOTFS_MOTD`, `ROOTFS_PATTERN_*`, `ROOTFS_HELLO_PATH`,
  `rootfs_pattern_byte`), so a read proof is a *check* against the constant rather than a
  transcription of it.
- **The driver contains no `unsafe` block and never dereferences its mapping**: client transfers are
  `sys_safecopy`, and the boot self-check is `sys_copy(SELF, …, SELF, …)`, so a page that failed to
  map is an `EFAULT` *return value* rather than an EL0 abort — hence no MMIO sibling module and no
  new Sonar exclusion.
- The kernel builds the ramdisk mapping this driver reads; that pre-map and its `.expect()` rule are
  in [kernel.md](./kernel.md) (slice 5.7 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Minors are a per-driver namespace

`/dev/null` and `/dev/zero` are **CDEV minors 3 and 5 of the memory driver** (MINIX 3's
`NULL_DEV`/`ZERO_DEV`) while the same driver's ramdisk is **BDEV minor 0**. The request band tells
them apart, and nothing asserts `CDEV_MINOR_*` against `BDEV_MINOR_*`.

The four-field CDEV parse lives in **`server-rt::cdev`** now that two drivers decode it;
**validation stays per driver**.

**The memory driver never clamps.** `CDEV_MAX_IO` protects TTY's stack staging buffer and there is
no staging here, so a null or zero write answers the whole count with **no copy at all** — an
unmapped buffer *succeeds*, which is Linux's behaviour, so **never aim a `bad-buf` probe at
`/dev/null`** — and a zero read fills the whole request from a 256-byte static in `CDEV_MAX_IO`
steps, reporting partial progress on a mid-way failure (slice 5.11 — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Don't answer for a driver

A console `read()` is a real `CDEV_READ` to TTY, which answers `ENOSYS` from its unknown-request arm
until Phase 6 gives it RX — the same errno VFS used to guess, now the driver's own answer, so Phase
6 adds one TTY arm and touches VFS not at all (slice 5.11 — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

## VFS

### Absorbing short writes

`CDEV_MAX_IO` is a driver staging detail that must not reach `write()`'s return. VFS's write loop:

- re-sends with `offset` advanced over the *same* grant until the buffer is out;
- clamps `off` with `.min(len)`, so an over-reporting driver cannot walk past the buffer;
- breaks on `n == 0` rather than spinning;
- on an error **after partial progress, reports the progress** (POSIX: those bytes really went out).

**VFS does not loop on read** — POSIX allows a short `read()`; it loops on `write` because it may
not. `rw.rs` (formerly `write.rs`) holds the direction-agnostic step logic, since "a peer reporting
0" is EOF on a read and a stall on a write, and `advance`'s rules are otherwise identical; `open.rs`
holds what differs.

**Lift a retry loop's body into a step function.** A retry loop's rules for a misbehaving peer — a
driver reporting `0`, or more than it was asked for — are unreachable while the peer works, so a
pure step function is the only way to test them. It also keeps them Sonar-measured, since `main.rs`
is coverage-excluded and a sibling module is not (slices 5.4 and 5.8 — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Grants on the write path

VFS is the granter: `do_write` issues a **`CPF_MAGIC` grant naming the caller's buffer with the
driver as grantee**, so the bytes move in **one** copy, caller → driver, and VFS never touches them.
The owner is `m_source` — see the confused-deputy rule above.

`user_range_ok` in `do_write` is **defence in depth, not the gate**: removing it moves no marker,
because the kernel's page-table walk answers `EFAULT` regardless (slice 5.4 — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### The fd table

`servers/vfs/src/fd.rs`:

- `NR_FDS` is **VFS-local, not ABI**. Both of its sizings were chosen to keep a failure mode
  distinguishable: at `4`, the 4th slot made "past the end" and "not open" distinct; raised 4 → 8,
  it makes "lowest free" and "table full" distinct tests. Rows are sized from `com::NR_SERVED_PROCS`
  with the usual `const _` guard, and fds 0/1/2 are pre-opened to `CDEV_MINOR_CONSOLE` in every row.
- `Fd::File { ino, pos }` carries **no cached size** — EOF is `n == 0`, one source of truth, and a
  cached size is exactly what a write path would invalidate.
- `Fd::CharDev { dev: CharDriver, minor }` names its driver with an **enum, not an `Endpoint`**,
  because `DEFAULT_ROW` is a `const`.
- Storage is an interior-mutable `UnsafeCell<[FdRow; N]>` newtype (the `vm/region.rs` shape),
  **whose rule is never hold a table borrow across a SENDREC**. `Fd` is `Copy`, so the borrow dies
  at the destructuring `let`.
- **`resolve_in` takes the rows as a borrowed slice, and that is design-for-change, not style.** The
  table was an immutable `static` while nothing mutated it (which is how VFS kept zero `unsafe`);
  taking a borrowed slice is precisely what let it survive the switch to the `UnsafeCell<[FdRow;
  N]>` newtype untouched. Write a pure table helper that way from the start.
- **`alloc_in` scans upwards** from fd 3. A `.rposition()` returns 7 and passes every "did I get a
  descriptor" check (slices 5.4 and 5.8 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### The device-node table

`servers/vfs/src/dev.rs` is a three-row device-node table consulted **after the path copy and before
the mount** — a device open needs no filesystem. `O_CREAT`/`O_TRUNC` are ignored on a hit, the match
is exact bytes (there is no `/dev` on the image), and `/dev/other` falls through to MFS's `ENOENT`.
Paths are `callnr::DEV_*_PATH` so clients and VFS cannot drift (slice 5.11 — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Open flags and dispatch

`VFS_OPEN` gained `VFS_FLAGS_OFF` (an `i32`, `fcntl`'s values) without moving `NR_VFS_MSGS` — a new
*field* on an existing request, not a new request. Dispatch:

- `O_CREAT` on a lookup miss → `FS_CREATE`;
- `O_TRUNC` on a lookup hit → `FS_TRUNC`;
- `O_CREAT | O_TRUNC` on a miss takes the create arm and **stops**, because a fresh file is already
  empty.

So the truncate, when it runs at all, runs **before the descriptor is installed**, and a failure
never leaves a descriptor onto a half-truncated file (slice 5.10b — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Exec staging

`VFS_EXEC_STAGE` reads a binary off the filesystem into a VFS-owned buffer so that `SYS_EXEC` can
load it by grant. Four rules:

- **The path travels inline**, with no `SYS_COPY`. Its client is PM, which already holds the path
  inline — so there is no source process to misname, and the control-plane-travels-inline rule above
  applies with the confused-deputy question deleted outright.
- **A short stream is `EIO`, not a short stage** — the exception recorded under "Short transfers"
  above, and the one place in VFS where a partial transfer is not a legitimate answer.
- **The 256 KiB staging buffer is a `.bss` static.** It is the buffer a 64 KiB stack still cannot
  hold — four times over — which is why it stayed static when MFS's block buffer became a `main`
  local. Unlike that buffer it needs **no capability token and no borrow discipline**: VFS never
  dereferences the staged bytes, it only wants the buffer's address, which is why the crate keeps
  zero `unsafe` blocks.
- **Nothing releases the stage grant.** Re-granting per request bumps the sequence and kills the
  previous id, and PM serialises exec (slice 5.9 — see
  [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Prologue ordering

VFS's probe batteries run in a fixed order in `main`'s prologue, each new one appended **last** (the
retired BDEV demo, then `fs.deny`, then `mem.deny`), so that a hang localizes to the newest battery
instead of blacking out every earlier slice's markers. **Do not tidy the prologue into alphabetical
order** (slices 5.7 and 5.11 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

## MFS

### Crate shape

`fs/mfs` is split unusually hard because `[[bin]] required-features = ["server"]` removes `main.rs`
from every CI job (clippy `--all-targets`, miri, llvm-cov — none passes the feature): **every line
with a decision in it is in the lib** (`proto.rs` wire codec, `walk.rs` traversal and read policy,
the `Vec`-free rewrite of `mkfs-mfs`'s `verify.rs`). See [ci.md](./ci.md) for the extra `cargo
clippy -p minixrs-mfs --features server` step; `kernel/build.rs` threads `--features server` through
the nested build the way it threads PM's `--no-default-features`.

`proto.rs` re-derives its own `rd_i32`/`rd_u64` because `server-rt` is an *optional* dependency —
the trade `userland/init` already makes.

The crate is `no_std`, `#![forbid(unsafe_code)]` and has **one dependency** — keep all three: an
`unsafe`-free filesystem with a single dep is what lets its decisions be host-tested and
geiger-clean while the server half is invisible to CI.

The read path (`superblock`/`inode`/`dirent`/`layout`/`read`) is **I/O-free by construction**: every
reader takes bytes the caller already fetched. That is both what makes it host-testable with no fake
device and the shape the server needs. `zone_for_offset` distinguishes a `Hole` (reads as zeroes)
from `OutOfRange` (past the single-indirect span) — explicit, never silent zeroes (slices 5.7 and
5.8 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Degraded, never fatal

MFS is **degraded, never fatal and never a panic** past `sef_startup`: a failed mount means `ENODEV`
to everything, the `memory` driver's `blocks = 0` precedent.

**Every device-derived loop bound has a cap** — `walk::dir_size`'s `MAX_DIR_BYTES`, `size < 0 →
EIO`, `zone_ok` before every read. A corrupt `size = i32::MAX` would otherwise spin MFS, blocking
VFS, blocking init. VFS's probes cannot reach that class, which is why it needs unit tests (slice
5.8 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Two copies, not one

A `FS_READ` is **two copies** — device → MFS's block buffer → the caller's granted buffer — unlike
the write path's single caller-to-driver copy. A MinixFS read is rarely block-aligned at both ends,
and a *hole* has no device block to copy from at all. This is MINIX 3's own shape (slice 5.8 — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Capability tokens: the `Blocks` shape

MFS's single block buffer is reached only through the **`Blocks` capability token**, whose
`read(&mut self) -> Result<&[u8; N], i32>` makes "hold a directory block across the next fetch" a
**borrow-check error** rather than a comment. `Blocks` also carries `buf_mut` and `write`, and its
standing grant to the `memory` driver is **`CPF_READ | CPF_WRITE`** — one buffer, both directions,
one static address, so `ensure_registered` still never re-fires.

Widening those flags does **not** widen what any one call may do (the kernel checks the direction
per call), but it *does* re-arm anything that used the pool's grant id as a
deliberately-insufficient grant: a denial probe leaning on the narrow flags needs its own narrower
grant, aimed at a spare block (`START_BLOCK = 2` leaves block 1 free), in the same change — or the
probe writes the block buffer over the superblock (slices 5.8 and 5.10a — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### What the single block buffer forces

MFS has exactly one 4 KiB `.bss` block buffer, and that constraint — not taste — fixes `do_write`'s
step order: **read the inode, clamp, compute the grown size, place the zone, read-modify-write the
data block, write the inode back.** Nothing may hold a block across the next fetch, so every
intermediate is a `Copy` scalar and `place_zone` must finish with the buffer free (slice 5.10a — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Allocation ordering

- **A zone's bitmap bit is set before its number is stored**, so a mid-write failure leaks a zone
  rather than letting two files share one — the recoverable failure over the corrupting one.
- **The inode write-back is keyed on "a zone was assigned *or* the size grew", never on the size
  alone.** Filling a hole in the middle of an existing file assigns `zone[i]` without moving `size`
  at all; keying on size would drop that pointer while leaving its bitmap bit set, so the bitmap and
  the inode would disagree about a live zone — corruption rather than a leak.
- `do_trunc` frees before writing the inode back, and `create` writes the inode before inserting the
  dirent. Neither ordering is provable at the boot-log layer; keep them anyway (slice 5.10b — see
  [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).
- `find_free_slot` must let **`Occupied` beat `Free` across *every* block of a directory, not just
  within one**. Scanning block-by-block and returning the first free slot is backwards: a name that
  already exists in a later block gets shadowed by a duplicate insert into an earlier block's free
  slot — silently, no error, no marker. Scan every block for the name first, and only then reuse a
  free slot (slices 5.10a and 5.10b — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Stage before you allocate

`do_write`'s create path copies the client's bytes into a second 4 KiB `.bss` buffer **before
anything is allocated**, so no client-controlled failure can occur after an allocation.

Do **not** "fix" such a path by clearing the bitmap bit on the error arm: for an indirect slot whose
indirect block already existed on disk, the block still names the zone, so freeing the bit hands one
zone to two files — corruption rather than a leak (slice 5.10b — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

## Writing probes and denial batteries

A server's probe battery is how these rules stay proved. Three rules for writing one:

- **Write every denial probe so that a growing capability makes it fail loudly, not pass
  vacuously.** A probe asserting `EROFS` from `write()` on a file descriptor stopped being a denial
  the moment the write path became real, and silently became a successful overwrite — with every
  other marker still green.
- **Spell an unknown-request probe band-relative** (`VFS_RQ_BASE + NR_VFS_MSGS as i32`), never as a
  literal neighbour of an existing request. `VFS_WRITE + 1` became a real `VFS_OPEN` the moment the
  band grew. Spell a flag probe relative to the known set the same way: `fcntl::O_UNKNOWN_BIT` is
  *derived* from `O_KNOWN`, so a flag becoming real makes the probe fail loudly.
- **Never aim a "must be refused" probe at a file or block whose accidental write would destroy
  something**, and retire a probe visibly — decrement the `n=` in its marker so the retirement is a
  diff rather than a deletion nobody reviews.

Report through the path under test where that is what is being proved: a client's write proof must
be written **through** the descriptor under test (e.g. `/dev/console`, never fd 1), because the
descriptor is the only thing that proves the table row points where it claims (slices 5.8, 5.10a,
5.10b and 5.11 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).
