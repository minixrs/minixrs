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

**Array position is load-bearing.** DS ordering is a chain — `ds < tty < memory < vblk < mfs < vfs`
— and it is satisfied only by array position in `kernel/build.rs`, so that a server is in its
receive loop before its first client looks it up (slice 5.8 — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)). The `vblk < mfs` link is the one with **no
fallback line behind it**: MFS reads a missing `vblk` as "no virtio-mmio disk attached" and mounts
the ramdisk without a word, so a `vblk` that had not published when MFS looked would give a ramdisk
root, not a FAIL line. Only the per-backend marker pairs catch it, and since slice 6.4 both do: the
disk boot requires `bdev.ds ok name=vblk` and forbids `name=memory`, and the ramdisk boot requires
the `bdev.root vblk rc=-6 fallback=memory` line, which MFS prints only after finding `vblk` (slices
6.3 and 6.4 — see [the 6.3 design](../superpowers/specs/2026-11-21-virtio-blk-design.md) and
[the 6.4 design](../superpowers/specs/2026-11-21-mkimage-gpt-disk-design.md)).

## DS: discovering peers

Servers discover each other via **DS** (the name→endpoint registry, `servers/ds/`): every server
publishes its endpoint at SEF init by setting `init_fresh: Some(...)` to a callback that calls
`server-rt::sef_publish_to_ds(name)` (a `DS_PUBLISH` SENDREC; key = 16-byte NUL-padded name in
payload `0..16`, and no endpoint — DS registers the kernel-stamped `m_source`, so a server can
publish only itself; an endpoint `i32` rides in `16..20` only on a `DS_RETRIEVE` reply).

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

