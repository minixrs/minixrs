# Servers

minix.rs keeps the microkernel tiny — IPC, scheduling, memory protection, and a
small set of privileged kernel calls — and runs every operating-system *service*
as an ordinary user-space process, exactly as MINIX 3 does. These **servers**
talk to each other and to the kernel only through message passing. A server never
shares memory with a client; it acts on a request, replies, and the kernel
enforces who may talk to whom via per-process privilege bitmaps.

This chapter describes the servers as they stand at the end of Phase 4: a common
runtime (SEF), a name registry (DS), a user-space scheduler (SCHED), a monitor
(RS), the process manager (PM), a still-skeletal file-system switch (VFS), and
`init` (PID 1) — the first real user process, which drives the whole
fork/exec/wait lifecycle through PM.

## Where servers live

Servers are freestanding `#![no_std]` / `#![no_main]` ELF binaries linked with
their own `user.ld` (page-aligned segments based at `0x0010_0000`) and branded
with the minixrs ELF identity note, which the kernel requires of every image it
loads. The kernel's `build.rs` compiles each for the `aarch64-unknown-minixrs`
target and concatenates them into a single **MXBI archive** embedded in the
kernel image; the boot loader
(`kernel/src/arch/aarch64/userland.rs`) walks the archive and loads each module
into the proc slot named by its record. Each server gets its own per-process
TTBR0, so they all share the same low load base with no collision.

A server has no `println`. Its behaviour is observed through kernel-side traces
(`[ipc]`, `[ksys]`, `[pf]`, `[alarm]`), and since slice 5.1 it can also emit a
line itself through the kernel debug channel — `server-rt`'s `diag_print` /
`diag_fmt` issue a `SYS_DIAGCTL` carrying the text inline, which the kernel prints
prefixed with the caller's own name (`[diag vfs] …`). That is deliberately a
*debug* channel, not stdio: it exists to keep working while stdio itself is under
construction. Real console output arrives via the TTY driver (see
[Drivers](../drivers/overview.md)) — slice 5.3 put the first EL0-composed text on
the serial line, and slice 5.4 puts a process's fd 1 and 2 on top of it.

## Request-number ranges

Every server's request numbers occupy a distinct band below `NOTIFY_MESSAGE`, so
a message type unambiguously identifies both its server and its meaning
(`kernel-shared/src/callnr.rs`, const-asserted disjoint). The bands are listed —
and rendered into the generated C header — in ascending numeric order. VFS took
`0x800` in slice 5.4, BDEV `0xA00` in 5.7, and the VFS↔FS band `0x900` in 5.8 —
which fills `0x700..0xC00` completely. A tenth band has no reserved slot left to
take; it has to find a home outside that span.

| Base | Value | Server / purpose |
|------|-------|------------------|
| `PM_RQ_BASE`    | `0x700` | PM: `PM_GETPID` / `FORK` / `EXIT` / `WAIT` / `EXEC` |
| `VFS_RQ_BASE`   | `0x800` | VFS: `VFS_WRITE` / `OPEN` / `READ` / `CLOSE` / `EXEC_STAGE` |
| `FS_RQ_BASE`    | `0x900` | File systems: `FS_READSUPER` / `LOOKUP` / `READ` / `WRITE` / `CREATE` / `TRUNC` (MFS) |
| `BDEV_RQ_BASE`  | `0xA00` | Block drivers: `BDEV_READ` / `BDEV_WRITE` (`memory`) |
| `CDEV_RQ_BASE`  | `0xB00` | Character drivers: `CDEV_WRITE` (TTY, memory driver) / `CDEV_READ` (memory driver; TTY answers `ENOSYS` until Phase 6) |
| `VM_RQ_BASE`    | `0xC00` | VM: `VM_PAGEFAULT` / `BRK` / `MMAP` / `MUNMAP` / `FORK` |
| `SEF_RQ_BASE`   | `0xD00` | SEF control messages (ping / signal / init) |
| `DS_RQ_BASE`    | `0xE00` | DS: `DS_PUBLISH` / `RETRIEVE` / `CHECK` |
| `SCHED_RQ_BASE` | `0xF00` | SCHED: `SCHEDULING_NO_QUANTUM` / `START` / `STOP` / `SET_NICE` |

## SEF: the server runtime

