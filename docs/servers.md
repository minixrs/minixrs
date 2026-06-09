# minix.rs System Servers

minix.rs inherits the microkernel architecture of MINIX 3: the kernel handles only interrupts,
IPC, and scheduling primitives, while all OS services run as independent user-space processes
that communicate via message passing. This document describes each system server, the SEF
framework they share, and the boot order that brings the system up.

---

## PM (Process Manager)

### Responsibilities

PM owns process lifecycle and identity:

- **Process creation/destruction**: `fork`, `exec`, `exit`, `wait4`
- **Signals**: `sigaction`, `kill`, `sigprocmask`, `sigpending`, `sigsuspend`
- **Process credentials**: real/effective/saved UIDs and GIDs, `setuid`, `setgid`
- **Process groups and sessions**: `setpgid`, `setsid`, `getpgrp`
- **Debugging**: `ptrace` (attach, single-step, read/write memory)

### Key Data Structure: `mproc`

The `mproc` table has one entry per process slot. Important fields:

| Field | Purpose |
|---|---|
| `mp_parent` | PID of parent process |
| `mp_child` / `mp_sibling` | Linked list of children |
| `mp_sigact[]` | Per-signal disposition (handler, mask, flags) |
| `mp_realuid` / `mp_effuid` | Real and effective user IDs |
| `mp_exitstatus` | Exit code, set on `exit()` |
| `mp_flags` | State bits (IN_USE, WAITING, STOPPED, TRACED, etc.) |

### Coordination with VFS

PM and VFS must stay in sync about the process table. PM sends endpoint-aware
notifications to VFS on key events:

- **`VFS_PM_FORK`** -- VFS duplicates the parent's file descriptor table for the child.
- **`VFS_PM_EXEC`** -- VFS closes all close-on-exec file descriptors and updates the
  executable vnode.
- **`VFS_PM_EXIT`** -- VFS closes all open file descriptors and releases vnodes.

These are sent as synchronous IPC messages; PM waits for VFS to acknowledge before
completing the operation toward the user process.

### Coordination with VM

PM notifies VM when processes are created or destroyed so VM can allocate or reclaim
address spaces. On `fork`, VM duplicates the parent's page tables using copy-on-write.
On `exec`, VM tears down the old address space and builds a new one from the ELF binary.
On `exit`, VM frees all page frames.

### MINIX 3 Reference

`minix/servers/pm/` -- `main.c`, `forkexit.c`, `exec.c`, `signal.c`, `table.c`

---

## VFS (Virtual File System)

### Responsibilities

VFS is the single entry point for all file-related system calls:

- **File operations**: `open`, `read`, `write`, `close`, `lseek`, `stat`, `fstat`
- **Directory operations**: `mkdir`, `rmdir`, `readdir`, `rename`, `link`, `unlink`
- **Mount/unmount**: manages the mount table that maps mount points to FS servers
- **Device mapping**: routes block device requests via `BDEV_*` messages to block drivers,
  character device requests via `CDEV_*` messages to character drivers
- **File descriptors**: per-process FD tables, dup/dup2, fcntl, close-on-exec

### Multi-Threaded Design

VFS uses worker threads so that a request blocked on disk I/O does not stall other
requests. When a worker sends a message to an FS server and awaits a reply, other
workers continue processing. This is critical because disk operations can take
milliseconds while VFS must remain responsive to pipe reads, TTY input, and other
non-blocking sources.

### Key Data Structures

