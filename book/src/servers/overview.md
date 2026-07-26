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
`0x800` in slice 5.4; `0x900` and `0xA00` stay reserved for the two bands Phase 5
still owes, BDEV and MFS.

| Base | Value | Server / purpose |
|------|-------|------------------|
| `PM_RQ_BASE`    | `0x700` | PM: `PM_GETPID` / `FORK` / `EXIT` / `WAIT` / `EXEC` |
| `VFS_RQ_BASE`   | `0x800` | VFS: `VFS_WRITE` |
| `CDEV_RQ_BASE`  | `0xB00` | Character drivers: `CDEV_WRITE` (TTY) |
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

## VFS: the write path

**VFS** (`servers/vfs/`) turns a small integer into something you can write to.
That is the whole of its job in Phase 5 so far, and since slice 5.4 it does it for
real: an ordinary user process can `write(1, buf, len)` and see bytes on the
console. Mount points, `open`, and reads arrive with the MFS server in slice 5.8.

### One request, one copy

```text
user ──VFS_WRITE{fd,buf,len}──► VFS ──CDEV_WRITE{minor,gid,len,off}──► TTY
                                 │                                      │
                                 └── magic grant: caller's buf ──────────┘
                                         (kernel copies, once)
```

VFS resolves the descriptor, issues a **magic** (third-party) grant naming the
*caller's* buffer with the driver as grantee, and forwards the grant id. TTY then
safecopies straight out of the caller's address space. The bytes never pass
through VFS: there is exactly one copy, from the process that wrote them to the
driver that transmits them. This is the first consumer of the magic grant form on
a real data path, and the rail slice 5.6's musl `write()` lands on.

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
  out.
- **Every request gets a reply**, including an unknown one (`ENOSYS`). VFS's
  clients are all inside a SENDREC, so a dropped message blocks the caller forever.

### The descriptor table

`servers/vfs/src/fd.rs` holds one row of descriptors per process, indexed by
kernel proc number and sized from the shared `NR_SERVED_PROCS` ceiling that PM's
`mproc` and VM's `ClientRegions` also derive from. Nothing opens or closes yet, so
every row is identical — fds 0, 1, and 2 name the console, everything else is
`EBADF` — which is POSIX's inheritance convention and is what lets init write
before any filesystem exists. Because nothing mutates, the storage is an ordinary
immutable `static` and VFS carries no `unsafe`; slice 5.8's `open` flips it to the
`UnsafeCell` newtype that VM's region table already uses.

VFS also remains the system's first *grant* client and first *console* client:
its startup still direct-grants a read-only buffer to PM (slice 5.2) and drives
`CDEV_WRITE` by hand (slice 5.3). Those are kept deliberately, as the regression
battery for three contracts the real write path never reaches — the direct-grant
form, a *visible* short write, and the two `CDEV_WRITE` refusals a well-formed
`write()` cannot provoke.

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

init is a plain `minix-ipc` program — no SEF, because it is not a server. The rest
of its body is a respawn loop: `PM_FORK`; the child (`m_type == 0`) issues
`PM_EXEC` to become the `worker` binary, which runs a few `PM_GETPID` round-trips
and exits; the parent (`m_type > 0`) issues `PM_WAIT` to reap the zombie, then
loops. Each cycle recycles a fork-pool slot with a fresh endpoint generation —
observable in the boot trace as repeating `SYS_FORK` → `SYS_EXEC name=worker` →
`SYS_EXIT` triples, the proof that fork, exec, teardown, and reap all compose.

The demo stubs A–D remain installed alongside init as a live regression battery:
A↔B exercise the raw SEND/RECEIVE/SENDREC primitives, C exercises the
kernel→SCHED quantum-delegation round-trip, and D exercises the page-fault→VM path
and the out-of-region SIGSEGV kill — coverage that init and worker, which only
fork/exec/wait/getpid, do not provide.
