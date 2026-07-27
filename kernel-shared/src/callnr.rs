// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Kernel-call numbers — the `m_type` values for `SENDREC`s addressed to the
//! `SYSTEM` task.
//!
//! Numbering convention follows MINIX 3 `include/minix/com.h` (`KERNEL_CALL`
//! base, contiguous offsets). Only the 14 calls needed by Phase 2 are
//! defined; more are added as later phases come online.

/// Base for kernel-call numbers. Matches MINIX 3 `KERNEL_CALL`.
pub const KERNEL_CALL: i32 = 0x600;

#[allow(clippy::identity_op)] // explicit `+ 0` keeps the table aligned visually
pub const SYS_GETINFO: i32 = KERNEL_CALL + 0;
pub const SYS_PRIVCTL: i32 = KERNEL_CALL + 1;
pub const SYS_FORK: i32 = KERNEL_CALL + 2;
/// PM → kernel: replace a target proc's program image (slice 4.7). Target
/// endpoint in payload `0..4` (i32); the boot-embedded binary name (NUL-padded,
/// `EXEC_NAME_LEN` bytes) in `4..4+EXEC_NAME_LEN`. The kernel resolves the name
/// in the MXBI archive (`BootImage::module_by_name`), builds a fresh address
/// space, resets the target to the new entry point, and tears down the old AS.
/// Target-taking (like `SYS_FORK`). On success the target is resumed at the new
/// image with no reply; failures return a negative errno to PM.
pub const SYS_EXEC: i32 = KERNEL_CALL + 3;
pub const SYS_EXIT: i32 = KERNEL_CALL + 4;
/// Raw privileged cross-address-space copy (MINIX 3 `sys_datacopy`), real as of
/// slice 5.2. Payload: source endpoint in `0..4` (i32, `SELF` allowed),
/// destination endpoint in `4..8` (i32), source address in `16..24` (u64),
/// destination address in `24..32` (u64), byte count in `32..40` (u64).
/// No grant is involved and there is no per-target authorization — the
/// `k_call_mask` gate is the whole check, the `SYS_VMCTL` trust stance. Used
/// for small control-plane reads (e.g. VFS fetching a caller's path string).
pub const SYS_COPY: i32 = KERNEL_CALL + 5;
/// Grant-mediated cross-address-space copy, real as of slice 5.2. One call
/// number covers both directions via the selector in payload `0..4`
/// ([`SAFECOPY_FROM`] / [`SAFECOPY_TO`]). Payload: direction in `0..4` (i32),
/// granter endpoint in `4..8` (i32), grant id in `8..12` (i32), offset within
/// the granted range in `16..24` (u64), the *caller's* buffer address in
/// `24..32` (u64), and the byte count in `32..40` (u64). The kernel reads the
/// grant entry out of the granter's own address space and validates kind,
/// access, grantee, sequence, and range before copying.
pub const SYS_SAFECOPY: i32 = KERNEL_CALL + 6;
pub const SYS_IRQCTL: i32 = KERNEL_CALL + 7;
pub const SYS_VMCTL: i32 = KERNEL_CALL + 8;
pub const SYS_SCHEDULE: i32 = KERNEL_CALL + 9;
pub const SYS_SETALARM: i32 = KERNEL_CALL + 10;
pub const SYS_TIMES: i32 = KERNEL_CALL + 11;
/// Server → kernel: the servers' debug channel (slice 5.1). Subcode in payload
/// `0..4` (i32); for `DIAGCTL_CODE_DIAG`, the text length in `4..8` (i32) and
/// the text itself inline in `DIAG_TEXT_OFF..DIAG_TEXT_OFF+len`. The kernel
/// prints one console line per call, prefixed with the *caller's own* name as
/// the kernel knows it. Caller-local. MINIX 3 (`kernel/system/do_diagctl.c`)
/// passes a `(buf, len)` user pointer here; minix.rs carries the text inline
/// so the debug channel needs no user-copy machinery and cannot fault — it has
/// to work while the copy engine and grants are themselves under construction.
pub const SYS_DIAGCTL: i32 = KERNEL_CALL + 12;
/// Server → kernel: register the caller's grant table (slice 5.2). Payload:
/// entry count in `4..8` (i32; 0 clears the registration) and the table's base
/// address *in the caller's own address space* in `16..24` (u64). The kernel
/// records `(addr, entries)` in the caller's `Priv` and reads entries back out
/// of that address space on each `SYS_SAFECOPY`, so the granter pays no kernel
/// memory and may revoke unilaterally. Caller-local in effect, but it mutates
/// the caller's privilege slot, so it is routed with the target-taking calls.
/// A process on a *shared* privilege slot is rejected (`EPERM`): one table
/// address cannot describe several processes' memory.
pub const SYS_SETGRANT: i32 = KERNEL_CALL + 13;
/// Scheduler claim/release. A user-space scheduler (SCHED) calls this to take a
/// target proc under its management (`target.scheduler = caller`) or hand it
/// back to the kernel scheduler (`SCHEDCTL_FLAG_KERNEL`). Made real in slice 4.3
/// alongside `SYS_SCHEDULE`; payload layout mirrors `SYS_VMCTL` (flags in
/// `0..4`, target endpoint in `4..8`).
pub const SYS_SCHEDCTL: i32 = KERNEL_CALL + 14;
/// Raise a signal on a target proc (slice 4.5). Target endpoint in payload
/// `0..4` (i32), signal number in `4..8` (i32, `1..NSIG`). The kernel records
/// the signal in the target's pending bitmap (`cause_sig`) and notifies PM,
/// which drains via `SYS_GETKSIG` / `SYS_ENDKSIG`. This is the MINIX 3
/// non-PM-caller semantics (queue toward PM); PM's own direct-delivery branch
/// (`send_sig` to a system proc) is deferred until a consumer exists.
pub const SYS_KILL: i32 = KERNEL_CALL + 15;
/// PM → kernel: fetch the next proc with pending kernel signals (slice 4.5).
/// Reply payload: target endpoint in `0..4` (i32; `NONE` when nothing is
/// pending) and the pending-signal bitmap in `4..8` (u32). The kernel hands
/// the bitmap off (clears `Proc::sig_pending`) but leaves the target's
/// signal-pending RTS state set until `SYS_ENDKSIG` acknowledges it.
pub const SYS_GETKSIG: i32 = KERNEL_CALL + 16;
/// PM → kernel: signal processing for the target (payload `0..4`, i32) is
/// complete — clear its signal-pending RTS state (slice 4.5).
pub const SYS_ENDKSIG: i32 = KERNEL_CALL + 17;

/// `SYS_SCHEDCTL` flag: revert the target to kernel scheduling
/// (`target.scheduler = NONE`). Absent → the caller claims the target as its
/// own scheduler. Matches MINIX 3 `SCHEDCTL_FLAG_KERNEL` (`include/minix/com.h`).
pub const SCHEDCTL_FLAG_KERNEL: i32 = 1 << 0;

// ---------------------------------------------------------------------------
// `SYS_SAFECOPY` direction selector (payload `0..4`).
//
// One kernel-call number covers both directions, MINIX-style, because the
// validation and the copy engine are identical either way — only the source and
// destination swap. Numbered from 1 so a zeroed payload is an obvious
// "invalid", the `VMCTL_*` / `PRIVCTL_*` / `DIAGCTL_*` convention.
// ---------------------------------------------------------------------------

/// Copy *out of* the granted range into the caller's buffer. Requires
/// `CPF_READ` on the grant.
pub const SAFECOPY_FROM: i32 = 1;
/// Copy *into* the granted range from the caller's buffer. Requires
/// `CPF_WRITE` on the grant.
pub const SAFECOPY_TO: i32 = 2;

/// Length of the boot-embedded binary name carried in the `SYS_EXEC` payload
/// (`4..4+EXEC_NAME_LEN`), NUL-padded. Sized to fit the MXBI record name field
/// (`< 20` bytes); a short name like `"worker"` fits with room to spare.
pub const EXEC_NAME_LEN: usize = 16;

/// Number of kernel calls defined. Reached 18 in Phase 4 (slice 4.3 made
/// `SYS_SCHEDULE` real and added `SYS_SCHEDCTL`; slice 4.5 added the signal
/// trio `SYS_KILL` / `SYS_GETKSIG` / `SYS_ENDKSIG`) and stays there through
/// Phase 5, which adds no new call numbers — it fills in bodies for calls that
/// were already numbered and `ENOSYS`-stubbed.
///
/// Named `NR_KERN_CALLS_PHASE4` until slice 5.1. The generated C header always
/// emitted it as `NR_KERN_CALLS`, and the two must not diverge past the slice
/// 5.6 ABI freeze.
pub const NR_KERN_CALLS: usize = 18;

