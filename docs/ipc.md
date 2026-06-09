# IPC (Inter-Process Communication) Design

IPC is the foundation of MINIX. Every interaction between user programs and the OS, and
between OS components themselves, happens through message passing. Understanding IPC is
understanding MINIX.

## Message Structure

Every IPC message is a fixed-size struct. On 64-bit platforms (aarch64, x86_64), the
message is 104 bytes total:

```
Offset  Size   Field
------  ----   -----
0       4      m_source    (Endpoint -- set by kernel, not caller)
4       4      m_type      (call number or result code)
8       4      _pad        (alignment)
12      92     payload     (union of typed message structs)
```

Total: 104 bytes.

**MINIX 3 reference:** `minix/include/minix/ipc.h`, `minix/include/minix/ipcconst.h`

### Payload Size

The payload is 96 bytes on x86_64 (per MINIX 3's `_IPC_PAYLOAD_SIZE`). In minix.rs
we use 88 bytes usable after alignment padding, keeping the total at 104 bytes.

The fixed message size is a deliberate design choice:
- Messages are copied by the kernel, so fixed size makes the copy fast and predictable
- No dynamic allocation in the IPC path
- Forces protocol designers to keep messages compact
- Larger data transfers use grant-based memory sharing (safe copies)

### Rust Representation

```rust
// kernel-shared/src/message.rs

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Message {
    pub m_source: Endpoint,    // 4 bytes (i32)
    pub m_type: i32,           // Call number (positive) or error (negative)
    pub _pad: u32,             // Alignment
    pub payload: [u8; 88],     // Typed access via methods
}

impl Message {
    pub fn as_pm_fork(&self) -> &MsgPmFork { /* transmute payload */ }
    pub fn as_vfs_read(&self) -> &MsgVfsRead { /* transmute payload */ }
    // ... typed accessors for each message type
}
```

### MINIX 3 Comparison

MINIX 3 uses opaque field names like `m1i1`, `m2l1`, `m3p1` in its legacy message
variants, plus newer typed variants like `mess_lc_vfs_creat`. minix.rs uses only named
typed structs -- e.g., `MsgVfsRead { fd, buf, count }` -- making the protocol
self-documenting.

## The Six IPC Primitives

### 1. SEND (blocking send)

```
Caller -> Kernel: "Send this message to process X"
```

- If X is waiting to receive from us (or from ANY): message is delivered immediately,
  X is unblocked
- If X is NOT waiting: caller blocks, is queued on X's `caller_q` linked list
- Caller remains blocked until X calls RECEIVE

**Non-blocking variant:** SENDNB returns an error instead of blocking.

**MINIX 3 reference:** `mini_send()` in `kernel/proc.c` (line ~880)

### 2. RECEIVE (blocking receive)

```
Caller -> Kernel: "Give me a message (from process X, or from ANY)"
```

The kernel checks, in order:
1. **Pending notifications** -- bits in `priv.notify_pending` bitmap
2. **Pending async messages** -- bits in `priv.asyn_pending` bitmap
3. **Caller queue** -- processes blocked trying to send to us (`caller_q`)

If a message is found, it's delivered immediately. Otherwise, the caller blocks
with `RTS_RECEIVING` set.

**MINIX 3 reference:** `mini_receive()` in `kernel/proc.c` (line ~977)

### 3. SENDREC (atomic send + receive)

```
Caller -> Kernel: "Send this message to X, then wait for a reply"
```

This is the most common IPC pattern. Used by `_syscall()` in the C library:

```c
int _syscall(endpoint_t who, int callnr, message *msg) {
    msg->m_type = callnr;
    ipc_sendrec(who, msg);        // trap to kernel
    if (msg->m_type < 0) {
        errno = -msg->m_type;
        return -1;
    }
    return msg->m_type;
}
```

SENDREC is essentially SEND followed by RECEIVE, but atomic -- the caller transitions
directly from "sending" to "receiving" without returning to user space in between.

**MINIX 3 reference:** handled in `do_ipc()` / `do_sync_ipc()` in `kernel/proc.c`

### 4. NOTIFY (asynchronous notification)

```
Caller -> Kernel: "Notify process X that something happened"
```

- If X is waiting: a notification message is assembled and delivered immediately
- If X is NOT waiting: a bit is set in X's `priv.notify_pending` bitmap
- NOTIFY never blocks the caller
- Multiple notifications from the same source coalesce into one bit

Notifications are used for:
- Hardware interrupt delivery (kernel notifies driver)
- Timer expiry (CLOCK notifies server)
- Signal delivery (PM notifies process)

**MINIX 3 reference:** `mini_notify()` in `kernel/proc.c` (line ~1132)

### 5. SENDNB (non-blocking send)

Same as SEND, but returns immediately with an error if the destination is not
waiting to receive. Used when blocking would be inappropriate.

### 6. SENDA (asynchronous send table)

```
Caller -> Kernel: "Here is a table of messages to deliver asynchronously"
```

The caller provides an array of `asynmsg_t` structures. The kernel tries to deliver
each one. Those that can't be delivered immediately remain in the table; the kernel
sets status bits so the caller can check later.

Used for non-blocking multi-destination messaging (e.g., RS checking multiple services).

**MINIX 3 reference:** `mini_senda()` in `kernel/proc.c` (line ~1341)

## Endpoint System

Processes are identified by **endpoints**, not raw process numbers. An endpoint
encodes both the process table slot and a generation number:

```rust
pub type Endpoint = i32;

pub fn endpoint(generation: u32, proc_nr: i32) -> Endpoint {
    (generation << GENERATION_SHIFT) | (proc_nr & PROC_NR_MASK)
}

pub fn proc_nr(endpoint: Endpoint) -> i32 {
    endpoint & PROC_NR_MASK
}
```

The generation number increments each time a slot is reused. This prevents stale
endpoints from accidentally sending messages to the wrong process (a new process
that happens to occupy the same slot).

**Special endpoints (kernel tasks):**

| Endpoint | Name | Role |
|----------|------|------|
| -5 | ASYNCM | Async message handler |
| -4 | IDLE | Idle task |
| -3 | CLOCK | Clock/timer task |
| -2 | SYSTEM | Kernel call handler |
| -1 | HARDWARE | Hardware interrupt pseudo-process |
| ANY | `0x7FFF` | Receive from any source |
| NONE | `0x7FFE` | No endpoint |

**Server endpoints (boot processes):**

| Endpoint | Name |
|----------|------|
| 0 | PM (Process Manager) |
| 1 | VFS (Virtual File System) |
| 2 | RS (Reincarnation Server) |
| 3 | MEM (Memory driver) |
| 4 | SCHED (Scheduler) |
| 5 | TTY (Terminal driver) |
| 6 | DS (Data Store) |
| 8 | VM (Virtual Memory) |

## Deadlock Detection

Since SEND is blocking, circular dependencies can deadlock the system:

```
Process A sends to B (blocks)
Process B sends to A (blocks)
-> Deadlock: neither can proceed
```

Before blocking a sender, the kernel traces the chain of send dependencies. If adding
this sender would create a cycle, the SEND fails with `ELOCKED`.

**MINIX 3 reference:** `deadlock()` in `kernel/proc.c` (line ~713)

The algorithm:
1. Start at the destination process
2. If it's SENDING, follow `sendto_e` to see who it's waiting for
3. If that process is also SENDING, continue the chain
4. If the chain reaches the original caller, it's a deadlock
5. Pairs (A sends to B, B sends to A) are allowed (group size 2)
6. Larger cycles are rejected

## Privilege Model

Every system process has a `Priv` structure controlling what IPC operations it can perform.

### trap_mask

Bitmask of which IPC operations the process can use:

```
Bit 0: unused
Bit 1: SEND
Bit 2: RECEIVE
Bit 3: SENDREC
Bit 4: NOTIFY
Bit 5: SENDNB
Bit 16: SENDA
```

Regular user processes typically only have SENDREC (send a request, wait for reply).
Servers have SEND, RECEIVE, NOTIFY, etc.

### ipc_to (destination bitmap)

A bitmap controlling which other processes this process can send messages to. Bit N
corresponds to system process with privilege ID N.

User processes can only send to the servers they need (typically PM, VFS, VM).
Drivers can send to VFS and RS but not to PM directly.

### k_call_mask (kernel call bitmap)

Controls which kernel calls (SYS_FORK, SYS_VMCTL, etc.) this process can make.
Only system processes have kernel call access. User programs have no bits set.

### I/O and IRQ permissions

- `io_ranges` -- Hardware I/O port ranges the process can access (x86_64 only)
- `irqs` -- IRQ lines the process can register handlers for
- `mem_ranges` -- Physical memory ranges accessible via safe copy

### Grant Table

For transferring data larger than the message payload, processes use **grants**.
A grant gives another process temporary, controlled access to a region of the
granting process's memory:

```rust
struct CpGrant {
    flags: u32,           // CPF_READ, CPF_WRITE
    who_to: Endpoint,     // Who can use this grant
    start: VirAddr,       // Start address in granting process
    len: usize,           // Size of region
}
```

The granting process publishes its grant table address via `sys_setgrant()`.
The kernel validates grants during safe copy operations (`sys_safecopyfrom`,
`sys_safecopyto`).

**Flow example (VFS reading user buffer):**
1. User calls `read(fd, buf, 4096)` -> message to VFS with `buf` pointer
2. VFS creates a grant allowing the FS driver to write to the user's buffer
3. VFS sends `REQ_READ` to MFS with the grant ID
4. MFS uses `sys_safecopyfrom` (kernel call) with the grant to copy disk data into the user's buffer
5. The kernel validates the grant chain and performs the copy

## IPC Status

When receiving a message, the kernel also provides an **IPC status** word:

```rust
pub fn ipc_status_call(status: u32) -> u32 {
    (status >> 0) & 0x3F
}
```

The status encodes:
- Which IPC primitive delivered the message (bits 0-5)
- Whether the message originated in the kernel (`IPC_FLG_MSG_FROM_KERNEL`, bit 16)

This allows the receiver to distinguish between a regular message and a notification,
and to handle kernel-originated messages specially (never reply to them).

## IPC Register ABI

### aarch64 (primary)

The `SVC #0` instruction traps to EL1. Registers:

| Register | Purpose |
|----------|---------|
| x0 | IPC operation (SEND=1, RECEIVE=2, SENDREC=3, NOTIFY=4, SENDNB=5, SENDA=16) |
| x1 | Source/destination endpoint |
| x2 | Pointer to Message struct in user space |
| x3 | (SENDA only) number of async messages |
| x0 (return) | 0 on success, negative error code on failure |

### x86_64

The `SYSCALL` instruction traps to ring 0. Registers:

| Register | Purpose |
|----------|---------|
| rax | IPC operation |
| rbx | Source/destination endpoint |
| rcx | (clobbered by SYSCALL -- holds return RIP) |
| rdx | Pointer to Message struct in user space |
| r8 | (SENDA only) number of async messages |
| rax (return) | 0 on success, negative error code |

## Key Data Structures in Kernel

### Process IPC State (in `struct Proc`)

```rust
// Linked list of processes waiting to send to this process
pub caller_q: Option<ProcNr>,   // Head of queue
pub q_link: Option<ProcNr>,     // Next in queue (each proc has one)

// What this process is blocked on
pub getfrom_e: Endpoint,        // RECEIVE: accept from this endpoint (or ANY)
pub sendto_e: Endpoint,         // SEND: trying to send to this endpoint

// Buffered messages
pub send_msg: Message,          // Copy of message being sent (while SENDING)
pub deliver_msg: Message,       // Message ready for delivery (when MF_DELIVERMSG)

// Runtime status flags (determine blocking state)
pub rts_flags: AtomicU32,
//   RTS_SENDING   (0x04) -- blocked trying to send
//   RTS_RECEIVING (0x08) -- blocked trying to receive
```

A process is **runnable** if and only if `rts_flags == 0`.

### Run Queue Integration

When `rts_flags` transitions from non-zero to zero (via `rts_unset()`), the process
is added to the appropriate priority run queue. When it transitions from zero to non-zero
(via `rts_set()`), it's removed.

This means IPC operations directly control scheduling: unblocking a receiver makes it
runnable; blocking a sender removes it from the run queue.

## MINIX 3 Source Reference

| Concept | MINIX 3 File | Key Functions |
|---------|-------------|---------------|
| IPC dispatch | `kernel/proc.c` | `do_ipc()`, `do_sync_ipc()` |
| Blocking send | `kernel/proc.c` | `mini_send()` |
| Blocking receive | `kernel/proc.c` | `mini_receive()` |
| Async notification | `kernel/proc.c` | `mini_notify()` |
| Async send table | `kernel/proc.c` | `mini_senda()` |
| Deadlock detection | `kernel/proc.c` | `deadlock()` |
| Process table | `kernel/proc.h` | `struct proc` |
| Privilege structure | `kernel/priv.h` | `struct priv` |
| Message definitions | `include/minix/ipc.h` | `message`, `mess_*` types |
| IPC constants | `include/minix/ipcconst.h` | SEND, RECEIVE, etc. |
| User-space stubs | `lib/libc/arch/x86_64/sys/_ipc.S` | `_ipc_sendrec_intr` |
| Syscall wrapper | `lib/libc/sys/syscall.c` | `_syscall()` |
