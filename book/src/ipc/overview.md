# IPC

IPC is the foundation of MINIX. Every interaction between a user program and the
OS, and between OS components themselves, happens through message passing.
Understanding IPC is understanding minix.rs.

This chapter describes IPC as it stands at the end of Phase 4: a fixed-size
message, five live primitives (with `SENDA` stubbed), generation-aware endpoints,
deadlock detection, and the per-process privilege bitmaps that gate it all. The
kernel side lives in `kernel/src/ipc/`; the shared wire types in `kernel-shared`;
the user-space trap stubs in `minixrs-ipc`.

## Message structure

Every IPC message is a fixed-size, 8-aligned struct — 104 bytes on 64-bit
platforms (`kernel-shared/src/message.rs`):

```rust
#[repr(C, align(8))]
pub struct Message {
    pub m_source: Endpoint,   // i32, offset 0 — set by the kernel on delivery
    pub m_type: i32,          //      offset 4 — call number, or result code on reply
    pub payload: [u8; 96],    //      offset 8 — interpreted per-call
}
```

`size_of::<Message>() == 104` and `align_of::<Message>() == 8` are enforced by
`const` assertions. There is no separate padding field: `m_source` and `m_type`
occupy the first 8 bytes and the 96-byte payload starts at offset 8. The payload
is a raw byte array that each call interprets with its own typed accessors —
minix.rs uses named per-call structs rather than MINIX 3's opaque `m1i1` / `m2l1`
union fields, so the protocol is self-documenting.

The fixed size is deliberate: the kernel copies the whole message on delivery, so
a fixed size keeps the copy fast and allocation-free, and forces protocol
designers to keep messages compact. Transfers larger than the payload are the job
of grant-based safe copy (planned; see [Privilege model](#privilege-model)).

> **Note on the "64-bit MINIX 3" reference.** MINIX 3 only ever shipped as a
> 32-bit operating system (i386, and later 32-bit ARM on the BeagleBone). Its
> source tree does carry x86_64 ABI definitions — including the `__x86_64__`
> branch of `include/minix/ipc.h` that fixes `sizeof(message) == 104` — but the
> upstream project never released a working 64-bit MINIX 3. A **functioning
> 64-bit MINIX 3 prototype was a personal effort by this project's author (Kevin
> Barnard), separate from the upstream MINIX 3 project**. minix.rs takes that
> 104-byte layout as its ABI reference point, but is itself a clean, 64-bit-only
> implementation. When the docs say minix.rs "preserves the original ABI," that
> means tracking those source-tree definitions, not a shipped 64-bit MINIX 3.

## The IPC primitives

Six primitive numbers are defined (`kernel-shared/src/ipc_const.rs`); five are
live and one is a stub:

| Primitive | Number | Status | Behavior |
|-----------|-------:|--------|----------|
| `SEND`    | 1 | live | Blocking send; caller blocks until the receiver accepts. |
| `RECEIVE` | 2 | live | Blocking receive from a specific endpoint or `ANY`. |
| `SENDREC` | 3 | live | Atomic send-then-receive; the common client→server call. |
| `NOTIFY`  | 4 | live | Non-blocking notification; sets a bit if the target isn't waiting. |
| `SENDNB`  | 5 | live | Non-blocking send; fails instead of blocking. |
| `SENDA`   | 16 | **stub** | Asynchronous send-table. Returns `ENOSYS`. |

**SEND** — if the destination is waiting to receive from us (or from `ANY`), the
message is delivered immediately and the destination is unblocked; otherwise the
caller blocks and is queued on the destination's `caller_q`.

**RECEIVE** — the kernel checks, in order: pending notifications
(`notify_pending`), pending async messages (`asyn_pending`), then the caller queue
(`caller_q`). If nothing is ready, the caller blocks with `RTS_RECEIVING` set.

**SENDREC** — a `SEND` immediately followed by a `RECEIVE`, but atomic: the caller
transitions straight from sending to receiving without returning to user space.
This is what a POSIX wrapper uses — send a request to a server and block for the
reply.

**NOTIFY** — if the target is waiting, a synthetic notification message
(`m_type == NOTIFY_MESSAGE`, `0x1000`) is delivered immediately; otherwise a bit
is set in the target's `notify_pending` bitmap. `NOTIFY` never blocks, and
repeated notifications from the same source coalesce into one bit. Used for timer
expiry (`CLOCK` → server), kernel-raised signals (`SYSTEM` → PM), and — in the
driver era — hardware-interrupt delivery.

**SENDNB** — `SEND` semantics, but returns an error rather than blocking when the
destination is not ready to receive.

**SENDA** is defined for ABI completeness but is not implemented: it returns
`ENOSYS`, and it cannot be reached anyway because the `trap_mask` gate is a `u16`
and `SENDA`'s bit is 16 — outside the mask. Making it real is deferred (see
[Roadmap](../roadmap.md)).