/// Size of the privilege-table kernel-call mask, in bits. Sized as a single
/// `u32` chunk (32 slots) to leave headroom past Phase 4's 15 calls while
/// keeping the bitmap a single word per privilege slot.
pub const NR_SYS_CALLS: usize = 32;

const _: () = assert!(NR_SYS_CALLS >= NR_KERN_CALLS);
const _: () = assert!(NR_SYS_CALLS.is_multiple_of(32));

// ---------------------------------------------------------------------------
// `SYS_GETINFO` request sub-types.
//
// `SYS_GETINFO` is a multi-purpose introspection call: the request sub-type
// in the first 4 bytes of the message payload selects what the kernel reports
// back. Numbering matches MINIX 3 `include/minix/sysinfo.h` so the same wire
// values can be reused once musl + servers land.
// ---------------------------------------------------------------------------

/// `SYS_GETINFO` request: return the caller's endpoint, priv flags, init
/// flags, and process name. The kernel writes the reply into the payload of
/// the request message in-place; on return `m_type == OK`.
pub const GET_WHOAMI: i32 = 12;

/// Length of the `name` field in the `GET_WHOAMI` reply payload. minix.rs uses
/// the kernel's own `PROC_NAME_LEN` here rather than MINIX 3's 44-byte field —
/// the name is only used for debug/log output and the kernel never stores more
/// than 16 bytes per slot.
pub const SYS_GETINFO_NAME_LEN: usize = 16;

// ---------------------------------------------------------------------------
// `SYS_PRIVCTL` subcodes.
//
// `SYS_PRIVCTL` (real as of slice 4.5) sets up a target proc's privilege
// slot. The target endpoint lives in payload `0..4` and the subcode in `4..8`
// (both i32, the same target-first convention as `SYS_SCHEDULE`). Numbers
// start at 1 so a zeroed payload is an obvious "invalid" (the `VMCTL_*`
// convention). Modeled on MINIX 3 `SYS_PRIV_SET_USER`; the system-proc
// variants (`SET_SYS`, range grants) arrive with RS service starts.
// ---------------------------------------------------------------------------

/// Point a frozen (`RTS_NO_PRIV`) target at the shared USER privilege slot
/// and release it. The USER slot carries `USR_T` traps, `ipc_to` = {PM, VFS}, and
/// an empty kernel-call mask — ordinary user processes make no kernel calls.
/// The 4.6 fork path leans on this to hand forked children a privilege.
pub const PRIVCTL_SET_USER: i32 = 1;

// ---------------------------------------------------------------------------
// `SYS_DIAGCTL` subcodes + inline-text payload geometry.
//
// The subcode lives in payload `0..4` (the `GET_WHOAMI` / `VMCTL_*`
// convention), numbered from 1 so a zeroed payload is an obvious "invalid".
// Numbering follows MINIX 3's `DIAGCTL_CODE_*` order; only `DIAG` is
// implemented in Phase 5, the rest are reserved so their wire values cannot be
// reused by a later minix.rs-specific code.
// ---------------------------------------------------------------------------

/// Print inline text to the kernel console. Length in payload `4..8`, text in
/// `DIAG_TEXT_OFF..DIAG_TEXT_OFF+len`.
pub const DIAGCTL_CODE_DIAG: i32 = 1;
/// Reserved (MINIX 3 `DIAGCTL_CODE_STACKTRACE`) — `EINVAL` in Phase 5.
pub const DIAGCTL_CODE_STACKTRACE: i32 = 2;
/// Reserved (MINIX 3 `DIAGCTL_CODE_REGISTER`) — `EINVAL` in Phase 5.
pub const DIAGCTL_CODE_REGISTER: i32 = 3;
/// Reserved (MINIX 3 `DIAGCTL_CODE_UNREGISTER`) — `EINVAL` in Phase 5.
pub const DIAGCTL_CODE_UNREGISTER: i32 = 4;

/// Offset of the inline text within the `SYS_DIAGCTL` payload. Follows the
/// subcode (`0..4`) and length (`4..8`) words, so the text starts 8-aligned.
pub const DIAG_TEXT_OFF: usize = 8;

/// Maximum inline text bytes carried by one `SYS_DIAGCTL(DIAGCTL_CODE_DIAG)`
/// call — the rest of the 96-byte payload. `server-rt::diag_print` splits
/// longer strings across successive calls, one console line each.
pub const DIAG_TEXT_MAX: usize = 96 - DIAG_TEXT_OFF;

const _: () = assert!(DIAG_TEXT_OFF + DIAG_TEXT_MAX == 96);

// ---------------------------------------------------------------------------
// `SYS_VMCTL` subcalls.
//
// `SYS_VMCTL` mediates all user-space page-table changes: the kernel owns the
// physical frame allocator and every unsafe PTE write, and VM (slice 3.4)
// drives policy by issuing these subcalls. The subcall selector lives in the
// first 4 bytes of the message payload (same convention as `GET_WHOAMI`); the
// target process is named by an endpoint in the next 4 bytes (`SELF` allowed).
// Numbers start at 1 so a zeroed payload (subcall 0) is an obvious "invalid".
// These are minix.rs-specific — MINIX 3's VMCTL subcall set differs because its
// frame allocator lives in VM, not the kernel.
// ---------------------------------------------------------------------------

/// Allocate a fresh zeroed frame and map it at `vaddr` in the target's
/// address space with the requested protection. The allocated PA is returned
/// in the reply payload. (The kernel allocates because the frame allocator is
/// kernel-side; VM supplies only `vaddr` + protection.)
pub const VMCTL_PT_MAP: i32 = 1;
/// Unmap `vaddr` in the target's address space and free the backing frame.
pub const VMCTL_PT_UNMAP: i32 = 2;
/// Clear the target's pending page fault and make it runnable again.
pub const VMCTL_CLEAR_PAGEFAULT: i32 = 3;
/// Read the target's recorded page-fault state (addr/flags/ip) into the reply.
/// Valid only while the target is blocked on a page fault.
pub const VMCTL_GET_PAGEFAULT: i32 = 4;
/// Inhibit scheduling of the target while VM mutates its address space.
pub const VMCTL_VMINHIBIT_SET: i32 = 5;
/// Release a prior `VMCTL_VMINHIBIT_SET`.
pub const VMCTL_VMINHIBIT_CLEAR: i32 = 6;

/// Number of `SYS_VMCTL` subcalls. Locks the dispatch-match coverage in
/// `system::do_vmctl` via a const-assert.
pub const NR_VMCTL_SUBCALLS: usize = 6;

// `VMCTL_PT_MAP` protection bits (message payload, `vaddr`-adjacent word).
/// EL0 may write the mapped page.
pub const VMCTL_PROT_WRITE: i32 = 1 << 0;
/// EL0 may execute from the mapped page.
pub const VMCTL_PROT_EXEC: i32 = 1 << 1;

// ---------------------------------------------------------------------------
// PM (process manager) server request numbers — `m_type` values for messages
// addressed to the PM server (slice 4.5).
//
// Like the VM/DS/SEF/SCHED ranges these are *server IPC requests*, not kernel
// calls. SCHED's `0xF00` block is the last one below the IPC `NOTIFY_MESSAGE`
// marker (`0x1000`), so PM takes the free gap between the kernel-call range
// (`0x600..0x618`) and VM (`0xC00`). Numbering is minix.rs-specific — MINIX 3
// carries PM call numbers in `callnr.h`; those ABI numbers arrive with the
// musl wrappers in Phase 5.
// ---------------------------------------------------------------------------

/// Base for PM server request `m_type` values.
pub const PM_RQ_BASE: i32 = 0x700;

/// Client → PM: return the caller's process id. No request payload — the
/// kernel-stamped `m_source` names the caller. Reply: `m_type` *is* the pid
/// (MINIX convention: the result is the pid, >= 0; errors are negative, e.g.
/// `ESRCH` for a caller unknown to PM's mproc table), with the parent's pid
/// in payload `0..4` (i32) so `getppid` needs no second call.
pub const PM_GETPID: i32 = PM_RQ_BASE;

/// User → PM: `fork()`. No request payload — the kernel-stamped `m_source` names
/// the parent. PM allocates a child slot, drives `SYS_FORK` + `VM_FORK` +
/// `SCHEDULING_START` + `SYS_PRIVCTL(PRIVCTL_SET_USER)`, then replies to **both**
/// halves of the shared blocked SENDREC: the child receives `m_type = 0`, the
/// parent receives `m_type = child_pid` (MINIX fork-returns-twice). On failure
/// the parent's reply carries a negative errno (`EAGAIN` if PM's process table
/// is full). (slice 4.6b)
pub const PM_FORK: i32 = PM_RQ_BASE + 1;