**A lookup whose miss is a healthy answer is the exception, and it needs a different guard.** MFS's
`vblk` lookup has no boot-endpoint fallback and prints nothing on a miss, because "no `vblk` in DS"
is how a boot with no virtio-mmio disk looks (since slice 6.4 only the hand-run
`MINIXRS_ROOT=nodisk` boot; both CI boots attach the boot disk). A silent miss cannot also be the
alarm, so the alarm is moved to the marker files: each checked backend requires a line MFS prints
only after a hit (see [`testing-and-markers.md`](./testing-and-markers.md#markers)).

**A server publishes before anything in it can block.** The publish goes in `init_fresh` and nothing
ahead of it may wait on a device, a peer or an interrupt. A server that blocks first is descheduled
with its name absent, the next server in packing order runs, and its lookup misses — on every boot,
not occasionally. DS has no blocking lookup and a retry loop in the client is polling.
[Block drivers](#block-drivers) has the case that forced the rule.

## Privilege wiring

`init_boot_image` fills a boot server's `ipc_to` only for active boot priv slots `[0, n_active)`
(0–19). A hand-installed stub in a higher priv slot (21+) that a server must *reply* to needs the
reverse `ipc_to` bit opened explicitly — see `install_stub_d_priv` opening VM→D after setting D→VM.

A boot process's priv id is its `IMAGE` index in `kernel/src/proc/table.rs`, so adding image rows
moves what is "above the image". The four virtio driver rows take priv ids 16–19, `USER_PRIV_ID` is
`N_IMAGE` (20), and the four demo stubs' dedicated priv ids are `USER_PRIV_ID + 1..=4` (21–24) —
above the user slot, so `install_stub_privs` can never overwrite an image row's privileges. Two
compile-time asserts pin it (`USER_PRIV_ID >= N_IMAGE`; the lowest stub priv id above
`USER_PRIV_ID`); the next row added to the image fails the build instead of colliding. With the
stubs on, a collision does not show as a `FAIL` line: it hangs the boot, and `--no-default-features`
cannot see it at all.

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
reason (widened to `{PM, VFS}` in slice 5.4 and to `{PM, VFS, VM}` in pre-Phase-6 chunk 5) (slice
4.6b — see [phase-4-servers.md](../plans/phase-4-servers.md)).

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
usable proof of the user-facing paths. `USR_T` admits **SENDREC only**: a user-grade `ipc_receive`
fails `ETRAPDENIED`. To block a user-grade process, SENDREC to a server that never replies (6.1's
`idle_park`).

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
  client is a filesystem that cannot interpret half a block. `EIO` is not available for this: on the
  BDEV band it means the request was well-formed and the *device* failed, and since slice 6.3
  `virtio-blk` really answers it (see [Block drivers](#block-drivers)).
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
  four times over, so it stays a static, and so does `virtio-blk`'s 20 480-byte partition-table
  buffer (five blocks, read once per boot) — the same `UnsafeCell` newtype with a hand-written
  `Sync`. The stack grew enough to make a *block* a local, not enough to make a quarter-megabyte
  staging buffer one.
- MFS's move out of `.bss` does **not** weaken TTY's "stage in `main`'s frame, not a static" rule —
  it satisfies it. That rule's load-bearing half is *the buffer outlives every call that names it*,
  and `main` never returns, so a `main`-frame local is as stable as a static: MFS's grant to the
  block driver it selected is still issued once at boot (plus, on the fallback boot only, the one
  issued to a `vblk` that declined and then revoked) and `ensure_registered` still never re-fires. A
  **helper's** frame would compile and then leave the driver safecopying into a dead one; that is
  the shape to refuse.
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

Beyond the shared contracts above, the driver-specific rules:

- PL011 register offsets are **duplicated** in `kernel/src/arch/aarch64/uart.rs` and
  `drivers/tty/src/pl011.rs` and cannot be shared: the kernel crate is bare-metal-only and pinned by
  `forced-target`, so it can never be a user-space dependency, and a register layout is a hardware
  fact, not a shared ABI.
- **QEMU 11.0.3's PL011 has no TX FIFO:** `INT_TX` is set on every `DR` write and cleared by a
  `UARTICR` write. Acking with `UARTICR` instead of clearing `TXIM` re-fires on every byte printed
  (83,741 interrupts in 60 s) while `irq ok` still prints; TTY's post-proof `irq FAIL extra` guard
  is what catches it. Disarm it by clearing `TXIM`.
- **A driver that owns an interrupt line claims and re-arms it through `minixrs-driver-rt`'s
  `IrqLine`** (`drivers/tty` does since slice 6.2). The crate's `src` is on the shared
  `rerun-if-changed` list in `kernel/build.rs`, so an edit re-embeds every driver ELF.
  `drivers/memory` deliberately does not depend on it: it has no interrupt and no device.

- **A driver identifies an interrupt by `m_source == boot_endpoint(HARDWARE)`, never by payload,**
  and re-arms with `IRQ_ENABLE` only after draining and acknowledging at the device — the kernel
  leaves the line masked until then, and a re-enable before the device is quiet re-enters at once
  (slice 6.1 — see
  [I12](../superpowers/specs/2026-09-29-sys-irqctl-design.md#i12--one-pattern-for-tty-rx-virtio-console-and-virtio-blk)).
  For a **virtio** device the acknowledgement comes *before* the drain: `InterruptStatus` is a
  level, so acknowledge exactly the bits read, walk the used ring until `pop_used` returns
  `Ok(None)`, then `rearm` — an acknowledgement placed after the walk would clear a completion that
  landed between the last pop and the ack, and the re-arm would find the line low. The order is
  written once, in `driver-rt`'s crate documentation. Re-arm after **every** interrupt on a healthy
  device, including one whose walk completed nothing: 6.3's mutation that dropped the re-arm hung
  the disk boot before `root ok`
- **`pop_used` returning `Err(QueueFault)` ends the device, it is not an element to skip.** A fault
  is sticky — nothing was consumed, so popping again reports it again — so stop the walk, write
  `FAILED`, print one `virtio FAIL step=io …` line, leave the line masked, and answer `EIO` from
  then on. The same goes for anything else that leaves a request unanswerable: a `rearm` that fails
  (the next completion would never arrive, so the next request would hang) and a failed receive
  while a request is in flight. 6.2's queue skipped a bad used element silently
- **A driver waiting for its own completion receives from `HARDWARE` only**, blocked, never polling.
  The filter is what makes one-request-in-flight need no queue in the driver: a second client stays
  a blocked sender in the kernel until the main loop receives again. The cost is stated, not hidden
  — a device that never completes hangs the driver with its client behind it, nothing reports it (RS
  pings DS, VM, SCHED, VFS and PM, never a driver — the symptom is the marker set going missing
  behind a silent `vblk` and MFS), and there is no timeout because no user-space driver holds an
  alarm
- **A virtio driver brings its device up after `sef_startup`, in straight-line code, never inside an
  `init_fresh` callback**: a callback that returns non-`OK` fails startup and leaves the driver
  spinning with no way to say why, where bring-up after the handshake can print one `[diag]` verdict
  line. No bring-up step waits on a register or a ring; and a failed step prints its name (`virtio
  FAIL step=<step>`), abandons the rest, and leaves the driver in its receive loop, replying to
  every request rather than dropping it. A driver whose `SYS_GETINFO(GET_VIRTIO_MMIO)` answers
  `ENOENT` has no device: it publishes nothing, prints `no device` and stays in its receive loop —
  healthy, not a failure. The one thing that **does** belong in `init_fresh` is the DS publish,
  decided by that same one kernel call and nothing that touches the device (see
  [Block drivers](#block-drivers))
- **A virtio driver never exits and never execs.** Its DMA run is ordinary RAM that every
  address-space teardown frees while the device may still be writing to it; `do_exit` and `do_exec`
  halt the kernel for a process holding a virtio assignment until a device-reset path exists
  ([`kernel.md`](./kernel.md#virtio-devices-and-the-dma-window))
- **`driver-rt` is the one place virtio register knowledge lives.** The negotiation sequence, the
  `VIRTIO_F_VERSION_1` requirement and the version-2-only refusal belong to its `Transport` and
  `negotiate`; a driver passes the optional features it wants and decides what a refusal means
- **`driver-rt`'s `unsafe` is confined to `mmio.rs` (register accesses) and `dma.rs` (every access
  to memory the device shares); keep `queue.rs`, `gpt.rs` and every driver's request path free of
  it.** (`gpt.rs`, the partition-table parser added in 6.4, is pure: byte slices in, no I/O, no
  `unsafe`, `checked_*` arithmetic throughout — see [Block drivers](#block-drivers).)
  `DmaRegion::new` is the one `unsafe` constructor — its contract is that the kernel-reported run
  stays mapped for the life of the process and that **no Rust reference to it ever exists**, since
  the device writes it too, and that the run is adopted **once**. Queue memory comes off the front
  with `DmaRegion::split_queue`, which consumes the region, refuses one that has already lent a
  buffer, and returns a `QueueMem` (not `Copy`, no writable view) plus the remainder; data buffers
  are `DmaBuf`s from the remainder's `buf`, bounds-checked and volatile. `VirtQueue::new` takes the
  `QueueMem`, `VirtQueue::add` takes `DmaBuf`s and `Transport::queue_enable` takes the queue. That
  is why those three are safe functions: every address the device is given derives from the one
  constructor, and **no buffer handle can ever overlap a ring**, so safe code cannot rewrite a
  descriptor and aim the device outside the run. (The first cut handed the queue a `Copy` `DmaBuf`
  like any other, and review found exactly that hole: keep a copy, `write_from` over a descriptor's
  address, notify.) Read the run before the split with `DmaRegion::peek`, which lends nothing. A new
  driver gets its buffers the same way and adds **one** `unsafe` block for the region, never a raw
  `(pa, len)` pair and never a `&mut [u8]` over the run — 6.2's queue held one, which was a contract
  the driver could not keep once the queue outlived bring-up. What the handles do not buy: two data
  buffers can still overlap each other (a logic bug: the device corrupts the driver's own data), and
  none of it constrains a hostile driver — there is no IOMMU
- **`Transport::notify` carries its own barrier.** `MmioTransport::notify` issues a `SeqCst` fence
  before the `QueueNotify` write, so the store that published `avail.idx` is ordered before the
  device is told to look whatever the caller did in between. Do not rely on `VirtQueue::add`
  happening to end in one, and give a second transport the same property
- **A mock device must model the device's refusals, not accept every write.** 6.2's `negotiate`
  re-set a status bit the device had just cleared; the mock transport stored whatever it was sent,
  so the host test passed while QEMU dropped the whole write. A mock that cannot say no proves only
  that the driver talks
- **Run `cargo miri test -p minixrs-driver-rt` after touching its `unsafe`** (the crate is in the
  `-p` list of CI's advisory `miri` job as well, but that job cannot fail a PR, so the local run is
  the one that counts). 6.2's first virtqueue wrote through a pointer taken from a shared borrow:
  every host test passed, and Miri rejected it under both aliasing models. `queue.rs`'s tests are
  what drive `dma.rs` under Miri, so a queue change is a reason to run it as well

### Block drivers

- **A block driver must not depend on the filesystem format.** The `memory` driver's init check is
  device-level: `mkfs-mfs` writes a 32-byte **image header** into block 0's unused boot block (bytes
  `0..1024`, which MFS never reads) plus a 32-byte **tail label** in a reserved last zone, and the
  driver verifies those. Format-level facts are verified by a *client* through a real BDEV round
  trip. `virtio-blk`'s root check is the same three checks applied to the root partition, read
  through its queue, and it does not depend on `minixrs-mfs` either — that independence is what let
  it serve the same image under an unchanged MFS.
- **BDEV parse *and* validation live in `server-rt::bdev`, and no block driver carries its own range
  checks.** MFS's denial battery has to read identically on both backends, so both drivers must
  refuse the same requests with the same errno in the same order, and two copies of the checks would
  drift. Since slice 6.4 the validation is two functions, because *which minors exist* is the one
  thing that differs by driver: `validate_range(req, blocks)` is the length, grant-id, zero-length
  and block-range checks against a block count and never reads the minor; `validate_read(req,
  blocks)` is the minor check for a driver whose only minor is `BDEV_MINOR_ROOT`, followed by
  `validate_range`. `memory` calls `validate_read`. `virtio-blk` **resolves the minor to an extent
  first** (`ENXIO` if it has none) and then calls `validate_range` with *that extent's* block count.
  Either way a request meets the minor check first and the same checks after it. A driver that calls
  `validate_range` without its own minor check has no minor check at all. `validate_geometry` stays
  in `drivers/memory` because it is about the ramdisk window, and `memory` still does not depend on
  `driver-rt`.
- **A partition's first block is added after the range check, by `checked_add`, in one place.**
  `validate_range` answers in blocks relative to the extent; `virtio-blk`'s `Extent::sector` adds
  the extent's first block and refuses a block outside the extent again on its own. A request can
  therefore never reach past its partition into the next one, and the order (resolve the minor,
  validate, then translate) lives in `MinorTable::locate` so no request path can reorder it. The
  driver's main loop reaches it through `part::locate`, which decides the no-root case first.
- **`EIO` is real on the BDEV band and means exactly one thing: the request was well-formed and the
  device failed.** `virtio-blk` answers it for a status byte other than `0` and for a broken device.
  Preset the status byte to a value the device never reports (`0xFF`) before every request, so a
  completion that left it alone reads as a failure rather than as whatever the last request left
  there. Never use `EIO` for a caller's mistake (that is `EINVAL` / `ENXIO`, from the shared
  validation) or for a refused grant or unmapped buffer (relayed verbatim, see
  [Error relay](#error-relay)).
- **Minor 0 is "the root filesystem this driver serves" on every block driver** — `BDEV_MINOR_ROOT`,
  an alias of `BDEV_MINOR_RAMDISK` with the same value. On `memory` it is the whole ramdisk and the
  only minor. On `virtio-blk` it is the GPT partition carrying `rootfs::ROOT_PARTITION_TYPE_GUID` —
  selected **by type, never by position**, and two partitions of that type is a refusal (`root FAIL
  gpt ambiguous`), not first-wins. Minor 1 (`BDEV_MINOR_WHOLE_DISK`) is the whole disk and minor
  `1 + n` is GPT partition *n*, counted from 1 by slot; a partition that does not start and end on a
  4096-byte block boundary has no minor. Every minor is read-write.
- **A driver that has a device but no minix.rs root on it answers `ENXIO` to every BDEV request, on
  every minor**, well-formed or not — a failed bring-up, no acceptable partition table, no root
  partition, a read-only disk, labels that do not match. The whole-disk minor is included: with no
  root there are no minors at all. A zero-length request is included too — it is the request MFS
  probes with, and a `0` there would mount a disk with no root. The gate is `part::locate` called
  with no table: a pure function, so host tests hold it. Keep it there. As a `match` arm in
  `main.rs` it had no test, and no boot marker can cover it, because on the rootless boot MFS's only
  request for another minor goes to `memory`.
- **The partition table is read by the driver and parsed by `driver-rt::gpt`.** The driver reads
  whole-disk blocks 0–4 through its ordinary request path and hands the bytes over — after
  `part::geometry` has refused a device that does not have five whole blocks, so the driver never
  asks a device for sectors past its end and leaves the refusal to it. Only the **primary** header
  and entry array are consulted; a disk whose primary copy is damaged has no root as far as the
  driver is concerned, and repairing from the backup is a tool's job. The writer (`tools/mkimage`)
  shares no code with the parser — a host test in the tool parses a built image with the driver's
  parser, so the two are derived separately and checked against each other.
- **Flush before the reply.** When the device accepted `VIRTIO_BLK_F_FLUSH`, every `BDEV_WRITE`
  whose device write succeeded is followed by a `VIRTIO_BLK_T_FLUSH` — a two-descriptor chain,
  header and status, completed by interrupt like any request — before the client is answered, and a
  failed flush makes the reply `EIO`. No flush follows a zero-length write, a refused grant or a
  failed device write. There is no BDEV flush request: write-through is the driver's, so a client
  that was told a write succeeded needs nothing more. The decision (`blk::flush_after`) and the
  reply (`blk::write_reply`) are pure and host-tested; the call in `main.rs` is evidenced only by
  the `flush ok n=1` marker, and the three "no flush" cases only by reading: each is an early return
  ahead of the call, so `flush_after`'s tests for them exercise arms the driver never reaches.
- **A read-only disk is declined as a root, not mounted.** The driver requests `VIRTIO_BLK_F_RO` so
  a read-only device says so, and answers `root FAIL readonly`; MFS falls back. Nothing above the
  driver can mount a filesystem read-only, and before 6.4 such a disk mounted and failed at its
  first write.
- **A block driver publishes when it has a device and gives its verdict by how it answers** (slice
  6.3, B1, which amends phase decision H5). `vblk` publishes in `init_fresh` if `GET_VIRTIO_MMIO`
  assigned it a device, and only afterwards brings the device up and reads the blocks that say
  whether it carries a root. Publishing after the verdict cannot work: the verdict takes several
  completion interrupts, the driver blocks for each, and MFS — next in packing order — runs
  meanwhile, finds no `vblk`, and mounts the ramdisk. Checking for the root inside `init_fresh`
  blocks before the publish just the same. Published first, the client's SENDREC waits in the kernel
  until the driver reaches its receive loop, which is after the verdict, so nothing races and
  nothing is answered early. The consequence for every client: **a DS entry for a block driver is
  not a root** — ask it.
- **Take the client's bytes before touching the device on a write, and read before overlaying on a
  partial one.** A whole-block `BDEV_WRITE` does its `SAFECOPY_FROM` first, so a bad grant never
  reaches the device. A write of fewer than 4096 bytes replaces the start of the block and must
  leave the rest alone, which `memory` gets from a shorter copy and a sector device only from
  read-modify-write: fetch the block into the bounce buffer, copy the client's bytes over its start,
  write it back. Skip the fetch and the driver stores whatever the bounce buffer last held.
- **Block-to-sector arithmetic is `checked_mul`**, like every other offset computed from a request:
  drivers ship `--release` with `overflow-checks = false`, where `block * 8` on a large block wraps
  and names a valid sector (see [`rust-style.md`](./rust-style.md)).
- That header ABI lives in **`kernel-shared::rootfs`** — mkfs writes it, `memory`, `virtio-blk` and
  MFS read it, and none may depend on the others; `image.rs` re-exports it. `kernel-shared::rootfs`
  also holds the image's *contents* (`ROOTFS_MOTD`, `ROOTFS_PATTERN_*`, `ROOTFS_HELLO_PATH`,
  `rootfs_pattern_byte`), so a read proof is a *check* against the constant rather than a
  transcription of it.
- **The `memory` driver contains no `unsafe` block and never dereferences its mapping**
  (`virtio-blk` cannot say the same: `main.rs` has three `unsafe` blocks — the `driver-rt`
  constructors for its DMA run and its register window, and the one borrow of its static
  partition-table buffer — plus that buffer's `unsafe impl Sync`; its pure modules have none):
  `memory`'s client transfers are `sys_safecopy`, and the boot self-check is `sys_copy(SELF, …,
  SELF, …)`, so a page that failed to map is an `EFAULT` *return value* rather than an EL0 abort —
  hence no MMIO sibling module and no new Sonar exclusion.
- The kernel builds the ramdisk mapping this driver reads; that pre-map and its `.expect()` rule are
  in [kernel.md](./kernel.md) (slice 5.7 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Minors are a per-driver namespace

`/dev/null` and `/dev/zero` are **CDEV minors 3 and 5 of the memory driver** (MINIX 3's
`NULL_DEV`/`ZERO_DEV`) while the same driver's ramdisk is **BDEV minor 0**. The request band tells
them apart, and nothing asserts `CDEV_MINOR_*` against `BDEV_MINOR_*`. BDEV minors are per driver
too: `memory` has minor 0 alone, and `virtio-blk` has the table under
[Block drivers](#block-drivers).

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
- **256 KiB is nearly full for a C program**: `/bin/fileio` (stdio + malloc) is 259,752 B, about 2.3
  KB under the cap. Check `ls -l target/fileio/fileio` before a C program grows (chunk 5).

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

### The root device is chosen by an answer

MFS resolves `vblk` first and `memory` second, and what selects the disk is **not** the DS hit but
the driver's reply to one zero-length `BDEV_READ` of `BDEV_MINOR_ROOT` (why a hit is not enough is
under [Block drivers](#block-drivers)). Four rules for that prologue:

- **Only `0` selects the disk.** `ENXIO` is the designed refusal, but every other reply falls back
  too, a positive count included: a driver that answers a zero-length read with anything else is not
  one to mount a filesystem through. The reply goes into the fallback line verbatim (`bdev.root vblk
  rc=<rc> fallback=memory`), so the log says which it was.
- **The decision is `root::select`, in the library**, with its own host tests — `main.rs` is linted
  by clippy and booted, but no host test, Miri or coverage job sees it (see
  [Crate shape](#crate-shape)), so the prologue only gathers the probe result and prints.
- **Revoke the grant issued to a driver that declined.** The block buffer was granted to `vblk` for
  the probe; on fallback that grant is revoked before `memory` gets its own, so a declined driver
  holds nothing read/write over the buffer for the rest of the boot.
- **Name the selected device on exactly one line** (`bdev.ds ok name=<vblk|memory> ep=<ep>`),
  printed after the probe so it never names a device MFS then declined. The two backends' `[diag
  mfs]` output may differ by that line, by the ramdisk boot's one fallback line, and by the verdict
  of the `bdev.minor` probe below — and by nothing else, which `tools/diff-backends.sh` enforces. So
  a new `[diag mfs]` line must not depend on the backend, and one that has to needs an explicit
  allowance there with self-test cases (slice 6.3 — see
  [the design](../superpowers/specs/2026-11-21-virtio-blk-design.md)).
- **One prologue probe opens a minor other than the root** (`bdev.minor`, slice 6.4): a whole-block
  read of block 0 of `BDEV_MINOR_WHOLE_DISK`, after `bdev.partial` and before the denial battery.
  The filesystem mounts minor 0 and nothing else, so without it `virtio-blk`'s minor table would be
  run by host tests alone. `ok minor=1 mbr=1` means a **protective** MBR came back: `55 AA` at
  offset 510 **and** type `0xEE` at offset 450 **and** no image label at offset 0. The signature
  alone is not enough — the ESP's boot sector ends in the same two bytes, and with the `0xEE` check
  removed a mutation that served the ESP as minor 1 printed a false `ok`. `memory` answers `ENXIO`,
  which prints `absent minor=1 rc=-6` and is the healthy line on the ramdisk boot. The verdict is
  `minor::classify`, in the library, for the same reason `root::select` is.

### Two copies, not one

A `FS_READ` is **two copies** — device → MFS's block buffer → the caller's granted buffer — unlike
the write path's single caller-to-driver copy. A MinixFS read is rarely block-aligned at both ends,
and a *hole* has no device block to copy from at all. This is MINIX 3's own shape (slice 5.8 — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### Capability tokens: the `Blocks` shape

MFS's single block buffer is reached only through the **`Blocks` capability token**, whose
`read(&mut self) -> Result<&[u8; N], i32>` makes "hold a directory block across the next fetch" a
**borrow-check error** rather than a comment. `Blocks` also carries `buf_mut` and `write`, and its
standing grant to the block driver it was built for is **`CPF_READ | CPF_WRITE`** — one buffer, both
directions, one fixed address, so `ensure_registered` still never re-fires.

Widening those flags does **not** widen what any one call may do (the kernel checks the direction
per call), but it *does* re-arm anything that used the pool's grant id as a
deliberately-insufficient grant: a denial probe leaning on the narrow flags needs its own narrower
grant, aimed at a spare block (`START_BLOCK = 2` leaves block 1 free), in the same change — or the
probe writes the block buffer over the superblock (slices 5.8 and 5.10a — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

### What the single block buffer forces

MFS has exactly one 4 KiB block buffer (a `main`-frame local since the stack grew to 64 KiB; `.bss`
before), and that constraint — not taste — fixes `do_write`'s step order: **read the inode, clamp,
compute the grown size, place the zone, read-modify-write the data block, write the inode back.**
Nothing may hold a block across the next fetch, so every intermediate is a `Copy` scalar and
`place_zone` must finish with the buffer free (slice 5.10a — see
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

`do_write`'s create path copies the client's bytes into a second 4 KiB buffer (the `Stage`, a
`main`-frame local like the block buffer) **before anything is allocated**, so no client-controlled
failure can occur after an allocation.

Do **not** "fix" such a path by clearing the bitmap bit on the error arm: for an indirect slot whose
indirect block already existed on disk, the block still names the zone, so freeing the bit hands one
zone to two files — corruption rather than a leak (slice 5.10b — see
[phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).

## Writing probes and denial batteries

A server's probe battery is how these rules stay proved. The rules for writing one:

- **Write every denial probe so that a growing capability makes it fail loudly, not pass
  vacuously.** A probe asserting `EROFS` from `write()` on a file descriptor stopped being a denial
  the moment the write path became real, and silently became a successful overwrite — with every
  other marker still green.
- **Spell an unknown-request probe band-relative** (`VFS_RQ_BASE + NR_VFS_MSGS as i32`), never as a
  literal neighbour of an existing request. `VFS_WRITE + 1` became a real `VFS_OPEN` the moment the
  band grew. Spell a flag probe relative to the known set the same way: `fcntl::O_UNKNOWN_BIT` is
  *derived* from `O_KNOWN`, so a flag becoming real makes the probe fail loudly.
- **Spell a "no such device" probe with `BDEV_MINOR_NONE`, never a small literal minor.** MFS's
  `bad-minor` and `wr-minor` named minor 7, which was nobody's on `memory` and became GPT partition
  6 on `virtio-blk` in slice 6.4 — still `ENXIO`, but only because the boot disk has two partitions:
  the probe's premise had become a fact about one disk image instead of about the driver. The
  constant is `i32::MAX`, and each block driver holds it outside its own minors at compile time
  (`virtio-blk`'s `part.rs`, `server-rt`'s `bdev`), so a table that grows to reach it fails the
  build. The CDEV probes still use a literal 7; they are a different namespace with two fixed
  tables, and owe the same treatment the day one of them grows.
- **Never aim a "must be refused" probe at a file or block whose accidental write would destroy
  something**, and retire a probe visibly — decrement the `n=` in its marker so the retirement is a
  diff rather than a deletion nobody reviews.
- **A C probe sets `errno = 0` before every errno-checked call.** mallocng's ignored `MAP_FIXED`
  guard `mmap` leaves `ENOSYS` behind after the first `malloc`/`fopen` (chunk 5).
- **With the boot stubs on, two copies of one C program can run at once.** A probe that mutates
  filesystem state must be serialised (init skips the `fileio` slot while a `fileio` pid lives) or
  keep its state per pid.
- **A probe of a device path must first put the device's hidden state where a skipped step would
  show.** Slice 6.3's mutation that removed the read from `virtio-blk`'s partial write moved no
  marker and no host test: the filesystem never issues a partial write, and a probe that only wrote
  the tail block's own bytes back and read them again would have passed too, because the bounce
  buffer still held that block from the read before. `bdev.partial` therefore reads **block 0
  first**, leaving a superblock in the bounce buffer, and only then issues the partial write — so a
  skipped fetch stores block 0's bytes in the tail and the read-back sees them. It also *checks*
  that block 0 is non-zero past its header instead of assuming it (`bdev.partial FAIL vacuous`),
  because a priming step that primes nothing makes the probe pass for the wrong reason. The general
  shape: name the state a wrong implementation would reuse (a bounce buffer, a cached block, a
  staging page), make it differ from the right answer, and prove that it does.

Report through the path under test where that is what is being proved: a client's write proof must
be written **through** the descriptor under test (e.g. `/dev/console`, never fd 1), because the
descriptor is the only thing that proves the table row points where it claims (slices 5.8, 5.10a,
5.10b and 5.11 — see [phase-5-musl-fs.md](../plans/phase-5-musl-fs.md)).