`server-rt` is minix.rs's small equivalent of MINIX 3's SEF (System Event
Framework). A server calls `sef_startup(SefConfig { init_fresh, signal_handler })`,
which learns the server's own endpoint and name from the kernel via
`SYS_GETINFO(GET_WHOAMI)`, runs the optional `init_fresh` callback, and returns a
`Sef` handle. The main loop is then `loop { if sef.receive(&mut msg) != 0 { continue } match msg.m_type { … } }`:
`sef.receive` wraps `ipc_receive(ANY, …)` and transparently handles SEF control
traffic — an RS heartbeat ping, a `SEF_SIGNAL` from PM/RS, a `SEF_INIT` — so the
server only sees genuine application messages.

The classifier (`server-rt/src/classify.rs`, host-tested) gates each control
event on the message's *source*, not its type alone: an RS ping is only honored
from RS, a signal only from a signal manager, an init only from RS. A client
holding a mere `ipc_to` bit to the server cannot spoof one. `server-rt` is
`#![forbid(unsafe_code)]` — callbacks travel in the config struct, not global
state. The `init_fresh` body most servers use is the shared
`sef_publish_to_ds(endpoint, name)` helper, which registers the server in DS.

## DS: the name registry

Servers discover each other by name through **DS** (`servers/ds/`), a
name→endpoint registry backed by a static `[Entry; 16]` table
(`servers/ds/src/registry.rs`; the pure `publish` / `retrieve` / `check` helpers
are host-tested). A `DS_PUBLISH` request carries a 16-byte NUL-padded name in
payload `0..16` and the publisher's endpoint in `16..20`. DS is the one server
that *cannot* publish to itself over IPC — a SENDREC to itself before reaching
its receive loop would deadlock — so it seeds its own entry in-process during
`ds_init`.

## SCHED: user-space scheduling

The kernel scheduler is **delegatable** rather than replaced. Each `Proc` carries
a `scheduler` endpoint; `NONE` (the boot default) means kernel-scheduled — the
kernel refills the quantum and rotates the run queue. A non-`NONE` value means the
process is scheduled by a user-space server: on quantum exhaustion the kernel
dequeues it, leaves `RTS_NO_QUANTUM` set, and sends `SCHEDULING_NO_QUANTUM` to its
scheduler, which decides when to re-admit it via `SYS_SCHEDULE`.

**SCHED** (`servers/sched/`) is that scheduler. It claims a target with
`SYS_SCHEDCTL` (setting `scheduler = SCHED`), tracks it in a static
`[SchedProc; 16]` policy table (`servers/sched/src/policy.rs`, host-tested), and
on each `SCHEDULING_NO_QUANTUM` refreshes the quantum at a fixed managed band
(`USER_Q = 8`, the boot-server band, so a CPU-bound managed process round-robins
instead of starving behind kernel-scheduled work). SCHED itself and the kernel
tasks stay `NONE` — a scheduler must not schedule itself. `SCHEDULING_START` /
`STOP` are the hooks PM drives during fork and exit; MINIX-style priority aging is
left for later.

## RS: the reincarnation server

**RS** (`servers/rs/`) is the system-process monitor and the root of the boot
process tree. It arms a periodic one-shot alarm (`SYS_SETALARM`, `ALARM_PERIOD =
100` ticks) and on each fire pings a fixed peer set (DS/VM/SCHED/VFS/PM) with
`ipc_notify`, tallying acknowledgements in a host-tested monitor
(`servers/rs/src/monitor.rs`). Peers acknowledge through the ordinary SEF ping
path, so no extra wiring is needed. In Phase 4 restart-on-crash is detect-only —
RS counts unresponsive peers but cannot yet re-exec them (exec of a fresh service
image is future work). The alarm expiry arrives as a kernel-originated `NOTIFY`
from `CLOCK`, which RS distinguishes from its own SEF ping by keying on
`m_source == boot_endpoint(CLOCK)`.

## PM: the process manager

**PM** (`servers/pm/`) owns the POSIX process lifecycle. Its `mproc` table
(`servers/pm/src/mproc.rs`, host-tested) records one entry per process — pid,
parent, a generation-aware endpoint, and flags. Boot servers and the demo stubs
are seeded at init; forked children are allocated from a pool
(`[FORK_POOL_BASE, NR_MPROCS)`) where a slot's index is also the child's kernel
proc number.