/// User → PM: `exit(status)`. Exit status in payload `0..4` (i32). PM tears the
/// caller down (`SCHEDULING_STOP` + `SYS_EXIT`) and either wakes a `wait()`ing
/// parent or leaves a zombie for the parent's next `wait()`. The caller is dead
/// after `SYS_EXIT`, so PM sends **no** reply. (slice 4.6b)
pub const PM_EXIT: i32 = PM_RQ_BASE + 2;

/// User → PM: `wait()` for any child. No request payload — `m_source` names the
/// parent. If a zombie child exists PM reaps it and replies immediately;
/// otherwise, with a live child, PM suspends the caller (no reply) until the
/// child exits. Reply: `m_type` *is* the reaped child's pid (>= 0), with the
/// encoded exit status in payload `0..4` (i32, `(status & 0xff) << 8`), or
/// `ECHILD` in `m_type` if the caller has no children. (slice 4.6b)
pub const PM_WAIT: i32 = PM_RQ_BASE + 3;

/// User → PM: `execve()`. The caller names its own target: the payload carries
/// the boot-image module name at [`PM_EXEC_NAME_OFF`]`..+`[`EXEC_NAME_LEN`],
/// NUL-padded, and PM passes it straight through to `SYS_EXEC` (whose kernel
/// side has been name-driven since slice 4.7). An all-NUL name is `EINVAL`.
///
/// PM sends **no** reply on success (the kernel resumes the caller at the new
/// image's entry point); on failure the reply `m_type` carries a negative errno
/// and the caller continues in its old image.
///
/// Slice 4.7 had no payload at all — PM hardcoded `"worker"`. Slice 5.6 moved the
/// choice to the caller so `init` can alternate `worker` and `hello`, which is
/// what keeps 5.5's exec-stack probe alive alongside the C milestone. A real
/// filesystem *path* (rather than a boot-image module name) arrives with
/// slice 5.9. (slice 5.6)
pub const PM_EXEC: i32 = PM_RQ_BASE + 4;

/// Offset of the target's boot-image module name in a [`PM_EXEC`] payload,
/// [`EXEC_NAME_LEN`] bytes, NUL-padded. Shares the kernel's `SYS_EXEC` name
/// width so PM can forward it without re-framing.
pub const PM_EXEC_NAME_OFF: usize = 0;

/// VFS → PM: the slice-5.2 grant demo. Carries a grant id in-band, which is how
/// grant ids really travel — slice 5.3's [`CDEV_WRITE`] `{minor, grant_id, len,
/// offset}` is the same shape, and takes its granter from `m_source` for the same
/// reason this does. (DS cannot carry a grant id at all: `DS_PUBLISH`
/// deliberately registers the kernel-stamped `m_source` and ignores the payload,
/// which is precisely its anti-spoof property.)
///
/// Payload: grant id in `0..4` (i32), granted length in `4..8` (i32), a second
/// grant id in `8..12` (i32) that deliberately claims `CPF_WRITE` over the same
/// read-only buffer for PM's denial probe, and the granter's raw buffer address
/// in `16..24` (u64) for the ungranted `SYS_COPY` comparison.
///
/// The **granter is not in the payload**: PM takes it from the kernel-stamped
/// `m_source`, the same anti-spoof property `DS_PUBLISH` relies on. It matters
/// here because PM holds `SYS_COPY` / `SYS_SAFECOPY` and its clients do not — a
/// caller-supplied granter endpoint would let any PM client aim a privileged
/// copy at a third party's address space through PM (a confused deputy). PM
/// additionally serves this request only when `m_source` is VFS.
///
/// Sent with `ipc_send`, so the sender blocks until PM's loop picks it up and
/// the demo is self-synchronizing; PM sends no reply. **Demo-only** — retire it
/// when a real grant consumer (CDEV, slice 5.3) lands.
pub const PM_GRANT_TEST: i32 = PM_RQ_BASE + 5;

/// Number of PM server requests defined so far. Locks the PM server's
/// dispatch coverage the way `NR_DS_REQUESTS` locks the DS server.
pub const NR_PM_MSGS: usize = 6;

// The PM range sits strictly above the kernel-call range and strictly below
// VFS's (and therefore every other) server request range and the NOTIFY marker.
const _: () = assert!(PM_RQ_BASE > KERNEL_CALL + (NR_KERN_CALLS as i32 - 1));
const _: () = assert!(PM_RQ_BASE + (NR_PM_MSGS as i32 - 1) < VFS_RQ_BASE);
const _: () = assert!(PM_RQ_BASE + (NR_PM_MSGS as i32 - 1) < crate::ipc_const::NOTIFY_MESSAGE);

// The `PM_EXEC` name fits the 96-byte payload. It shares `EXEC_NAME_LEN` with
// the kernel's `SYS_EXEC` so PM forwards the bytes without re-framing; that
// equality is what makes the forward a memcpy rather than a truncation.
const _: () = assert!(PM_EXEC_NAME_OFF + EXEC_NAME_LEN <= 96);

// ---------------------------------------------------------------------------
// VFS (virtual file system) request numbers — `m_type` values for messages
// addressed to VFS, the POSIX file-descriptor layer (slice 5.4).
//
// Like the PM/CDEV/VM/DS/SEF/SCHED ranges these are *server IPC requests*, not
// kernel calls. `0x800` was reserved for VFS when the CDEV band claimed `0xB00`,
// keeping the bands in numeric order between PM (`0x700`) and CDEV; `0x900` and
// `0xA00` stay free for the two bands Phase 5 still owes, BDEV (5.7) and MFS
// (5.8). Numbering is minix.rs-specific — MINIX 3 carries VFS's call numbers in
// `include/minix/callnr.h` as POSIX syscall numbers, a layer minix.rs does not
// have (a libc wrapper builds the server request directly).
//
// Unlike the CDEV band, the payload carries a **raw buffer address**, not a
// grant id: VFS's client is an ordinary user process with no grant table and no
// server-grade privilege. VFS is the one that grants — it issues a `CPF_MAGIC`
// grant naming the *caller's* buffer with the driver as grantee, so the bytes
// move in a single copy from the caller straight into the driver and VFS never
// touches them. The **granter of that magic grant is the kernel-stamped
// `m_source`, never a payload field**: VFS holds `SYS_PROC`, so a caller-supplied
// owner would let any VFS client aim a privileged cross-address-space copy at a
// third party (the confused-deputy rule the CDEV band's comment states, applied
// to the granting side).
// ---------------------------------------------------------------------------

/// Base for VFS server request `m_type` values.
pub const VFS_RQ_BASE: i32 = 0x800;

/// User → VFS: `write(fd, buf, len)`.
///
/// Payload: the file descriptor in [`VFS_FD_OFF`]`..+4` (i32), the byte count in
/// [`VFS_LEN_OFF`]`..+4` (i32), and the buffer's address *in the caller's own
/// address space* in [`VFS_BUF_OFF`]`..+8` (u64).
///
/// Reply `m_type` is the **number of bytes written** (`>= 0`; `0` is legal), or a
/// negative errno — identical to [`CDEV_WRITE`]'s contract, and exactly what
/// musl's `write()` returns. `EBADF` for a descriptor that is not open,
/// `EINVAL` for a negative length, `EFAULT` for a buffer outside the caller's
/// user-address range.
///
/// A short write from the underlying driver is **not** visible here: [`CDEV_MAX_IO`]
/// is a driver staging detail, so VFS re-sends `CDEV_WRITE` with `offset` advanced
/// until the buffer is out and reports the total. A partial transfer that then
/// fails still reports the bytes already written — POSIX's rule that a partial
/// success beats an error.
pub const VFS_WRITE: i32 = VFS_RQ_BASE;

/// Number of VFS server requests defined so far. Locks VFS's dispatch coverage
/// the way `NR_DS_REQUESTS` locks the DS server.
pub const NR_VFS_MSGS: usize = 1;

/// Offset of the file descriptor in a `VFS_WRITE` payload (i32).
pub const VFS_FD_OFF: usize = 0;
/// Offset of the requested byte count in a `VFS_WRITE` payload (i32).
pub const VFS_LEN_OFF: usize = 4;
/// Offset of the caller's buffer address in a `VFS_WRITE` payload (u64, so
/// 8-aligned relative to the message base — the payload itself starts at message
/// offset 8, and 8 + 8 is a multiple of 8; the same reasoning
/// [`CDEV_OFFSET_OFF`] documents).
pub const VFS_BUF_OFF: usize = 8;