### The user-space trap (aarch64)

`minixrs-ipc` issues the IPC trap with `svc #0`. The register convention is:

| Register | Purpose |
|----------|---------|
| `x0` | Endpoint (`src` / `dest` / `src_dest`); also receives the result |
| `x1` | IPC primitive number |
| `x2` | Pointer to the 104-byte `Message` (null for `NOTIFY`, which carries none) |

The kernel's `do_ipc` writes the result into the saved `x0`; the SVC entry saves
and restores `x1..x30`, so from the caller's view only `x0` is clobbered. The
public wrappers are `ipc_send`, `ipc_receive`, `ipc_sendrec`, `ipc_sendnb`, and
`ipc_notify` — there is no `ipc_senda`. The x86_64 trap ABI is design intent, part
of the planned port.

## Endpoints

Processes are named by **endpoints**, not raw slot numbers. An `Endpoint` is an
`i32` that encodes a generation counter and a process-table slot
(`kernel-shared/src/endpoint.rs`):

```rust
pub const ENDPOINT_GEN_SHIFT: i32 = 15;   // proc_nr in the low 15 bits, generation above
pub const fn make_endpoint(g: GenNr, p: ProcNr) -> Endpoint;
pub const fn endpoint_proc(e: Endpoint) -> ProcNr;   // sign-extends: task slots stay negative
pub const fn endpoint_gen(e: Endpoint) -> GenNr;
```

The generation increments each time a slot is reused (`bump_generation`, which
wraps to 1 — never back to 0, so a recycled slot can never re-alias a boot
endpoint). This catches stale-endpoint bugs: a message aimed at a dead process is
rejected rather than misdelivered to whatever now occupies the slot.

**Sentinel endpoints** live just below `ENDPOINT_SLOT_TOP` (`(1 << 14) − 1 =
16383`), all at generation 0 so they can never collide with a real
`(gen, proc)` endpoint:

| Endpoint | Value | Meaning |
|----------|------:|---------|
| `ANY`  | `16383` (`0x3FFF`) | receive from any sender |
| `NONE` | `16382` | no process |
| `SELF` | `16381` | the calling process itself |

**Kernel tasks** occupy negative slots (`kernel-shared/src/com.rs`):

| Proc nr | Name | Role |
|--------:|------|------|
| -5 | `ASYNCM` | async-message driver |
| -4 | `IDLE` | idle task |
| -3 | `CLOCK` | timer-driven work |
| -2 | `SYSTEM` | handles `SYS_*` kernel calls |
| -1 | `HARDWARE` | source endpoint for IRQ notifications |

**Well-known servers** are numbered contiguously from 0. minix.rs renumbers these
compared to MINIX 3 (which scatters the slots). Note that some numbers are
*reserved* for servers that do not yet boot:

| Proc nr | Server | Boots today? |
|--------:|--------|--------------|
| 0 | PM | yes |
| 1 | VFS | yes (skeletal) |
| 2 | RS | yes |
| 3 | `MEM` (memory driver) | reserved, not built |
| 4 | `TTY` | reserved, not built |
| 5 | DS | yes |
| 6 | `MFS` | reserved, not built |
| 7 | VM | yes |
| 8 | `PFS` | reserved, not built |
| 9 | SCHED | yes |
| 10 | `init` (PID 1) | yes |

Endpoints at generation 0 are *boot* endpoints; construct one from a task/server
`ProcNr` with `boot_endpoint(p)`.

## Deadlock detection