User processes drive their whole lifecycle through PM — the POSIX shape, *user →
server, never user → kernel* (the shared user privilege opens `ipc_to` edges to PM
and VFS, and nothing else):

- **`PM_GETPID`** replies with the caller's pid (`m_type` *is* the pid, MINIX
  result-is-pid), parent pid in the payload.
- **`PM_FORK`** builds a child in a fixed, safety-critical order: allocate the
  `mproc` slot, `SYS_FORK` (the kernel clones a *frozen* child — `RTS_RECEIVING |
  RTS_NO_PRIV`), `VM_FORK` (VM copies the parent's regions), `SCHEDULING_START`,
  then `SYS_PRIVCTL(PRIVCTL_SET_USER)` to release the freeze — and finally replies
  to *both* halves of the shared SENDREC (child sees `0`, parent sees the child
  pid: fork returns twice). Only PM's reply clears `RTS_RECEIVING`, so the child
  cannot run before its identity, memory, and scheduling are fully built. Any
  mid-fork failure rolls back every completed step.
- **`PM_EXEC`** issues `SYS_EXEC` naming the caller as the target; the kernel
  replaces the caller's image with a boot-embedded binary and resumes it at the
  new entry (no reply on success). Phase 4 hardcodes the target as `worker`; a
  user-supplied path arrives with the Phase-5 filesystem.
- **`PM_EXIT`** does `SCHEDULING_STOP` then `SYS_EXIT` (full teardown: address
  space freed, endpoint generation bumped, slot freed) and marks the `mproc` slot
  a zombie holding the encoded status; the dead child gets no reply.
- **`PM_WAIT`** reaps a zombie child (reply pid + status, free the slot) or, if a
  live child exists, suspends the parent until the child's exit wakes it. There is
  no async `SIGCHLD` in Phase 4 — the zombie + wait-reap handshake is the only
  parent notification, because the kernel signal path default-*terminates* and
  would kill a handler-less parent.

### Minimal signals

PM is also the signal manager for user processes. The kernel half is a small trio
(`SYS_KILL` / `SYS_GETKSIG` / `SYS_ENDKSIG`): `SYS_KILL` records a bit in the
target's `Proc::sig_pending`, sets `RTS_SIGNALED | RTS_SIG_PENDING`, and wakes PM
with a kernel-originated `NOTIFY`. PM drains pending signals with `SYS_GETKSIG` and
disposes of each — `SYS_ENDKSIG` to acknowledge a survivor, or `SYS_EXIT` to
terminate. Handlers (catching, `sigaction`) are Phase 5; Phase 4's default action
for a user process is termination.

## VFS: the write, read, and exec-staging paths

**VFS** (`servers/vfs/`) turns a small integer into something you can read from or
write to. Since slice 5.4 it does the writing for real — an ordinary user process
can `write(1, buf, len)` and see bytes on the console — and since 5.8 it does the
reading too, against a real filesystem served by MFS.

### One request, one copy

```text
user ──VFS_WRITE{fd,buf,len}──► VFS ──CDEV_WRITE{minor,gid,len,off}──► driver
                                 │                                      │
                                 └── magic grant: caller's buf ──────────┘
                                         (kernel copies, once)
```

VFS resolves the descriptor, issues a **magic** (third-party) grant naming the
*caller's* buffer with the driver as grantee, and forwards the grant id. TTY
safecopies straight out of the caller's address space; the memory driver's
`/dev/null` and `/dev/zero` writes issue no `SYS_SAFECOPY` at all — they discard
the bytes and reply the whole count with no copy. Either way the bytes never
pass through VFS: at most one copy happens, from the process that wrote them to
the driver that transmits them. This is the first consumer of the magic grant
form on a real data path, and the rail slice 5.6's musl `write()` lands on.

Three properties hold that path together:

- **The grant's owner is the kernel-stamped `m_source`.** VFS holds `SYS_PROC`,
  which is what makes a magic grant legal for it at all — so a caller-supplied
  owner field would let any VFS client aim a privileged cross-address-space copy
  at a third party's memory. `VFS_WRITE` has no such field, and must never gain
  one. It is the same anti-confused-deputy rule that keeps a granter out of the
  `CDEV_WRITE` payload, applied to the granting side.
- **VFS absorbs short writes.** A character driver may move fewer bytes than asked
  (`CDEV_MAX_IO`, its staging limit); POSIX `write()` is not allowed to expose
  that, so VFS re-sends with `offset` advanced until the buffer is out and reports
  the total. One grant covers the whole buffer — only the offset moves. An error
  after partial progress reports the *progress*, since those bytes really did go
  out. The file-backed route slice 5.10a added loops on the same rules but grants
  *afresh* each round, because the FS band deliberately has no grant-offset field:
  there, the grant is what moves.
- **Every request gets a reply**, including an unknown one (`ENOSYS`). VFS's
  clients are all inside a SENDREC, so a dropped message blocks the caller forever.

### The descriptor table

`servers/vfs/src/fd.rs` holds one row of descriptors per process, indexed by
kernel proc number and sized from the shared `NR_SERVED_PROCS` ceiling that PM's
`mproc` and VM's `ClientRegions` also derive from. Every row *starts* identical —
fds 0, 1, and 2 name the console, everything else is `EBADF` — which is POSIX's
inheritance convention and is what lets init write before any filesystem exists.

Since slice 5.11 a character-device entry names its **driver** as well as its
minor (`Fd::CharDev { dev: CharDriver, minor }`, with `CharDriver` an enum
because the default row is a `const` and a DS-resolved endpoint is not) — minors
are a per-driver namespace, so the driver is half the address. `open` consults a
three-row **device-node table** (`servers/vfs/src/dev.rs`: `/dev/console`,
`/dev/null`, `/dev/zero`, matched byte-for-byte) after copying the path in and
*before* touching the mount, so a device open needs no filesystem; `O_CREAT` and
`O_TRUNC` are ignored on a hit, Linux's behaviour for a device node. Everything
else falls through to MFS, `/dev/other` included, and there is no `/dev` on the
image at all. A device `read()` is one `CDEV_READ` against the descriptor's
driver, no loop and no position; a console `read()` therefore reaches TTY and
hears `ENOSYS` from its unknown-request arm until Phase 6.

Slice 5.8's `open` is what makes rows diverge, and it moved the storage to the
`UnsafeCell` newtype VM's region table already uses. That brings a rule with it:
**never hold a borrow of the table across a SENDREC**. `Fd` is `Copy` precisely so
that is easy to obey — a resolve's borrow dies at the destructuring `let`, and the
handler carries values into the round trip.

`open` hands out the **lowest free descriptor**, POSIX's rule and the only thing
about it a client can observe without reading the file: close one and the next
`open` reuses that number. `close` frees the slot and sends the filesystem
nothing, because MFS keeps no per-open state — which is also why the FS band has
no `PUTNODE`.

### The read path, and its two copies

```text
user ──VFS_OPEN{path,len,flags}──► VFS ──SYS_COPY──────────► (the path, into VFS)
                                    │
                                    └──FS_LOOKUP{path}─────► MFS  → (ino, mode)
                                       (or FS_CREATE / FS_TRUNC, below)

user ──VFS_READ{fd,buf,len}──► VFS ──FS_READ{ino,gid,len,pos}──► MFS
                                │                                 │
                                └── magic grant: caller's buf ────┘
```

Two copies, not one, and the difference from the write path is deliberate: MFS
stages a block through its own buffer before safecopying the requested slice out
of it. A MinixFS read is rarely block-aligned in both the file and the
destination, and a *hole* has no device block to copy from at all — so the staging
cannot be elided. This is MINIX 3's own shape. Only the second copy is VFS's
grant; the bytes still never pass through VFS.

**`open` is not lookup-only.** Slice 5.10b gave `VFS_OPEN` a third payload field,
`flags` (`VFS_FLAGS_OFF`, i32), read straight from the same `kernel-shared/fcntl`
that musl's `open()` fills in. A plain lookup answering `ENOENT` is no longer the
end of the story: `O_CREAT` on a missing name dispatches `FS_CREATE` instead, and
`O_TRUNC` on an existing regular file dispatches `FS_TRUNC` after the lookup
succeeds. `O_CREAT | O_TRUNC` on a missing name takes the create arm and stops
there — a fresh file is already empty.

**The descriptor is allocated before that truncate runs, and handed back if it
fails.** A full descriptor table is `EMFILE`, and a caller whose `open` failed
has no reason to believe anything changed — so emptying the file first and *then*
refusing the descriptor would destroy its contents behind a failure. Linux orders
it the same way. The converse still holds too: a truncate that fails closes the
descriptor before returning, so nobody is ever left holding one onto a
half-truncated file. The access-mode bits (`O_RDONLY`/`O_WRONLY`/`O_RDWR`) are accepted and
ignored — there is no uid, gid, or permission check anywhere in the tree, so
honouring them would be a check with nothing behind it. A flag bit outside
`O_KNOWN` is `EINVAL`, and that comparison is written **against `O_KNOWN`**
rather than as a literal mask, so a future flag becoming real fails a stale denial
probe loudly instead of letting it pass vacuously — the same lesson slice 5.8's
`VFS_WRITE + 1` probe taught the hard way.

Two more properties, each with its own boot marker:

- **`SYS_COPY` reads the path, and its source is the kernel-stamped `m_source`.**
  This is the first live consumer of decision D4's "`SYS_COPY` for small
  control-plane reads" sentence, and the confused-deputy rule in its sharpest
  form: `SYS_COPY` has *no per-target authorization whatsoever* — the caller's
  `k_call_mask` bit is the whole check — so a payload-supplied source process
  would let any client read any process's memory through VFS.
- **VFS does not loop on read.** It loops on `write` because a driver's staging
  limit may not reach `write()`'s return value; `read()` is explicitly allowed to
  return less than asked for, and a file read is short at EOF regardless. EOF is a
  read returning `0` — no file's size is cached anywhere along the path, so that
  is the single source of truth.

### Staging an executable

Slice 5.9 gave VFS one more request, and it is the only one that reads a *whole*
file:

```text
PM ──VFS_EXEC_STAGE{path}──► VFS ──FS_LOOKUP──► MFS   → (ino, mode, size)
                              │
                              ├──FS_READ × N──► MFS   → bytes into EXEC_STAGE
                              │
                              └── direct grant over EXEC_STAGE (CPF_READ) ──► PM
```

PM hands that grant to `SYS_EXEC`, and the **kernel** reads the ELF through it —
so the bytes pass through neither PM nor the kernel's own memory, and the kernel
gains no filesystem (decision D6). Four things are worth stating:

- **The path travels inline**, unlike `VFS_OPEN`'s pointer-and-length. The client
  is PM, which already holds the path inline in the `PM_EXEC` it is serving, so
  passing it by value costs no `SYS_COPY` — and it deletes the confused-deputy
  question outright, because there is no source process for a caller to misname.
- **Only PM may ask.** Any other `m_source` is `EPERM`, and init's denial battery
  is the only thing that exercises that guard.
- **A short stream is `EIO`, not a short stage.** Everywhere else in VFS a partial
  transfer is a legitimate answer; here it is not, because an ELF cannot be loaded
  in pieces by a loader with no filesystem.
- **The staging buffer is a 256 KiB `.bss` static**, for MFS's block-buffer
  reason: a server's stack is one page, so a local would fault into VM's SIGSEGV
  arm, which prints nothing the forbidden-marker list catches. Unlike MFS's block
  buffer it needs no capability token and no borrow discipline — **VFS never
  dereferences the staged bytes**. MFS writes into them by safecopy and the kernel
  reads them through the grant; VFS only ever needs the address.

Nothing releases the grant afterwards and nothing needs to: each request re-grants
the same buffer, which bumps the sequence and kills the previous id, and PM
serialises exec so two staged images are never alive at once.

VFS also remains the system's first *grant* client and first *console* client:
its startup still direct-grants a read-only buffer to PM (slice 5.2) and drives
`CDEV_WRITE` by hand (slice 5.3). Those are kept deliberately, as the regression
battery for three contracts the real write path never reaches — the direct-grant
form, a *visible* short write, and the two `CDEV_WRITE` refusals a well-formed
`write()` cannot provoke. Slice 5.7's block-device demo, by contrast, is **gone**:
MFS is the real BDEV client now, so the battery moved there and VFS is back to
knowing nothing about block devices.

## MFS: the file system

**MFS** (`fs/mfs/`) is the first file system in minix.rs — read-only as of slice
5.8, writable as of 5.10a, and able to create and truncate files as of 5.10b. It
sits between VFS and a block driver, and it is the piece that makes a path
resolve to bytes: VFS asks `FS_LOOKUP` for an inode, `FS_READ` for its contents,
`FS_WRITE` to replace them, `FS_CREATE` to name a new one, and `FS_TRUNC` to
discard one's contents, and MFS answers by moving blocks to and from the
`memory` ramdisk over BDEV, decoding them with the `minixrs-mfs` format library
(`superblock`, `inode`, `layout`, `dirent`, `read`, 5.10a's `write`, and 5.10b's
allocator) that slice 5.7 began and host-tested. The image lives in RAM, so a
write survives until the machine stops — long enough to be read back and proved,
not long enough to be persistence.

The crate is split unusually hard. Its `[[bin]]` carries
`required-features = ["server"]` so the format library stays a one-dependency
crate the kernel's build script can use for free — and the price is that **the
binary is invisible to every CI job except the QEMU boot smoke test**. So every
line with a decision in it lives in the library (`proto.rs` for the wire codec,
`walk.rs` for traversal and read policy), and `main.rs` is SEF/IPC/grant glue.

Three things characterise the server itself:

- **One 4 KiB block buffer, in `.bss`.** A boot server's stack is exactly one page
  and a block is exactly one page, so the buffer cannot be a local — the frame
  base would land below the mapping, and VM turns that fault into a SIGSEGV that
  prints nothing the forbidden-marker list catches. It is reached only through a
  `Blocks` capability token whose `read(&mut self) -> &[u8; N]` makes "hold a
  directory block across the next fetch" a *borrow-check error* rather than a
  promise. Every intermediate the walk needs is a small `Copy` value.
- **Streaming, not buffering.** `tools/mkfs-mfs`'s `verify.rs` is the reference
  implementation of the same reader, but it materializes a whole directory into a
  `Vec`; MFS asks about one block at a time and keeps nothing but a `u32`. The
  `fs.selfcheck` boot marker is the one place the two readers meet over a real
  image.
- **Degraded, never fatal.** Past `sef_startup` nothing panics and nothing spins:
  a failed mount answers `ENODEV` to every request, and every device-derived loop
  bound has a cap, because a corrupt inode claiming `size = i32::MAX` would
  otherwise spin MFS — which would block VFS, which would block init.

Two error-relay rules sit side by side and read as contradictory. A failed
`BDEV_READ` becomes `EIO`, because MFS's client addressed a *file* and the device
beneath it is an implementation detail. A failed `SYS_SAFECOPY` against VFS's
grant is relayed **verbatim**, because `EPERM` ("your grant does not authorize
this") and `EFAULT` ("your buffer is not mapped") are different bugs on the
caller's side.

### The write path

`FS_WRITE` (slice 5.10a) is `FS_READ`'s payload field for field — inode, grant,
length, position — because it is the same question asked in the other direction,
and one wire codec and one clamp serve both. The reply `m_type` is the byte count
stored. Nothing in the payload says which direction it is; the request number
does, and the dispatch arm is the only place that needs to know. The grant is the
one thing that must differ, and it differs by direction: an `FS_READ`'s carries
`CPF_WRITE` because the copy lands in the client's buffer, an `FS_WRITE`'s carries
`CPF_READ` because the copy is taken out of it. Neither server checks that — the
kernel's `verify_grant` does, and no server re-implements it.

**A short write is normal here, not an error.** MFS clamps every request to the
end of the block containing `pos`, so one call moves at most a block and usually
less. That is `CDEV_WRITE`'s stance and deliberately not `BDEV_READ`'s
refuse-or-nothing, and the two are consistent once you ask who the client is: BDEV
refuses because its client is a filesystem, which cannot interpret a fraction of a
block, while this request's client is VFS, whose whole job is hiding staging from
POSIX. So VFS loops, one fresh grant per round.

Writing where no zone exists means allocating one, and **the order is the part
worth remembering: the bitmap bit is made durable before the zone number is stored
anywhere** — an inode's `zone[i]`, an indirect block's slot, either. A failure
between the two therefore leaks a zone rather than letting two files share one,
and the asymmetry is the whole argument: a leak is unreachable space some future
`fsck` can reclaim, while a shared zone is silent corruption on a filesystem that
has no `fsck` to notice it. The alternative — rolling the bit back on the error
path — is worse in exactly the direction that matters, because a rollback that
itself fails hands the same zone out twice; worse still, an indirect slot whose
indirect block *already existed* has the zone durably referenced by that block
the instant the bit is set, so clearing the bit again on any later failure would
hand the same zone to two files rather than merely leaking it. A freshly
allocated zone is also *zeroed* before anything can reach it, which is what makes
a new **indirect** block safe to read: all 1024 of its slots come back as holes
rather than as whatever the previous owner left there, which this code would read
as zone pointers.

**Slice 5.10b closes the one gap that ordering alone didn't cover.** Through
5.10a, `do_write` allocated a zone and only *then* copied the client's bytes out
of its grant — and that copy could still fail on the client's own account, if the
buffer it granted was unmapped. Looping `write()` against such a buffer leaked
one zone per call (each `EFAULT`, none rolled back, for the reason above), which
exhausted the image's free zones in under 200 calls and left every write after
that — including a legitimate one — answering `ENOSPC` for the rest of the boot: a
reachable denial of service, not a benign leak. The fix is a second `.bss`
staging buffer (`Stage`) that `do_write` fills from the client's grant **before**
anything is allocated, so no client-controlled failure can occur after an
allocation. Note what this is not: it is a *restaging*, not a rollback — the
corruption case above (an indirect slot's already-existing block) is exactly why
rolling back was never the right fix. The `fs.leak` boot probe proves the closure
directly: 256 writes aimed at an unmapped buffer must all answer `EFAULT` and
allocate nothing, and a real write must still succeed afterwards.

The write path checks zone numbers against a **lower bound the read path
deliberately lacks** (`write_zone_ok`, requiring `zone >= first_data_zone`). The
reader can afford to be loose, because the worst a corrupt pointer costs it is the
wrong bytes. A *write* to the same pointer destroys what it is aimed at: an inode
whose `zone[i]` reads back as `3` would have MFS store a data block straight over
the zone bitmap, and an indirect pointer of `4` would have it patch and store a
block of the inode table. Nothing the allocator hands out can be that low — but a
zone number read off the device is whatever the device says.

One field is deliberately left alone: `mtime` and `ctime` are not updated, because
there is no clock a user-space filesystem can read yet. A written file keeps the
timestamps `tools/mkfs-mfs` stamped into it, on the grounds that an obviously
stale timestamp is better than an invented one — and this becomes a real field to
fill the moment a clock is reachable.

### Create, truncate, and directory growth

Slice 5.10b gave the FS band two more requests. `FS_CREATE` reuses `FS_LOOKUP`'s
wire codec **verbatim** — same request shape, same reply shape, one parser and
one classifier for both — because a create is a path operation exactly like a
lookup, just one that is allowed to make something exist. `FS_TRUNC` carries only
an inode number and discards a regular file's contents down to zero, with no
length field: `O_TRUNC` is the only client anywhere in the tree, and there is no
`ftruncate()` to serve.

**Both new requests mirror the write path's leak-over-corruption ordering, in the
opposite order from each other, on purpose.** `create` allocates the inode and
writes it back *before* the directory entry names it — a failure in between
orphans an inode (a leak, reclaimable by a future `fsck`) rather than leaving a
directory entry pointing at an inode that was never written. **And nothing
reaches that failure**, because `create` extends `do_write`'s "no
client-controlled failure after an allocation" rule to its own path:
`reserve_slot` places the directory's slot — including the zone its growth may
need, the only `ENOSPC` a client can provoke here — *before* `alloc_inode` claims
anything. A reservation that fails has allocated nothing; one that succeeds
leaves the directory one legitimately grown block larger, which is not a leak
because the parent inode names that zone. Without that ordering the path would be
the 5.10a denial of service one step later: each failure would burn one of the
image's 128 inodes for good, and unlike a leaked zone — which `do_trunc` hands
back — no amount of truncating recovers an orphaned inode. `do_trunc` writes
the zeroed inode back *before* freeing the zones it used to hold — a failure in
between merely fails to reclaim some zones, where the reverse order could leave a
live inode still naming zones the allocator has already handed to someone else.
**That truncate ordering has no boot probe, and the honest thing is to say so
rather than let a passing boot imply otherwise**: proving it needs a failure
between the inode write-back and the zone free that nothing this slice can send
induces — the same class of gap slice 5.10a documented for the `dirty` half of
the write-back condition, recorded here rather than repeated by omission.

**A directory grows through the same allocator a file's data does.**
`find_free_slot` tries every existing block first — and it does not stop at the
first free slot it finds, because a name occupying a *later* block would
otherwise get shadowed by a duplicate inserted ahead of it; `Occupied` has to win
over `Free` across the *whole* scan, not just within one block. Only when no
block has room does `reserve_slot` append one, and it does that through
`place_zone`, the exact function `do_write` uses for file data — so directory
growth costs no second code path and inherits the bitmap-before-pointer ordering
already proved for files. The image ships `/full`, a directory with `.` and `..`
plus 62 empty files — exactly 64 entries, one block — so that a single boot-time
create is guaranteed to take the append arm; no other probe reaches it.
`/etc/holey` plays
the same role for the write-back condition's other half: its first block is a
hole, so writing into it assigns a zone pointer with the file's size unchanged,
which is the one case a size-only write-back condition would silently drop.

`FS_CREATE` on a name that already exists is `EEXIST`, checked by re-resolving
the name afterwards and confirming the inode number **did not change** — the
errno alone would not catch a dropped guard that shadowed the original entry with
a second one.

## init: PID 1

**init** (`userland/init/`) is the first real user process and the live exercise
for everything above. Unlike the demo stubs it replaced, it is a genuine boot
module: `build.rs` packs it into the MXBI archive with its true proc number
(`INIT_PROC_NR = 10`), and the ordinary boot loop loads it and makes it runnable —
PM does not hand-release it. It runs at user grade, sharing the `USER_PRIV_ID`
privilege (SENDREC to PM and VFS, no kernel calls) with every forked child.

Since slice 5.4 it also speaks. Before the respawn loop it writes to fd 1 and fd 2
through VFS, which is the whole POSIX write path exercised from the one place that
proves it matters: a process with no kernel calls, no grant table, and no debug
channel of any kind. That last part is deliberate — `write()` is init's *only* way
to say anything, so it reports on the path under test through the path under test,
and a regression takes the evidence with it. It prints a banner, a line longer than
one `CDEV_WRITE` can carry (whose tail marker only appears if VFS looped, and whose
returned count init checks against what it asked for), and four probes that must
each be refused: a closed descriptor (`EBADF`), an unknown request number
(`ENOSYS` — and the *reply* is the assertion, since a dropped request would hang
init and the boot with it), an unmapped buffer (`EFAULT`, from the kernel's
page-table walk, which costs init no page fault because the copy engine walks
rather than dereferences), and a negative length (`EINVAL`).

init is a plain `minixrs-ipc` program — no SEF, because it is not a server. The rest
of its body is a respawn loop: `PM_FORK`; the child (`m_type == 0`) issues
`PM_EXEC` naming the binary it wants to become; the parent (`m_type > 0`) issues
`PM_WAIT` to reap the zombie, then loops. Each cycle recycles a fork-pool slot
with a fresh endpoint generation — observable in the boot trace as repeating
`SYS_FORK` → `SYS_EXEC` → `SYS_EXIT` triples, the proof that fork, exec,
teardown, and reap all compose.

Since slice 5.6 the exec target is the *caller's* choice — `PM_EXEC` carries a
name, rather than PM hardcoding one — and init **alternates** between two
binaries, so the trace shows `name=worker` and `name=hello` on successive
cycles:

- **`worker`** is slice 5.5's exec-ABI probe. It validates the SysV initial
  stack against its own `sp` and reports the verdict as its exit status, which
  init prints once (keyed on the child's *pid*, because PM parents the demo
  stubs to init and stub D's deliberate SIGSEGV would otherwise be the first
  thing reaped).
- **`hello`** is slice 5.6's C milestone, linked against the musl fork.

Alternating rather than switching is deliberate: retiring `worker` to make room
for `hello` would have taken the exec-ABI proof down with it. See
[C Library & musl Port](../libc/overview.md).

The demo stubs A–D remain installed alongside init as a live regression battery:
A↔B exercise the raw SEND/RECEIVE/SENDREC primitives, C exercises the
kernel→SCHED quantum-delegation round-trip, and D exercises the page-fault→VM path
and the out-of-region SIGSEGV kill — coverage that init and worker, which only
fork/exec/wait/getpid, do not provide.