// The VFS range sits strictly above the PM range and strictly below CDEV's (and
// therefore every other server request range) and the NOTIFY marker.
const _: () = assert!(VFS_RQ_BASE > PM_RQ_BASE + (NR_PM_MSGS as i32 - 1));
const _: () = assert!(VFS_RQ_BASE + (NR_VFS_MSGS as i32 - 1) < CDEV_RQ_BASE);
const _: () = assert!(VFS_RQ_BASE + (NR_VFS_MSGS as i32 - 1) < crate::ipc_const::NOTIFY_MESSAGE);

// The `VFS_WRITE` payload fields are ordered, non-overlapping, and fit the
// 96-byte payload. `VFS_BUF_OFF` is 8 wide (u64); the rest are 4 (i32).
const _: () = assert!(VFS_FD_OFF + 4 <= VFS_LEN_OFF);
const _: () = assert!(VFS_LEN_OFF + 4 <= VFS_BUF_OFF);
const _: () = assert!(VFS_BUF_OFF + 8 <= 96);
// The u64 buffer address must be 8-aligned within the message, whose payload
// starts at byte 8.
const _: () = assert!((8 + VFS_BUF_OFF).is_multiple_of(8));

// ---------------------------------------------------------------------------
// CDEV (character device) request numbers — `m_type` values for messages
// addressed to a character-device driver, TTY being the first (slice 5.3).
//
// Like the PM/VFS/VM/DS/SEF/SCHED ranges these are *server IPC requests*, not
// kernel calls. `0xB00` keeps the bands in numeric order between VFS (`0x800`,
// claimed in slice 5.4) and VM (`0xC00`) while leaving `0x900` / `0xA00` free for
// the two bands Phase 5 still owes: BDEV (5.7) and MFS (5.8). Numbering is
// minix.rs-specific — MINIX 3 carries `CDEV_*` in `include/minix/com.h` with its
// own values, which minix.rs does not inherit because its device protocol is
// narrower (no `CDEV_REPLY` message class; a driver replies to the SENDREC).
//
// The payload carries a **grant id**, not a buffer address: a driver's client
// lives in a different address space, so the bytes move via `SYS_SAFECOPY`. The
// **granter is deliberately not a payload field** — the driver takes it from the
// kernel-stamped `m_source`. A caller-supplied granter endpoint would turn any
// grant-holding driver into a confused deputy, aiming a privileged cross-address-
// space copy wherever its caller pointed; this is the same anti-spoof rule
// `DS_PUBLISH` relies on, and it binds every grant-id-carrying request in the
// CDEV, BDEV, and FS bands.
// ---------------------------------------------------------------------------

/// Base for character-device driver request `m_type` values.
pub const CDEV_RQ_BASE: i32 = 0xB00;

/// Client → character driver: write bytes to a device minor.
///
/// Payload: minor number in [`CDEV_MINOR_OFF`]`..+4` (i32), grant id in
/// [`CDEV_GRANT_OFF`]`..+4` (i32), byte count in [`CDEV_LEN_OFF`]`..+4` (i32), and
/// the offset within the granted range in [`CDEV_OFFSET_OFF`]`..+8` (u64). The
/// grant must carry `CPF_READ` and name the driver as its grantee; the driver
/// reads the bytes with `SYS_SAFECOPY(SAFECOPY_FROM, m_source, …)`.
///
/// Reply `m_type` is the **number of bytes written** (`>= 0`; `0` is legal), or a
/// negative errno. A request longer than [`CDEV_MAX_IO`] is a **short write, not
/// a failure**: the driver moves the first `CDEV_MAX_IO` bytes and reports that
/// count, and the client re-sends with `offset` advanced. That is POSIX
/// `write()`'s contract, and it is what lets a driver stage through a small
/// stack buffer with no allocator.
///
/// There is deliberately no `CDEV_READ`: RX needs interrupts (`SYS_IRQCTL`) and
/// arrives in Phase 6. Slice 5.11's `/dev/null` and `/dev/zero` are new *minors*
/// of this same request, not new request numbers.
pub const CDEV_WRITE: i32 = CDEV_RQ_BASE;

/// Number of character-device requests defined so far. Locks a driver's
/// dispatch coverage the way `NR_DS_REQUESTS` locks the DS server.
pub const NR_CDEV_MSGS: usize = 1;

/// Offset of the device minor number in a `CDEV_WRITE` payload (i32).
pub const CDEV_MINOR_OFF: usize = 0;
/// Offset of the grant id in a `CDEV_WRITE` payload (i32).
pub const CDEV_GRANT_OFF: usize = 4;
/// Offset of the requested byte count in a `CDEV_WRITE` payload (i32).
pub const CDEV_LEN_OFF: usize = 8;
/// Offset of the byte offset within the granted range in a `CDEV_WRITE` payload
/// (u64, so 8-aligned relative to the message base — the payload itself starts at
/// message offset 8, hence 16 rather than 12).
pub const CDEV_OFFSET_OFF: usize = 16;

/// The console minor: TTY's UART. Any other minor is `ENXIO` until slice 5.11
/// adds `/dev/null` and `/dev/zero`.
pub const CDEV_MINOR_CONSOLE: i32 = 0;

/// Largest byte count a character driver moves in one `CDEV_WRITE`. A longer
/// request is short-written (see [`CDEV_WRITE`]). Sized for a staging buffer in a
/// driver's `main` frame on a one-page stack.
pub const CDEV_MAX_IO: usize = 256;

// The CDEV range sits strictly above the VFS range and strictly below VM's (and
// therefore every other server request range) and the NOTIFY marker.
const _: () = assert!(CDEV_RQ_BASE > VFS_RQ_BASE + (NR_VFS_MSGS as i32 - 1));
const _: () = assert!(CDEV_RQ_BASE + (NR_CDEV_MSGS as i32 - 1) < VM_RQ_BASE);
const _: () = assert!(CDEV_RQ_BASE + (NR_CDEV_MSGS as i32 - 1) < crate::ipc_const::NOTIFY_MESSAGE);

// The `CDEV_WRITE` payload fields are ordered, non-overlapping, and fit the
// 96-byte payload. `CDEV_OFFSET_OFF` is 8 wide (u64); the rest are 4 (i32).
const _: () = assert!(CDEV_MINOR_OFF + 4 <= CDEV_GRANT_OFF);
const _: () = assert!(CDEV_GRANT_OFF + 4 <= CDEV_LEN_OFF);
const _: () = assert!(CDEV_LEN_OFF + 4 <= CDEV_OFFSET_OFF);
const _: () = assert!(CDEV_OFFSET_OFF + 8 <= 96);
// A short write must be expressible in the i32 reply `m_type`.
const _: () = assert!(CDEV_MAX_IO <= i32::MAX as usize);

// ---------------------------------------------------------------------------
// VM server request numbers — `m_type` values for messages addressed to VM.
//
// These are *server IPC requests*, not kernel calls, so they live in their own
// range distinct from `KERNEL_CALL` (`0x600`). The kernel originates
// `VM_PAGEFAULT` on a faulting process's behalf (slice 3.4); later slices add
// `VM_BRK` / `VM_MMAP` / `VM_MUNMAP`. Numbering is minix.rs-specific (MINIX 3's VM request set
// differs because its frame allocator lives in VM, not the kernel).
// ---------------------------------------------------------------------------

/// Base for VM server request `m_type` values.
pub const VM_RQ_BASE: i32 = 0xC00;

/// Kernel → VM: a process page-faulted. `m_source` identifies the faulting
/// process; the payload carries the fault address (`0..8`, u64) and fault
/// flags (`8..12`, u32). VM resolves it via `SYS_VMCTL(VMCTL_PT_MAP)` +
/// `SYS_VMCTL(VMCTL_CLEAR_PAGEFAULT)`.
pub const VM_PAGEFAULT: i32 = VM_RQ_BASE;

/// EL0 → VM: set the caller's program break to `new_break` (payload `0..8`,
/// u64). VM grows or creates the caller's heap region to `[HEAP_BASE,
/// new_break)`; pages fault in lazily on first touch (no eager mapping). The
/// reply carries `m_type = OK` and the resulting break in payload `0..8`, or a
/// negative error in `m_type`. (slice 3.5)
pub const VM_BRK: i32 = VM_RQ_BASE + 1;

