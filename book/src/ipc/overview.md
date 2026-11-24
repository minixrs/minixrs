# IPC

IPC is the foundation of MINIX. Every interaction between a user program and the OS, and between OS
components themselves, happens through message passing. Understanding IPC is understanding minix.rs.

This chapter describes IPC as it stands at the end of Phase 4: a fixed-size message, five live
primitives (with `SENDA` stubbed), generation-aware endpoints, deadlock detection, and the
per-process privilege bitmaps that gate it all. The kernel side lives in `kernel/src/ipc/`; the
shared wire types in `kernel-shared`; the user-space trap stubs in `minixrs-ipc`.

## Message structure

Every IPC message is a fixed-size, 8-aligned struct — 104 bytes on 64-bit platforms
(`kernel-shared/src/message.rs`):

```rust
#[repr(C, align(8))]
pub struct Message {
    pub m_source: Endpoint,   // i32, offset 0 — set by the kernel on delivery
    pub m_type: i32,          //      offset 4 — call number, or result code on reply
    pub payload: [u8; 96],    //      offset 8 — interpreted per-call
}
```

`size_of::<Message>() == 104` and `align_of::<Message>() == 8` are enforced by `const` assertions.
There is no separate padding field: `m_source` and `m_type` occupy the first 8 bytes and the 96-byte
payload starts at offset 8. The payload is a raw byte array that each call interprets with its own
typed accessors — minix.rs uses named per-call structs rather than MINIX 3's opaque `m1i1` / `m2l1`
union fields, so the protocol is self-documenting.