Because `SEND` blocks, circular send dependencies could deadlock the system:

```text
A sends to B (blocks) ; B sends to A (blocks) → neither can proceed
```

Before blocking a sender, the kernel (`kernel/src/ipc/deadlock.rs`) traces the
chain of send dependencies starting at the destination: if the destination is
itself sending, it follows `sendto_e`, and so on. If the chain loops back to the
original caller, the `SEND` fails with `ELOCKED` instead of blocking. A two-party
pair (A→B, B→A) is allowed; larger cycles are rejected.

## Privilege model

Every system process has a privilege-table entry
(`kernel/src/proc/priv_struct.rs`) controlling what IPC and kernel operations it
may perform. User processes share one user-class slot.

- **`trap_mask`** (`u16`) — which IPC primitives the process may use; bit `i`
  allows primitive `i`. User processes typically hold only `SENDREC`; servers hold
  `SEND`/`RECEIVE`/`NOTIFY`/… (This is also why `SENDA`, bit 16, is unreachable.)
- **`ipc_to`** — bitmap of which other privileged slots this process may send to.
  A user process can reach only the servers it needs: the shared user slot opens
  `ipc_to = {PM, VFS}` — the process lifecycle and the file descriptors, and
  nothing else. Each entry is really a *pair* of bits, because the server needs the
  reverse edge to reply; `populate_user_priv` opens both.
- **`k_call_mask`** — bitmap of permitted kernel calls (empty for user
  processes). See [System Calls & ABI](../reference/syscalls.md).
- **`notify_pending` / `asyn_pending`** — per-source bitmaps of deferred
  notifications and async messages, consumed by `RECEIVE`.
- **`sig_mgr`** — the endpoint that manages signals raised against this process.

The entry also reserves `io_ranges`, `irqs`, `mem_ranges`, and a `grant_table`
pointer. These are the hooks for two future mechanisms:

- **Grants / safe copy** — for data larger than the payload, a process will
  publish a grant table (via `SYS_SETGRANT`) describing memory regions and
  permitted operations, and the kernel will validate a grant before copying
  (`SYS_SAFECOPY`). The call numbers exist; a real grant table and validated copy
  are an opening Phase-5 slice, so no cross-address-space bulk copy runs yet.
- **I/O and IRQ ownership** — `io_ranges` / `irqs` will gate a driver's access to
  ports and interrupt lines. Drivers are a later phase.

## IPC status

When it delivers a message, the kernel also provides an **IPC status** word
encoding which primitive delivered it (bits 0–5) and whether it originated in the
kernel (a "from kernel" flag). This lets a receiver distinguish a plain message
from a notification and handle kernel-originated messages specially — for
instance, never replying to them.

## Kernel IPC state

The relevant per-process fields (`kernel/src/proc/`):

- `caller_q` / `q_link` — the head of, and links through, the queue of processes
  blocked trying to send to this one (`Option<ProcNr>` indices, not raw pointers).
- `getfrom_e` / `sendto_e` — the endpoint this process is receiving from / sending
  to while blocked.
- run-time-state flags (`rts_flags`) — `RTS_SENDING`, `RTS_RECEIVING`, and the
  others. **A process is runnable iff its RTS flags are all clear.**

IPC and scheduling are therefore coupled: unblocking a receiver (clearing its last
RTS bit via `rts_unset`) enqueues it on the run queue; blocking a sender
(`rts_set`) removes it. See [Servers](../servers/overview.md) for how the
delegatable scheduler builds on this.

## MINIX 3 source reference

| Concept | MINIX 3 file | Key functions |
|---------|--------------|---------------|
| IPC dispatch | `kernel/proc.c` | `do_ipc`, `do_sync_ipc` |
| Blocking send / receive | `kernel/proc.c` | `mini_send`, `mini_receive` |
| Notification / async send | `kernel/proc.c` | `mini_notify`, `mini_senda` |
| Deadlock detection | `kernel/proc.c` | `deadlock` |
| Process / privilege tables | `kernel/proc.h`, `priv.h` | `struct proc`, `struct priv` |
| Message / IPC constants | `include/minix/ipc.h`, `ipcconst.h` | `message`, `SEND`, … |