/// EL0 → VM: anonymous mmap. The caller requests `len` bytes (payload `0..8`,
/// u64); VM page-aligns the length, picks a free VA from the caller's mmap bump
/// arena, records an `Mmap` region, and replies with the chosen base address in
/// payload `0..8` and `m_type = OK`. Like `mmap(NULL, len, …)`: VM chooses the
/// address. Pages fault in lazily on first touch (no eager mapping). On failure
/// the negative error is in `m_type` (`EINVAL` for a zero or overflowing
/// length, `ENOMEM` when no region slot is free). (slice 3.6)
pub const VM_MMAP: i32 = VM_RQ_BASE + 2;

/// EL0 → VM: unmap a prior mmap. The caller passes the base address (payload
/// `0..8`, u64) and length (payload `8..16`, u64). VM page-aligns the range,
/// drops the matching `Mmap` region, and unmaps each backing page via
/// `SYS_VMCTL(VMCTL_PT_UNMAP)` (a never-faulted page returns a harmless
/// `EINVAL` from the kernel, which VM ignores). The reply carries
/// `m_type = OK`, or `EINVAL` in `m_type` if no `Mmap` region matches the base
/// address. (slice 3.6)
pub const VM_MUNMAP: i32 = VM_RQ_BASE + 3;

/// PM → VM: clone a parent's memory region set into a freshly forked child.
/// The kernel already copied the child's page tables (`SYS_FORK`); this copies
/// VM's own bookkeeping so the child's later brk/mmap/fault lookups see the
/// inherited heap/mmap regions. Payload: parent endpoint (`0..4`, i32), child
/// endpoint (`4..8`, i32). Reply `m_type = OK`, or `EINVAL` in `m_type` if
/// either endpoint maps to an out-of-range proc number. (slice 4.6b)
pub const VM_FORK: i32 = VM_RQ_BASE + 4;

// ---------------------------------------------------------------------------
// DS (Data Store) server request numbers — `m_type` values for messages
// addressed to the DS server.
//
// DS is a name→endpoint registry: every server publishes its own endpoint at
// init (slice 4.2) so others can look each other up without hard-coding boot
// proc numbers. These are *server IPC requests* like the VM range, so they live
// in their own range, distinct from `KERNEL_CALL` (`0x600`), the VM request
// range (`VM_RQ_BASE = 0xC00`), and the SEF control range (`SEF_RQ_BASE =
// 0xD00`), and stay below the IPC `NOTIFY_MESSAGE` marker (`0x1000`) so neither
// a server's `m_type` dispatcher nor the SEF classifier can ever misroute.
//
// The key (a NUL-padded server name) travels inline in the request payload
// (`0..SYS_GETINFO_NAME_LEN`); no grants / cross-AS copy are needed because the
// kernel copies the whole 96-byte payload on delivery. An endpoint value rides
// in payload `16..20` (i32, native-endian) only on `DS_RETRIEVE` replies;
// `DS_PUBLISH` registers the caller's kernel-stamped `m_source` (a process can
// only publish itself), so no endpoint is sent in a publish request. Numbering
// is minix.rs-specific.
// ---------------------------------------------------------------------------

/// Base for DS server request `m_type` values.
pub const DS_RQ_BASE: i32 = 0xE00;

/// Server → DS: publish the *caller's own* endpoint under the key in payload
/// `0..SYS_GETINFO_NAME_LEN`. DS records the caller's kernel-stamped `m_source`,
/// not a value from the payload, so a process can only publish itself and can
/// never spoof another server's endpoint. Re-publishing the same key updates the
/// stored endpoint. Reply `m_type = OK`, or `EINVAL` (empty key) / `ENOMEM`
/// (registry full).
pub const DS_PUBLISH: i32 = DS_RQ_BASE;

/// Client → DS: look up the endpoint for the key in payload `0..NAME_LEN`.
/// Reply `m_type = OK` with the endpoint in payload `16..20` (i32), or
/// `ESRCH` if the key is not registered.
pub const DS_RETRIEVE: i32 = DS_RQ_BASE + 1;

/// Client → DS: test whether the key in payload `0..NAME_LEN` is registered.
/// Reply `m_type = OK` with a status in payload `16..20` (i32: 1 = present,
/// 0 = absent) — absence is a status, not an error, so a `CHECK` never aborts
/// the caller's SENDREC.
pub const DS_CHECK: i32 = DS_RQ_BASE + 2;

/// Number of DS server requests defined so far. Locks the dispatch-match
/// coverage in the DS server the way `NR_VMCTL_SUBCALLS` locks `do_vmctl`.
pub const NR_DS_REQUESTS: usize = 3;

// The DS range sits strictly above the SEF range (0xD00..0xD01) so a server's
// `m_type` dispatcher and the SEF classifier can never collide, and stays
// below the NOTIFY marker.
const _: () = assert!(DS_RQ_BASE > SEF_RQ_BASE + (NR_SEF_MSGS as i32 - 1));
const _: () = assert!(DS_RQ_BASE + (NR_DS_REQUESTS as i32 - 1) < crate::ipc_const::NOTIFY_MESSAGE);

// ---------------------------------------------------------------------------
// SEF (System Event Framework) control message numbers — `m_type` values the
// server runtime (`server-rt`) intercepts before handing traffic to a server.
//
// These live in their own range distinct from `KERNEL_CALL` (`0x600`) and the
// VM request range (`VM_RQ_BASE = 0xC00`), and stay below the IPC
// `NOTIFY_MESSAGE` marker (`0x1000`), so neither a server's `m_type`
// dispatcher nor the SEF classifier can ever misroute. Numbering is
// minix.rs-specific (MINIX 3 carries these inside `lib/libsys/sef.c` request
// types rather than `com.h`).
//
// The RS heartbeat ("ping") deliberately gets NO number here: it is delivered
// as a NOTIFY, so it arrives with `m_type == NOTIFY_MESSAGE` and is keyed on
// `m_source == RS` instead (see `server-rt`'s `classify`). Do not add a
// `SEF_PING` — there is no payload room in a NOTIFY to carry one anyway.
// ---------------------------------------------------------------------------

/// Base for SEF control message `m_type` values.
pub const SEF_RQ_BASE: i32 = 0xD00;

/// RS → server: run the registered fresh-init callback. (Re-init / live-update
/// variants are deferred past Phase 4.)
pub const SEF_INIT: i32 = SEF_RQ_BASE;

/// PM/RS → server: deliver a signal. The signal number is in payload `0..4`
/// (i32, native-endian); `server-rt` dispatches it to the registered signal
/// handler.
pub const SEF_SIGNAL: i32 = SEF_RQ_BASE + 1;

/// Number of SEF control messages defined so far. Locks the classifier's
/// coverage in `server-rt` the way `NR_VMCTL_SUBCALLS` locks `do_vmctl`.
pub const NR_SEF_MSGS: usize = 2;

// The SEF range sits strictly above the VM request range (0xC00..0xC04) so a
// server's `m_type` dispatcher and the SEF classifier can never collide.
const _: () = assert!(SEF_RQ_BASE > VM_RQ_BASE + 4);
const _: () = assert!(SEF_RQ_BASE < crate::ipc_const::NOTIFY_MESSAGE);

// ---------------------------------------------------------------------------
// SCHED (scheduler) server request numbers — `m_type` values for messages
// addressed to the user-space SCHED server (slice 4.3).
//
// `SCHEDULING_NO_QUANTUM` is kernel-originated: when a SCHED-scheduled proc
// exhausts its quantum, the kernel sends it (with `m_source` = the preempted
// proc, so SCHED knows which proc to reschedule), exactly as it originates
// `VM_PAGEFAULT` for a faulter. The other three are PM/RS → SCHED requests
// (claim/release/renice a managed proc). Like the VM/DS ranges these are
// *server IPC requests*, not kernel calls, so they live in their own range
// distinct from `KERNEL_CALL` (`0x600`), VM (`0xC00`), SEF (`0xD00`), and DS
// (`0xE00`), and stay below the IPC `NOTIFY_MESSAGE` marker (`0x1000`) so the
// SEF classifier (which returns `Application` for them) can never misroute.
// Numbering is minix.rs-specific (MINIX 3 carries `SCHEDULING_*` in `com.h`).
// ---------------------------------------------------------------------------

/// Base for SCHED server request `m_type` values.
pub const SCHED_RQ_BASE: i32 = 0xF00;

/// Kernel → SCHED: the proc identified by `m_source` used up its full quantum.
/// SCHED applies its policy and re-admits the proc via `SYS_SCHEDULE`. Carries
/// no payload — `m_source` is the whole request.
pub const SCHEDULING_NO_QUANTUM: i32 = SCHED_RQ_BASE;