The fixed size is deliberate: the kernel copies the whole message on delivery, so a fixed size keeps
the copy fast and allocation-free, and forces protocol designers to keep messages compact. Transfers
larger than the payload are the job of grant-based safe copy (planned; see
[Privilege model](#privilege-model)).

> **Note on the "64-bit MINIX 3" reference.** MINIX 3 only ever shipped as a 32-bit operating system
> (i386, and later 32-bit ARM on the BeagleBone). Its source tree does carry x86_64 ABI definitions
> — including the `__x86_64__` branch of `include/minix/ipc.h` that fixes `sizeof(message) == 104` —
> but the upstream project never released a working 64-bit MINIX 3. A **functioning 64-bit MINIX 3
> prototype was a personal effort by this project's author (Kevin Barnard), separate from the
> upstream MINIX 3 project**. minix.rs takes that 104-byte layout as its ABI reference point, but is
> itself a clean, 64-bit-only implementation. When the docs say minix.rs "preserves the original
> ABI," that means tracking those source-tree definitions, not a shipped 64-bit MINIX 3.

## The IPC primitives

Six primitive numbers are defined (`kernel-shared/src/ipc_const.rs`); five are live and one is a
stub:

| Primitive | Number | Status   | Behavior                                                           |
| --------- | -----: | -------- | ------------------------------------------------------------------ |
| `SEND`    |      1 | live     | Blocking send; caller blocks until the receiver accepts.           |
| `RECEIVE` |      2 | live     | Blocking receive from a specific endpoint or `ANY`.                |
| `SENDREC` |      3 | live     | Atomic send-then-receive; the common client→server call.           |
| `NOTIFY`  |      4 | live     | Non-blocking notification; sets a bit if the target isn't waiting. |
| `SENDNB`  |      5 | live     | Non-blocking send; fails instead of blocking.                      |
| `SENDA`   |     16 | **stub** | Asynchronous send-table. Returns `ENOSYS`.                         |

**SEND** — if the destination is waiting to receive from us (or from `ANY`), the message is
delivered immediately and the destination is unblocked; otherwise the caller blocks and is queued on
the destination's `caller_q`.

**RECEIVE** — the kernel checks, in order: pending notifications (`notify_pending`), then the caller
queue (`caller_q`). (MINIX 3 checks pending async messages between the two; `SENDA` is a stub here,
so there are none.) If nothing is ready, the caller blocks with `RTS_RECEIVING` set. The first check
is skipped for the receive half of a `SENDREC` —
[see below](#a-notification-is-never-the-reply-to-a-sendrec).

**SENDREC** — a `SEND` immediately followed by a `RECEIVE`, but atomic: the caller transitions
straight from sending to receiving without returning to user space. This is what a POSIX wrapper
uses — send a request to a server and block for the reply.

**NOTIFY** — if the target is waiting in a plain `RECEIVE` that would accept the sender, a synthetic
notification message (`m_type == NOTIFY_MESSAGE`, `0x1000`) is delivered immediately; otherwise a
bit is set in the target's `notify_pending` bitmap and the message is built at the target's next
plain `RECEIVE`. `NOTIFY` never blocks, and repeated notifications from the same source coalesce
into one bit. Used for timer expiry (`CLOCK` → server), kernel-raised signals (`SYSTEM` → PM),
hardware-interrupt delivery (`HARDWARE` → driver), and — since slice 6.5 — one server telling
another to ask again (TTY → VFS, see
[Drivers](../drivers/overview.md#notify-then-pull-a-read-with-nothing-to-give)).

A `NOTIFY` from `HARDWARE` is the one notification with a payload: the owner's `int_pending` bitmap
sits at payload `0..4`, one bit per `notify_id` that fired, taken (read and zeroed) when the message
is built. Every other notification is all-zero. A receiver identifies it by `m_source` alone.

**SENDNB** — `SEND` semantics, but returns an error rather than blocking when the destination is not
ready to receive.

**SENDA** is defined for ABI completeness but is not implemented: it returns `ENOSYS`, and it cannot
be reached anyway because the `trap_mask` gate is a `u16` and `SENDA`'s bit is 16 — outside the
mask. Making it real is deferred (see [Roadmap](../roadmap.md)).

### A notification is never the reply to a `SENDREC`

A `SENDREC` is two steps inside one trap. `do_sendrec` runs the send half, and once that has
succeeded it sets the caller's `MF_REPLY_PEND` flag and runs the receive half against the same
partner. From that moment until a message is delivered to it, the caller is owed exactly one thing —
its partner's reply — and the kernel delivers it **no notification, from anyone**:

- **`NOTIFY` does not deliver at once to a flagged target.** `mini_notify` (and the three
  kernel-originated forms, for `CLOCK`, `SYSTEM` and `HARDWARE`) tests two things before building
  the message: that the target is blocked in a receive that would accept the sender, and that
  `MF_REPLY_PEND` is clear. Otherwise it sets the bit in `notify_pending`.
- **`RECEIVE` does not collect a pending bit for a flagged caller.** `mini_receive` skips the
  pending-notification check when the caller has `MF_REPLY_PEND` set, and goes straight to the
  caller queue. The bit stays set, and the notification is built at the caller's next *plain*
  `RECEIVE`.

The second half is the subtle one. The receive half of a `SENDREC` filters on the partner's
endpoint, and a notification from *that same partner* passes the filter. Deliver it, and the caller
returns from its `SENDREC` holding a message of type `NOTIFY_MESSAGE` where a reply should be; the
real reply turns up later, at the caller's ordinary receive loop, looking like a request nobody
sent. MINIX 3 guards exactly this in `mini_receive` ("pending notifications, except for SENDREC").
The Rust translation carried the first half from the start and lacked the second until slice 6.5 —
the first slice with a pair of processes where one both calls the other and is notified by it.

The flag is cleared when a message is delivered to its owner, and by `do_sendrec` itself if the
receive half fails (a dead partner, or `ELOCKED` from the deadlock check): that caller returns to
user space with an error and is owed nothing, and a flag left behind would withhold every later
notification from it.

Two lines in every boot log prove the rule, and they do not depend on timing, because TTY
[arranges the collision deliberately](../drivers/overview.md#what-the-boot-log-proves): `[diag vfs]
cdev.short ok n=256` and `[diag vfs] cons.notify ok`.

### The user-space trap (aarch64)

`minixrs-ipc` issues the IPC trap with `svc #0`. The register convention is:

| Register | Purpose                                                                   |
| -------- | ------------------------------------------------------------------------- |
| `x0`     | Endpoint (`src` / `dest` / `src_dest`); also receives the result          |
| `x1`     | IPC primitive number                                                      |
| `x2`     | Pointer to the 104-byte `Message` (null for `NOTIFY`, which carries none) |

The kernel's `do_ipc` writes the result into the saved `x0`; the SVC entry saves and restores
`x1..x30`, so from the caller's view only `x0` is clobbered. The public wrappers are `ipc_send`,
`ipc_receive`, `ipc_sendrec`, `ipc_sendnb`, and `ipc_notify` — there is no `ipc_senda`. The x86_64
trap ABI is design intent, part of the planned port.

## Endpoints

Processes are named by **endpoints**, not raw slot numbers. An `Endpoint` is an `i32` that encodes a
generation counter and a process-table slot (`kernel-shared/src/endpoint.rs`):

```rust
pub const ENDPOINT_GEN_SHIFT: i32 = 15;   // proc_nr in the low 15 bits, generation above
pub const fn make_endpoint(g: GenNr, p: ProcNr) -> Endpoint;
pub const fn endpoint_proc(e: Endpoint) -> ProcNr;   // sign-extends: task slots stay negative
pub const fn endpoint_gen(e: Endpoint) -> GenNr;
```

The generation increments each time a slot is reused (`bump_generation`, which wraps to 1 — never
back to 0, so a recycled slot can never re-alias a boot endpoint). This catches stale-endpoint bugs:
a message aimed at a dead process is rejected rather than misdelivered to whatever now occupies the
slot.

**Sentinel endpoints** live just below `ENDPOINT_SLOT_TOP` (`(1 << 14) − 1 = 16383`), all at
generation 0 so they can never collide with a real `(gen, proc)` endpoint:

| Endpoint |              Value | Meaning                    |
| -------- | -----------------: | -------------------------- |
| `ANY`    | `16383` (`0x3FFF`) | receive from any sender    |
| `NONE`   |            `16382` | no process                 |
| `SELF`   |            `16381` | the calling process itself |

**Kernel tasks** occupy negative slots (`kernel-shared/src/com.rs`):

| Proc nr | Name       | Role                                  |
| ------: | ---------- | ------------------------------------- |
|      -5 | `ASYNCM`   | async-message driver                  |
|      -4 | `IDLE`     | idle task                             |
|      -3 | `CLOCK`    | timer-driven work                     |
|      -2 | `SYSTEM`   | handles `SYS_*` kernel calls          |
|      -1 | `HARDWARE` | source endpoint for IRQ notifications |

**Well-known servers** are numbered contiguously from 0. minix.rs renumbers these compared to MINIX
3 (which scatters the slots). Some numbers are *reserved* for servers that do not yet boot:

| Proc nr | Server                  | Boots today?                               |
| ------: | ----------------------- | ------------------------------------------ |
|       0 | PM                      | yes                                        |
|       1 | VFS                     | yes                                        |
|       2 | RS                      | yes                                        |
|       3 | `MEM` (memory driver)   | yes                                        |
|       4 | `TTY`                   | yes                                        |
|       5 | DS                      | yes                                        |
|       6 | `MFS`                   | yes                                        |
|       7 | VM                      | yes                                        |
|       8 | `PFS`                   | reserved, not built                        |
|       9 | SCHED                   | yes                                        |
|      10 | `init` (PID 1)          | yes                                        |
|      11 | `VBLK` (virtio-blk)     | yes — serves its disk when one is attached |
|      12 | `VCON` (virtio-console) | reserved, not built                        |
|      13 | `VINPUT` (virtio-input) | reserved, not built                        |
|      14 | `VNET` (virtio-net)     | reserved, not built                        |

Slots 15–18 hold the demo stubs and PM's fork pool starts at 19.

Endpoints at generation 0 are *boot* endpoints; construct one from a task/server `ProcNr` with
`boot_endpoint(p)`.

## Deadlock detection

Because `SEND` blocks, circular send dependencies could deadlock the system:

```text
A sends to B (blocks) ; B sends to A (blocks) → neither can proceed
```

Before blocking a sender, the kernel (`kernel/src/ipc/deadlock.rs`) traces the chain of send
dependencies starting at the destination: if the destination is itself sending, it follows
`sendto_e`, and so on. If the chain loops back to the original caller, the `SEND` fails with
`ELOCKED` instead of blocking. A two-party pair (A→B, B→A) is allowed; larger cycles are rejected.

## Privilege model

Every system process has a privilege-table entry (`kernel/src/proc/priv_struct.rs`) controlling what
IPC and kernel operations it may perform. User processes share one user-class slot.

- **`trap_mask`** (`u16`) — which IPC primitives the process may use; bit `i` allows primitive `i`.
  User processes typically hold only `SENDREC`; servers hold `SEND`/`RECEIVE`/`NOTIFY`/… (This is
  also why `SENDA`, bit 16, is unreachable.)
- **`ipc_to`** — bitmap of which other privileged slots this process may send to. A user process can
  reach only the servers it needs: the shared user slot opens `ipc_to = {PM, VFS, VM}` — the process
  lifecycle, the file descriptors, and (since pre-Phase-6 chunk 5) the heap and anonymous mappings
  behind `brk`/`mmap`, and nothing else. Each entry is really a *pair* of bits, because the server
  needs the reverse edge to reply; `populate_user_priv` opens both.
- **`k_call_mask`** — bitmap of permitted kernel calls (empty for user processes). See
  [System Calls & ABI](../reference/syscalls.md).
- **`notify_pending` / `asyn_pending`** — per-source bitmaps of deferred notifications and async
  messages. A plain `RECEIVE` consumes `notify_pending`; the receive half of a `SENDREC` leaves it
  alone. Nothing sets `asyn_pending` while `SENDA` is a stub.
- **`sig_mgr`** — the endpoint that manages signals raised against this process.

The entry also carries a `grant_table` pointer and `irqs`, both in use, and reserves `io_ranges` and
`mem_ranges`, which nothing reads yet:

- **Grants / safe copy** — for data larger than the payload, a process publishes a grant table (via
  `SYS_SETGRANT`) describing memory regions and permitted operations, and the kernel validates a
  grant before copying (`SYS_SAFECOPY`). This is how every bulk transfer between servers, drivers
  and clients moves (live since Phase 5).
- **IRQ ownership** — `irqs` is the allow-list of interrupt lines a driver may claim: `SYS_IRQCTL`
  answers `EPERM` for a line that is not in it. The kernel fills it at boot — the PL011's line for
  TTY, the assigned device's line for each virtio driver (see
  [Drivers](../drivers/overview.md#interrupts-reach-a-driver-as-a-hardware-notification)).
- **I/O ports and memory ranges** — `io_ranges` / `mem_ranges` are reserved for gating a driver's
  access to ports and physical ranges; no call consults them today.

## IPC status

When it delivers a message, the kernel also provides an **IPC status** word encoding which primitive
delivered it (bits 0–5) and whether it originated in the kernel (a "from kernel" flag). This lets a
receiver distinguish a plain message from a notification and handle kernel-originated messages
specially — for instance, never replying to them.

## Kernel IPC state

The relevant per-process fields (`kernel/src/proc/`):

- `caller_q` / `q_link` — the head of, and links through, the queue of processes blocked trying to
  send to this one (`Option<ProcNr>` indices, not raw pointers).
- `getfrom_e` / `sendto_e` — the endpoint this process is receiving from / sending to while blocked.
- run-time-state flags (`rts_flags`) — `RTS_SENDING`, `RTS_RECEIVING`, and the others. **A process
  is runnable iff its RTS flags are all clear.**

IPC and scheduling are therefore coupled: unblocking a receiver (clearing its last RTS bit via
`rts_unset`) enqueues it on the run queue; blocking a sender (`rts_set`) removes it. See
[Servers](../servers/overview.md) for how the delegatable scheduler builds on this.

## MINIX 3 source reference

| Concept                    | MINIX 3 file                        | Key functions                |
| -------------------------- | ----------------------------------- | ---------------------------- |
| IPC dispatch               | `kernel/proc.c`                     | `do_ipc`, `do_sync_ipc`      |
| Blocking send / receive    | `kernel/proc.c`                     | `mini_send`, `mini_receive`  |
| Notification / async send  | `kernel/proc.c`                     | `mini_notify`, `mini_senda`  |
| Deadlock detection         | `kernel/proc.c`                     | `deadlock`                   |
| Process / privilege tables | `kernel/proc.h`, `priv.h`           | `struct proc`, `struct priv` |
| Message / IPC constants    | `include/minix/ipc.h`, `ipcconst.h` | `message`, `SEND`, …         |