| Structure | Purpose |
|---|---|
| **vnode table** | In-memory representation of open files; caches inode info from FS servers |
| **vmnt table** | Virtual mount table; one entry per mounted filesystem, references the FS server endpoint |
| **filp table** | Open file descriptions (shared across dup'd FDs); holds vnode pointer, offset, mode |
| **fproc table** | Per-process state: FD-to-filp mapping, root dir, working dir, umask |
| **dmap table** | Device map: maps major device numbers to driver endpoints |

### Request Routing

VFS never touches disk directly. It translates VFS-level requests into FS-protocol
requests and forwards them:

1. User process sends `VFS_READ(fd, buf, count)` to VFS.
2. VFS looks up the FD -> filp -> vnode, determines which vmnt (mount) owns it.
3. VFS sends `REQ_READ(inode_nr, offset, count)` to the FS server (e.g., MFS).
4. The FS server performs the read (possibly issuing `BDEV_READ` to a block driver).
5. The FS server replies with data and bytes-read count.
6. VFS copies data to the user process and replies with the byte count.

### MINIX 3 Reference

`minix/servers/vfs/` -- `main.c`, `open.c`, `read.c`, `path.c`, `mount.c`, `request.c`, `worker.c`

---

## VM (Virtual Memory)

### Responsibilities

VM manages all physical memory and per-process virtual address spaces:

- **Page fault handling**: receives `VM_PAGEFAULT` messages from the kernel when a process
  faults; resolves by mapping a physical page, performing copy-on-write, or killing the
  process on illegal access
- **mmap/munmap**: anonymous and file-backed memory mappings
- **brk/sbrk**: heap management
- **fork**: duplicates the parent address space with copy-on-write page table entries
- **exec**: tears down the old address space, parses the ELF binary, creates text/data/bss/stack
  regions

### Memory Regions

Each process address space is described as a list of regions:

| Region | Description |
|---|---|
| **Text** | Executable code, read-only, possibly shared between processes |
| **Data** | Initialized data from the ELF binary |
| **BSS/Heap** | Zero-initialized data extending to the program break |
| **Stack** | Grows downward, auto-extended on page fault near the stack pointer |
| **mmap** | File-backed or anonymous mappings created by `mmap()` |
| **Shared memory** | Regions shared between cooperating processes |

### Page Fault Flow

1. Process accesses an unmapped or protected virtual address.
2. CPU traps; kernel catches the fault and sends `VM_PAGEFAULT(endpoint, address, flags)` to VM.
3. VM looks up the faulting address in the process's region list.
4. If the region is copy-on-write: allocate a new page, copy contents, update page table.
5. If the region allows the access: allocate and map a zero page (for heap/stack growth).
6. If no region covers the address: send SIGSEGV via PM.

### MINIX 3 Reference

`minix/servers/vm/` -- `main.c`, `pagefaults.c`, `mmap.c`, `region.c`, `mem_anon.c`

---

## RS (Reincarnation Server)

### Responsibilities

RS is the watchdog and service manager for the entire system:

- **Service lifecycle**: start, stop, restart any system server or driver
- **Crash detection**: periodic heartbeat monitoring; a missing heartbeat triggers recovery
- **Live update**: replace a running server with a new binary without rebooting

### Service Commands

| Message | Action |
|---|---|
| `RS_UP` | Start a new service instance |
| `RS_DOWN` | Stop a running service |
| `RS_RESTART` | Stop and restart a service |
| `RS_REFRESH` | Re-read configuration for a service |
| `RS_UPDATE` | Perform a live update (quiesce, transfer state, switch) |

### Crash Recovery Flow

1. RS sends periodic pings to every registered service (via SEF).
2. A service must reply within a deadline to prove liveness.
3. If a service misses its deadline:
   - RS marks the service as crashed.
   - RS stops the old instance (reclaims its process slot and endpoint).
   - RS starts a new instance of the same binary.
   - The new instance calls `sef_startup()`, which triggers `init_restart` instead of
     `init_fresh`. The restart callback queries DS for saved state.
   - RS publishes the new endpoint to DS so other servers can find it.

### MINIX 3 Reference

`minix/servers/rs/` -- `main.c`, `manager.c`, `request.c`

---

## DS (Data Store)

### Responsibilities

DS provides a lightweight key-value publish/subscribe store used primarily for crash
recovery. Services publish critical state to DS so that, after a restart, the new
instance can retrieve it.

### Operations

| Operation | Description |
|---|---|
| **Publish** | Store a key-value pair (string key, arbitrary data) |
| **Subscribe** | Register interest in keys matching a label pattern |
| **Retrieve** | Fetch the current value for a key |
| **Delete** | Remove a key-value pair |
| **Snapshot** | Capture a consistent snapshot of a service's published state |

### Role in Recovery

The recovery protocol follows this pattern:

1. During normal operation, a service periodically publishes its state to DS.
2. The service crashes.
3. RS detects the crash and starts a new instance.
4. The new instance's `init_restart` callback queries DS with its own label.
5. DS returns the most recently published state.
6. The service rebuilds its internal tables from the recovered data.

DS itself is the first server started at boot, so it has no dependency on other servers
for its own recovery. If DS crashes, RS restarts it, and DS recovers from its own
in-memory checkpoint (or starts fresh if that is lost).

### MINIX 3 Reference

`minix/servers/ds/` -- `main.c`, `store.c`

---

## SCHED (Scheduler)

### Responsibilities

SCHED implements user-space scheduling policy. The kernel maintains the run queues and
performs the actual context switch, but it delegates priority and quantum decisions to
SCHED.

### Kernel-SCHED Interaction

1. A process exhausts its time quantum.
2. The kernel sends `SCHEDULING_NO_QUANTUM(endpoint)` to SCHED.
3. SCHED examines the process's history (CPU usage, nice value, priority class) and
   decides a new priority level and quantum length.
4. SCHED replies with `SCHEDULING_SET_NICE(endpoint, priority, quantum)`.
5. The kernel places the process on the appropriate run queue with the new quantum.

This split means scheduling policy can be changed, debugged, or even replaced entirely
without modifying or recompiling the kernel. Multiple schedulers can coexist (e.g., a
real-time scheduler for certain processes and the default scheduler for others).

### MINIX 3 Reference

`minix/servers/sched/` -- `main.c`, `schedule.c`

---

## SEF (System Events Framework)

SEF is not a server itself but a library linked into every server. It provides the
standard startup handshake, message loop, and event handling that RS depends on for
service management.

### Core Functions

#### `sef_startup(callbacks)`

Performs the handshake with RS at server start:

1. Sends a ready message to RS.
2. Receives the init message from RS (which indicates fresh start or restart).
3. Calls the appropriate init callback.

#### `sef_receive(source)`

The main receive loop. It wraps the raw IPC receive and intercepts SEF-level messages
before they reach the server's own dispatch:

- **Ping messages** from RS are automatically handled by calling the ping callback
  (which by default just replies, proving liveness).
- **Signal messages** forwarded by PM are handled by calling the signal callback.
- **Live-update messages** from RS are handled by the update callback (quiesce state,
  prepare for transfer).
- All other messages are returned to the server for normal dispatch.

### Callbacks

| Callback | When Called | Purpose |
|---|---|---|
| `init_fresh` | First start | Initialize all data structures from scratch |
| `init_restart` | After crash restart | Recover state from DS, rebuild tables |
| `ping_reply` | RS ping received | Prove liveness (default: just reply) |
| `signal_handler` | PM signal forwarded | Handle SIGTERM, SIGHUP, etc. |

### MINIX 3 Reference

`minix/lib/libsys/` -- `sef.c`, `sef_init.c`, `sef_ping.c`, `sef_signal.c`

---

## Server Message Loop Pattern

Every minix.rs server follows the same canonical structure:

```rust
fn main() {
    // Register callbacks for fresh init, restart, ping, signals
    sef_startup(&callbacks);

    // Main message loop
    loop {
        // sef_receive intercepts SEF messages (pings, signals, live-update)
        // and only returns application-level messages to us
        let (msg, status) = sef_receive(ANY).unwrap();

        let result = match msg.m_type {
            PM_FORK    => do_fork(&msg),
            PM_EXEC    => do_exec(&msg),
            PM_EXIT    => do_exit(&msg),
            PM_WAIT4   => do_wait4(&msg),
            PM_KILL    => do_kill(&msg),
            // ...
            _ => Err(ENOSYS),
        };

        // Some calls (like wait4) may suspend the caller and reply later.
        // Only send an immediate reply if the call did not suspend.
        if result != SUSPEND {
            reply(msg.m_source, result);
        }
    }
}
```

Key points about this pattern:

- **`sef_startup`** must be called before entering the loop. It completes the RS handshake
  and runs the init callback. Without it, RS considers the server failed to start.
- **`sef_receive(ANY)`** blocks until a message arrives. The `ANY` source means accept
  messages from any sender. Servers that need to receive from a specific source can pass
  a specific endpoint.
- **`SUSPEND`** is used for calls that cannot be answered immediately (e.g., `wait4` when
  no child has exited yet). The server records the pending request and replies later when
  the event occurs.
- **`reply`** sends a response message back to the requesting process, unblocking it from
  its `sendrec` call.

---

## Boot Order

```
DS -> RS -> PM -> SCHED -> VFS -> memory -> tty -> VM -> PFS -> MFS -> init
```

### Why This Order Matters

The boot order is dictated by dependencies -- each server requires the services of the
servers started before it.

1. **DS** starts first. It has no dependencies on other servers and provides the key-value
   store that all subsequent servers need for crash recovery and endpoint lookup. Without
   DS, no server can publish its state or find other servers' endpoints.

2. **RS** starts second. It needs DS to publish service state but nothing else. Once RS is
   running, it manages the lifecycle of every subsequent server. All servers from this
   point are started *by* RS rather than directly by the kernel.

3. **PM** starts next. It needs RS for lifecycle management and DS for state storage.
   PM must be running before VFS because VFS coordinates with PM on fork/exec/exit
   events. If VFS started first, it would have no PM to synchronize with.

4. **SCHED** starts after PM. Processes created by PM need a scheduler to assign them
   priorities and quanta. Without SCHED, newly forked processes would have no scheduling
   policy.

5. **VFS** starts after PM and SCHED. VFS coordinates with PM (via `VFS_PM_FORK`,
   `VFS_PM_EXEC`, `VFS_PM_EXIT` messages), so PM must be ready to receive those
   messages. VFS also needs SCHED running so its worker threads get scheduled.

6. **memory** (memory driver) provides `/dev/null`, `/dev/zero`, and `/dev/mem`. These
   are needed by VM and other services.

7. **tty** (terminal driver) provides console I/O. Started early so that subsequent
   servers and boot messages can print to the console.

8. **VM** starts after the memory driver because it needs to allocate physical pages.
   VM is started relatively late because the servers before it can operate with the
   boot-time memory layout set up by the kernel. Once VM is running, full demand
   paging, mmap, and copy-on-write are available.

9. **PFS** (Pipe File System) handles pipes and FIFOs. It must be running before MFS
   because pipe operations may occur during filesystem initialization.

10. **MFS** (MINIX File System) mounts the root filesystem. It needs VFS (to register
    itself as an FS server), PM (process management), and VM (memory mapping).

11. **init** (`/sbin/init`) is the first user-space process. It reads `/etc/rc` (or
    equivalent) and starts the rest of the system (login prompts, daemons, etc.).
    By this point the entire server infrastructure is operational.

### Circular Dependency Avoidance

A notable design constraint is avoiding circular dependencies during boot. For example,
VM would ideally be available from the start (for demand paging), but VM itself needs
PM and VFS. The solution is that early-boot servers operate with pre-allocated memory
from the kernel's boot image, and VM retrofits demand paging onto their address spaces
once it starts.