/// PM/RS → SCHED: start scheduling a proc. The target endpoint is in payload
/// `0..4` (i32), the initial priority in `4..8` (i32), and the quantum (ms) in
/// `8..12` (i32). SCHED claims the target via `SYS_SCHEDCTL` and assigns the
/// initial priority/quantum via `SYS_SCHEDULE`. (Driven by PM/RS from slice 4.5+.)
pub const SCHEDULING_START: i32 = SCHED_RQ_BASE + 1;

/// PM/RS → SCHED: stop scheduling the target (payload `0..4`, i32). SCHED hands
/// it back to the kernel scheduler via `SYS_SCHEDCTL(SCHEDCTL_FLAG_KERNEL)`.
pub const SCHEDULING_STOP: i32 = SCHED_RQ_BASE + 2;

/// PM/RS → SCHED: change the target's nice value. Target endpoint in payload
/// `0..4` (i32), new priority in `4..8` (i32). SCHED records it and applies it
/// via `SYS_SCHEDULE`.
pub const SCHEDULING_SET_NICE: i32 = SCHED_RQ_BASE + 3;

/// Number of SCHED server requests defined so far. Locks the dispatch-match
/// coverage in the SCHED server the way `NR_DS_REQUESTS` locks the DS server.
pub const NR_SCHED_MSGS: usize = 4;

