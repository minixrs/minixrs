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
their own `user.ld` (page-aligned segments based at `0x0010_0000`). The kernel's
`build.rs` compiles each into an isolated target directory and concatenates them
into a single **MXBI archive** embedded in the kernel image; the boot loader
(`kernel/src/arch/aarch64/userland.rs`) walks the archive and loads each module
into the proc slot named by its record. Each server gets its own per-process
TTBR0, so they all share the same low load base with no collision. Because a
server runs at EL0 with no console, its behavior is observed through kernel-side
traces (`[ipc]`, `[ksys]`, `[pf]`, `[alarm]`), never `println`.

## Request-number ranges

Every server's request numbers occupy a distinct band below `NOTIFY_MESSAGE`, so
a message type unambiguously identifies both its server and its meaning
(`kernel-shared/src/callnr.rs`, const-asserted disjoint):

| Base | Value | Server / purpose |
|------|-------|------------------|
| `PM_RQ_BASE`    | `0x700` | PM: `PM_GETPID` / `FORK` / `EXIT` / `WAIT` / `EXEC` |
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
PM, never user → kernel* (the shared user privilege only opens an `ipc_to` edge to
PM):

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

## VFS: skeletal

**VFS** (`servers/vfs/`) boots through SEF and publishes to DS, but does **no**
file operations yet: its receive loop drops any application message it gets. File
descriptors, the PM↔VFS work protocol, and real filesystem I/O require the
Phase-5 musl fork and file-system servers.

## init: PID 1

**init** (`userland/init/`) is the first real user process and the live exercise
for everything above. Unlike the demo stubs it replaced, it is a genuine boot
module: `build.rs` packs it into the MXBI archive with its true proc number
(`INIT_PROC_NR = 10`), and the ordinary boot loop loads it and makes it runnable —
PM does not hand-release it. It runs at user grade, sharing the `USER_PRIV_ID`
privilege (SENDREC to PM only, no kernel calls) with every forked child.

init is a plain `minix-ipc` program — no SEF, because it is not a server. Its
whole body is a respawn loop: `PM_FORK`; the child (`m_type == 0`) issues
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