// The SCHED range sits strictly above the DS range (0xE00..0xE02) and below the
// NOTIFY marker, so neither a server's `m_type` dispatcher nor the SEF
// classifier can ever collide with it.
const _: () = assert!(SCHED_RQ_BASE > DS_RQ_BASE + (NR_DS_REQUESTS as i32 - 1));
const _: () =
    assert!(SCHED_RQ_BASE + (NR_SCHED_MSGS as i32 - 1) < crate::ipc_const::NOTIFY_MESSAGE);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_are_contiguous_from_base() {
        let calls = [
            SYS_GETINFO,
            SYS_PRIVCTL,
            SYS_FORK,
            SYS_EXEC,
            SYS_EXIT,
            SYS_COPY,
            SYS_SAFECOPY,
            SYS_IRQCTL,
            SYS_VMCTL,
            SYS_SCHEDULE,
            SYS_SETALARM,
            SYS_TIMES,
            SYS_DIAGCTL,
            SYS_SETGRANT,
            SYS_SCHEDCTL,
            SYS_KILL,
            SYS_GETKSIG,
            SYS_ENDKSIG,
        ];
        for (i, call) in calls.iter().enumerate() {
            assert_eq!(*call, KERNEL_CALL + i as i32);
        }
        assert_eq!(calls.len(), NR_KERN_CALLS);
    }

    #[test]
    fn kernel_call_base_matches_minix3() {
        assert_eq!(KERNEL_CALL, 0x600);
    }

    #[test]
    fn get_whoami_matches_minix3() {
        // Pinned by MINIX 3 include/minix/sysinfo.h; servers / musl wrappers
        // built later in the project depend on this value.
        assert_eq!(GET_WHOAMI, 12);
    }

    #[test]
    fn vmctl_subcalls_are_contiguous_from_one() {
        // Subcall 0 is reserved as "invalid" (a zeroed payload). The six
        // real subcalls are 1..=6 and distinct; `NR_VMCTL_SUBCALLS` locks
        // the dispatch coverage in `system::do_vmctl`.
        let subcalls = [
            VMCTL_PT_MAP,
            VMCTL_PT_UNMAP,
            VMCTL_CLEAR_PAGEFAULT,
            VMCTL_GET_PAGEFAULT,
            VMCTL_VMINHIBIT_SET,
            VMCTL_VMINHIBIT_CLEAR,
        ];
        for (i, sc) in subcalls.iter().enumerate() {
            assert_eq!(*sc, 1 + i as i32);
        }
        assert_eq!(subcalls.len(), NR_VMCTL_SUBCALLS);
    }

    #[test]
    fn diagctl_subcodes_are_contiguous_from_one() {
        // Subcode 0 is reserved as "invalid" (a zeroed payload), matching the
        // VMCTL/PRIVCTL convention. Only DIAG is implemented in Phase 5; the
        // rest are reserved so a later minix.rs code cannot reuse their wire
        // values and diverge from MINIX 3's numbering.
        let codes = [
            DIAGCTL_CODE_DIAG,
            DIAGCTL_CODE_STACKTRACE,
            DIAGCTL_CODE_REGISTER,
            DIAGCTL_CODE_UNREGISTER,
        ];
        for (i, code) in codes.iter().enumerate() {
            assert_eq!(*code, 1 + i as i32);
        }
    }

    #[test]
    fn diag_text_fills_the_rest_of_the_payload() {
        let payload_len = crate::message::Message {
            m_source: 0,
            m_type: 0,
            payload: [0; 96],
        }
        .payload
        .len();
        assert_eq!(DIAG_TEXT_OFF + DIAG_TEXT_MAX, payload_len);
        // The text starts 8-aligned relative to the message base (payload is
        // at offset 8), so a future typed accessor can overlay u64 fields.
        assert_eq!((8 + DIAG_TEXT_OFF) % 8, 0);
        assert_eq!(DIAG_TEXT_MAX, 88);
    }

    #[test]
    fn vm_pagefault_distinct_from_kernel_calls_and_notify() {
        // VM requests must not collide with the KERNEL_CALL range, the IPC
        // NOTIFY_MESSAGE marker, or any SYS_* number — a server dispatcher
        // keys on m_type and a collision would misroute.
        assert_eq!(VM_PAGEFAULT, VM_RQ_BASE);
        assert!(VM_PAGEFAULT > KERNEL_CALL + NR_KERN_CALLS as i32);
        assert_ne!(VM_PAGEFAULT, crate::ipc_const::NOTIFY_MESSAGE);
    }

    #[test]
    fn vm_brk_follows_pagefault_in_request_range() {
        // VM_BRK is the second VM server request, contiguous after VM_PAGEFAULT.
        // It must stay distinct from the page-fault request, the KERNEL_CALL
        // range, and the NOTIFY marker so VM's m_type dispatcher can't misroute.
        assert_eq!(VM_BRK, VM_RQ_BASE + 1);
        assert_ne!(VM_BRK, VM_PAGEFAULT);
        assert!(VM_BRK > KERNEL_CALL + NR_KERN_CALLS as i32);
        assert_ne!(VM_BRK, crate::ipc_const::NOTIFY_MESSAGE);
    }

    #[test]
    fn vm_mmap_follows_brk_in_request_range() {
        // VM_MMAP is the third VM server request, contiguous after VM_BRK.
        assert_eq!(VM_MMAP, VM_RQ_BASE + 2);
        assert_ne!(VM_MMAP, VM_PAGEFAULT);
        assert_ne!(VM_MMAP, VM_BRK);
        assert!(VM_MMAP > KERNEL_CALL + NR_KERN_CALLS as i32);
        assert_ne!(VM_MMAP, crate::ipc_const::NOTIFY_MESSAGE);
    }

    #[test]
    fn vm_munmap_follows_mmap_in_request_range() {
        // VM_MUNMAP is the fourth VM server request, contiguous after VM_MMAP.
        // Each VM request must stay distinct from the others, the KERNEL_CALL
        // range, and the NOTIFY marker so VM's m_type dispatcher can't misroute.
        assert_eq!(VM_MUNMAP, VM_RQ_BASE + 3);
        assert_ne!(VM_MUNMAP, VM_MMAP);
        assert_ne!(VM_MUNMAP, VM_BRK);
        assert_ne!(VM_MUNMAP, VM_PAGEFAULT);
        assert!(VM_MUNMAP > KERNEL_CALL + NR_KERN_CALLS as i32);
        assert_ne!(VM_MUNMAP, crate::ipc_const::NOTIFY_MESSAGE);
    }

    #[test]
    fn vm_fork_follows_munmap_in_request_range() {
        // VM_FORK is the fifth VM server request, contiguous after VM_MUNMAP.
        assert_eq!(VM_FORK, VM_RQ_BASE + 4);
        assert_ne!(VM_FORK, VM_MUNMAP);
        assert_ne!(VM_FORK, VM_MMAP);
        assert_ne!(VM_FORK, VM_BRK);
        assert_ne!(VM_FORK, VM_PAGEFAULT);
        assert!(VM_FORK > KERNEL_CALL + NR_KERN_CALLS as i32);
        // (The VM_FORK < SEF_RQ_BASE ordering is locked by a module-level
        // const-assert, so it needs no runtime assertion here.)
        assert_ne!(VM_FORK, crate::ipc_const::NOTIFY_MESSAGE);
    }

    #[test]
    fn ds_requests_contiguous_from_base() {
        // DS requests are contiguous from DS_RQ_BASE; NR_DS_REQUESTS locks the
        // DS server's dispatch coverage.
        let reqs = [DS_PUBLISH, DS_RETRIEVE, DS_CHECK];
        for (i, r) in reqs.iter().enumerate() {
            assert_eq!(*r, DS_RQ_BASE + i as i32);
        }
        assert_eq!(reqs.len(), NR_DS_REQUESTS);
    }

    #[test]
    fn ds_requests_distinct_from_other_ranges() {
        // Each DS request must stay distinct from the VM request range, the SEF
        // control range, and the KERNEL_CALL range, and below NOTIFY_MESSAGE —
        // so a server's m_type dispatcher and the SEF classifier never collide.
        for r in [DS_PUBLISH, DS_RETRIEVE, DS_CHECK] {
            for vm in [VM_PAGEFAULT, VM_BRK, VM_MMAP, VM_MUNMAP, VM_FORK] {
                assert_ne!(r, vm);
            }
            assert_ne!(r, VFS_WRITE);
            assert_ne!(r, CDEV_WRITE);
            assert_ne!(r, SEF_INIT);
            assert_ne!(r, SEF_SIGNAL);
            assert!(r > SEF_RQ_BASE + (NR_SEF_MSGS as i32 - 1));
            assert!(r > KERNEL_CALL + NR_KERN_CALLS as i32);
            assert_ne!(r, crate::ipc_const::NOTIFY_MESSAGE);
            assert!(r < crate::ipc_const::NOTIFY_MESSAGE);
        }
    }

    #[test]
    fn sef_msgs_contiguous_from_base() {
        // SEF control messages are contiguous from SEF_RQ_BASE; NR_SEF_MSGS
        // locks `server-rt`'s classifier coverage.
        let msgs = [SEF_INIT, SEF_SIGNAL];
        for (i, m) in msgs.iter().enumerate() {
            assert_eq!(*m, SEF_RQ_BASE + i as i32);
        }
        assert_eq!(msgs.len(), NR_SEF_MSGS);
    }

    #[test]
    fn sched_msgs_contiguous_from_base() {
        // SCHED requests are contiguous from SCHED_RQ_BASE; NR_SCHED_MSGS locks
        // the SCHED server's dispatch coverage.
        let msgs = [
            SCHEDULING_NO_QUANTUM,
            SCHEDULING_START,
            SCHEDULING_STOP,
            SCHEDULING_SET_NICE,
        ];
        for (i, m) in msgs.iter().enumerate() {
            assert_eq!(*m, SCHED_RQ_BASE + i as i32);
        }
        assert_eq!(msgs.len(), NR_SCHED_MSGS);
    }

    #[test]
    fn sched_msgs_distinct_from_other_ranges() {
        // Each SCHED request must stay distinct from the VM/DS/SEF request
        // ranges and the KERNEL_CALL range, and below NOTIFY_MESSAGE — so a
        // server's m_type dispatcher and the SEF classifier never collide.
        for m in [
            SCHEDULING_NO_QUANTUM,
            SCHEDULING_START,
            SCHEDULING_STOP,
            SCHEDULING_SET_NICE,
        ] {
            for other in [
                VFS_WRITE,
                CDEV_WRITE,
                VM_PAGEFAULT,
                VM_BRK,
                VM_MMAP,
                VM_MUNMAP,
                VM_FORK,
                DS_PUBLISH,
                DS_RETRIEVE,
                DS_CHECK,
                SEF_INIT,
                SEF_SIGNAL,
            ] {
                assert_ne!(m, other);
            }
            assert!(m > DS_RQ_BASE + (NR_DS_REQUESTS as i32 - 1));
            assert!(m > KERNEL_CALL + NR_KERN_CALLS as i32);
            assert_ne!(m, crate::ipc_const::NOTIFY_MESSAGE);
            assert!(m < crate::ipc_const::NOTIFY_MESSAGE);
        }
    }

    #[test]
    fn endksig_is_last_kernel_call() {
        // The slice-4.5 signal trio extends the Phase-4 call set; the count
        // must cover it.
        assert_eq!(SYS_KILL, KERNEL_CALL + 15);
        assert_eq!(SYS_GETKSIG, KERNEL_CALL + 16);
        assert_eq!(SYS_ENDKSIG, KERNEL_CALL + 17);
        assert_eq!(NR_KERN_CALLS, 18);
    }

    #[test]
    fn safecopy_directions_are_contiguous_from_one() {
        // Selector 0 is reserved as "invalid" (a zeroed payload), the VMCTL
        // convention — `do_safecopy` must reject it rather than defaulting to
        // a direction.
        assert_eq!(SAFECOPY_FROM, 1);
        assert_eq!(SAFECOPY_TO, 2);
        assert_ne!(SAFECOPY_FROM, SAFECOPY_TO);
    }

    #[test]
    fn privctl_set_user_is_nonzero() {
        // Subcode 0 is reserved as "invalid" (a zeroed payload), the VMCTL
        // convention.
        assert_eq!(PRIVCTL_SET_USER, 1);
    }

    #[test]
    fn pm_msgs_contiguous_from_base() {
        // PM requests are contiguous from PM_RQ_BASE; NR_PM_MSGS locks the PM
        // server's dispatch coverage.
        let msgs = [PM_GETPID, PM_FORK, PM_EXIT, PM_WAIT, PM_EXEC, PM_GRANT_TEST];
        for (i, m) in msgs.iter().enumerate() {
            assert_eq!(*m, PM_RQ_BASE + i as i32);
        }
        assert_eq!(msgs.len(), NR_PM_MSGS);
        // The whole PM range stays below VFS's (and therefore every other
        // server request range and the NOTIFY marker).
        assert!(PM_RQ_BASE + (NR_PM_MSGS as i32 - 1) < VFS_RQ_BASE);
    }

    #[test]
    fn pm_msgs_distinct_from_other_ranges() {
        // Each PM request must stay distinct from the VM/DS/SEF/SCHED request
        // ranges and the KERNEL_CALL range, and below NOTIFY_MESSAGE — so a
        // server's m_type dispatcher and the SEF classifier never collide.
        for m in [PM_GETPID, PM_FORK, PM_EXIT, PM_WAIT, PM_EXEC, PM_GRANT_TEST] {
            for other in [
                VFS_WRITE,
                CDEV_WRITE,
                VM_PAGEFAULT,
                VM_BRK,
                VM_MMAP,
                VM_MUNMAP,
                VM_FORK,
                DS_PUBLISH,
                DS_RETRIEVE,
                DS_CHECK,
                SEF_INIT,
                SEF_SIGNAL,
                SCHEDULING_NO_QUANTUM,
                SCHEDULING_START,
                SCHEDULING_STOP,
                SCHEDULING_SET_NICE,
            ] {
                assert_ne!(m, other);
            }
            assert!(m > KERNEL_CALL + NR_KERN_CALLS as i32 - 1);
            assert!(m < VFS_RQ_BASE);
            assert_ne!(m, crate::ipc_const::NOTIFY_MESSAGE);
            assert!(m < crate::ipc_const::NOTIFY_MESSAGE);
        }
    }

    #[test]
    fn vfs_msgs_contiguous_from_base() {
        // VFS requests are contiguous from VFS_RQ_BASE; NR_VFS_MSGS locks the
        // VFS server's dispatch coverage.
        let msgs = [VFS_WRITE];
        for (i, m) in msgs.iter().enumerate() {
            assert_eq!(*m, VFS_RQ_BASE + i as i32);
        }
        assert_eq!(msgs.len(), NR_VFS_MSGS);
        assert_eq!(VFS_RQ_BASE, 0x800);
    }

    #[test]
    fn vfs_msgs_distinct_from_other_ranges() {
        // Each VFS request must stay distinct from every other band and the
        // KERNEL_CALL range, and below NOTIFY_MESSAGE — so a server's m_type
        // dispatcher and the SEF classifier never collide. VFS sits between PM
        // and CDEV, which is what its two bounds assert.
        for m in [VFS_WRITE] {
            for other in [
                PM_GETPID,
                PM_FORK,
                PM_EXIT,
                PM_WAIT,
                PM_EXEC,
                PM_GRANT_TEST,
                CDEV_WRITE,
                VM_PAGEFAULT,
                VM_BRK,
                VM_MMAP,
                VM_MUNMAP,
                VM_FORK,
                DS_PUBLISH,
                DS_RETRIEVE,
                DS_CHECK,
                SEF_INIT,
                SEF_SIGNAL,
                SCHEDULING_NO_QUANTUM,
                SCHEDULING_START,
                SCHEDULING_STOP,
                SCHEDULING_SET_NICE,
            ] {
                assert_ne!(m, other);
            }
            assert!(m > PM_RQ_BASE + (NR_PM_MSGS as i32 - 1));
            assert!(m > KERNEL_CALL + NR_KERN_CALLS as i32 - 1);
            assert!(m < CDEV_RQ_BASE);
            assert_ne!(m, crate::ipc_const::NOTIFY_MESSAGE);
            assert!(m < crate::ipc_const::NOTIFY_MESSAGE);
        }
    }

    #[test]
    fn vfs_write_payload_offsets_are_ordered_and_disjoint() {
        // Every field the request defines, in declaration order, with its width:
        // the i32 fields are 4 bytes, the u64 buffer address is 8.
        //
        // Unlike `CDEV_WRITE` this payload carries a raw buffer address rather
        // than a grant id — VFS's client is an ordinary user process with no
        // grant table. VFS makes the grant itself, naming the caller's buffer,
        // and takes the owner from the kernel-stamped `m_source`. There is
        // deliberately no owner/granter field here for the same reason there is
        // none in `CDEV_WRITE`: VFS holds `SYS_PROC`, so a caller-supplied owner
        // would aim a privileged cross-address-space copy at a third party.
        let fields = [
            ("fd", VFS_FD_OFF, 4),
            ("len", VFS_LEN_OFF, 4),
            ("buf", VFS_BUF_OFF, 8),
        ];
        assert_eq!(fields.len(), 3, "a VFS_WRITE payload field was added");
        assert_eq!(VFS_FD_OFF, 0, "the first field must start the payload");

        for pair in fields.windows(2) {
            let (name, off, width) = pair[0];
            let (next_name, next_off, _) = pair[1];
            assert!(
                off + width <= next_off,
                "{name} ({off}..{}) overlaps {next_name} at {next_off}",
                off + width,
            );
        }
        let (last_name, last_off, last_width) = fields[fields.len() - 1];
        assert!(
            last_off + last_width <= 96,
            "{last_name} runs past the 96-byte payload",
        );

        // The u64 field must be 8-aligned *within the message*, whose payload
        // starts at byte 8 — so an even multiple of 8 here.
        assert_eq!((8 + VFS_BUF_OFF) % 8, 0);
    }

    #[test]
    fn cdev_msgs_contiguous_from_base() {
        // CDEV requests are contiguous from CDEV_RQ_BASE; NR_CDEV_MSGS locks a
        // character driver's dispatch coverage.
        let msgs = [CDEV_WRITE];
        for (i, m) in msgs.iter().enumerate() {
            assert_eq!(*m, CDEV_RQ_BASE + i as i32);
        }
        assert_eq!(msgs.len(), NR_CDEV_MSGS);
    }

    #[test]
    fn cdev_msgs_distinct_from_other_ranges() {
        // Each CDEV request must stay distinct from every other band and the
        // KERNEL_CALL range, and below NOTIFY_MESSAGE — so a driver's m_type
        // dispatcher and the SEF classifier never collide.
        for m in [CDEV_WRITE] {
            for other in [
                PM_GETPID,
                PM_FORK,
                PM_EXIT,
                PM_WAIT,
                PM_EXEC,
                PM_GRANT_TEST,
                VFS_WRITE,
                VM_PAGEFAULT,
                VM_BRK,
                VM_MMAP,
                VM_MUNMAP,
                VM_FORK,
                DS_PUBLISH,
                DS_RETRIEVE,
                DS_CHECK,
                SEF_INIT,
                SEF_SIGNAL,
                SCHEDULING_NO_QUANTUM,
                SCHEDULING_START,
                SCHEDULING_STOP,
                SCHEDULING_SET_NICE,
            ] {
                assert_ne!(m, other);
            }
            assert!(m > VFS_RQ_BASE + (NR_VFS_MSGS as i32 - 1));
            assert!(m > KERNEL_CALL + NR_KERN_CALLS as i32 - 1);
            assert!(m < VM_RQ_BASE);
            assert_ne!(m, crate::ipc_const::NOTIFY_MESSAGE);
            assert!(m < crate::ipc_const::NOTIFY_MESSAGE);
        }
    }

    #[test]
    fn cdev_band_sits_at_0xb00_leaving_two_bands_free() {
        // The band base is load-bearing for numeric ordering: VFS (0x800, taken
        // in slice 5.4) below, VM (0xC00) above, with 0x900 / 0xA00 still
        // reserved for the two bands Phase 5 owes (BDEV 5.7, MFS 5.8). Nothing
        // may claim one of those without also moving whichever band would then
        // be out of numeric order.
        assert_eq!(CDEV_RQ_BASE, 0xB00);
        for reserved in [0x900, 0xA00] {
            for taken in [
                PM_RQ_BASE,
                VFS_RQ_BASE,
                CDEV_RQ_BASE,
                VM_RQ_BASE,
                SEF_RQ_BASE,
                DS_RQ_BASE,
                SCHED_RQ_BASE,
            ] {
                assert_ne!(reserved, taken, "{reserved:#x} is no longer free");
            }
        }
    }

    #[test]
    fn cdev_write_payload_offsets_are_ordered_and_disjoint() {
        // Every field the request defines, in declaration order, with its width:
        // i32 fields are 4 bytes, the u64 offset field is 8.
        //
        // There is deliberately NO granter field. The driver takes the granter
        // from the kernel-stamped `m_source`; a payload granter would let a client
        // aim the driver's privileged `SYS_SAFECOPY` at a third party's address
        // space (a confused deputy). This list is the record of that — adding a
        // field means editing it, and the length assertion below is what makes
        // "adding a granter" a visible change rather than a quiet one.
        let fields = [
            ("minor", CDEV_MINOR_OFF, 4),
            ("grant", CDEV_GRANT_OFF, 4),
            ("len", CDEV_LEN_OFF, 4),
            ("offset", CDEV_OFFSET_OFF, 8),
        ];
        assert_eq!(fields.len(), 4, "a CDEV_WRITE payload field was added");
        assert_eq!(CDEV_MINOR_OFF, 0, "the first field must start the payload");

        for pair in fields.windows(2) {
            let (name, off, width) = pair[0];
            let (next_name, next_off, _) = pair[1];
            assert!(
                off + width <= next_off,
                "{name} ({off}..{}) overlaps {next_name} at {next_off}",
                off + width,
            );
        }
        let (last_name, last_off, last_width) = fields[fields.len() - 1];
        assert!(
            last_off + last_width <= 96,
            "{last_name} runs past the 96-byte payload",
        );

        // The u64 field must be 8-aligned *within the message*, whose payload
        // starts at byte 8 — so an even multiple of 8 here.
        assert_eq!((8 + CDEV_OFFSET_OFF) % 8, 0);
    }

    #[test]
    fn cdev_max_io_fits_the_reply() {
        // The reply `m_type` carries the byte count as an i32, so a full-size
        // transfer must round-trip through i32 — a count that overflowed would
        // land in the negative, errno-shaped band and read as a failure.
        assert_eq!(i32::try_from(CDEV_MAX_IO), Ok(256));
        assert_eq!(CDEV_MAX_IO, 256);
        assert_eq!(CDEV_MINOR_CONSOLE, 0);
    }

    #[test]
    fn sef_msgs_distinct_from_vm_kernel_and_notify_ranges() {
        // Each SEF control message must stay distinct from the VM request
        // range, the KERNEL_CALL range, and the NOTIFY marker — and below
        // NOTIFY_MESSAGE — so a server's m_type dispatcher and the SEF
        // classifier can never collide. (The base-vs-VM-range ordering is
        // additionally locked by a module-level const-assert.)
        for m in [SEF_INIT, SEF_SIGNAL] {
            assert_ne!(m, VFS_WRITE);
            assert_ne!(m, CDEV_WRITE);
            assert_ne!(m, VM_PAGEFAULT);
            assert_ne!(m, VM_BRK);
            assert_ne!(m, VM_MMAP);
            assert_ne!(m, VM_MUNMAP);
            assert_ne!(m, VM_FORK);
            assert!(m > KERNEL_CALL + NR_KERN_CALLS as i32);
            assert_ne!(m, crate::ipc_const::NOTIFY_MESSAGE);
            assert!(m < crate::ipc_const::NOTIFY_MESSAGE);
        }
    }
}
