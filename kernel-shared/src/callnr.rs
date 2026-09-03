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
/// PM → kernel: replace a target proc's program image (slice 4.7, extended in
/// 5.9). Target endpoint in payload `0..4` (i32); `argv[0]` / the new proc name
/// (NUL-padded, [`EXEC_NAME_LEN`] bytes) in `4..4+EXEC_NAME_LEN`; a source
/// selector in [`EXEC_SRC_OFF`]`..+4`.
///
/// Two source forms, and the selector is what tells them apart:
///
/// * [`EXEC_SRC_NAME`] — the `4..20` field doubles as an MXBI module name, which
///   the kernel resolves with `BootImage::module_by_name`. The slice-4.7 form.
/// * [`EXEC_SRC_GRANT`] — the image's bytes live in a *granted* buffer described
///   by [`EXEC_GRANTER_OFF`] / [`EXEC_GRANT_OFF`] / [`EXEC_LEN_OFF`], and the
///   kernel reads them through the grant with the ordinary `verify_grant`
///   validator. This is what makes exec-from-FS possible without a kernel
///   filesystem: VFS stages the file and grants it to PM, PM names the grant here
///   (decision D6 — the kernel keeps ELF authority, PM/VFS do the staging).
///
/// In **both** forms `4..20` is `argv[0]` and the proc's new name; only where the
/// image's bytes come from changes. That is what keeps [`EXEC_NAME_LEN`],
/// `PROC_NAME_LEN`, and the initial stack's geometry untouched by 5.9 — PM passes
/// the path's *basename*, never the path.
///
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

/// Length of the `argv[0]` / proc-name field carried in the `SYS_EXEC` payload
/// (`4..4+EXEC_NAME_LEN`), NUL-padded. Sized to fit the MXBI record name field
/// (`< 20` bytes); a short name like `"worker"` fits with room to spare.
///
/// In the [`EXEC_SRC_NAME`] form this field is *also* the module name the kernel
/// resolves. In the [`EXEC_SRC_GRANT`] form it is only the name — slice 5.9's
/// deliberate choice not to carry a user `argv`, which is what leaves this width,
/// `PROC_NAME_LEN`, and `execstack::INITIAL_STACK_MAX` all unchanged by
/// exec-from-FS.
pub const EXEC_NAME_LEN: usize = 16;

// ---------------------------------------------------------------------------
// `SYS_EXEC` source selector and the grant triple (slice 5.9, decision D6).
//
// Numbered from 1 so a zeroed payload is an obvious "invalid" rather than
// defaulting to a form — the `SAFECOPY_FROM` / `SAFECOPY_TO` convention, and the
// same reason `VMCTL_*` / `PRIVCTL_*` / `DIAGCTL_*` start at 1.
//
// There is deliberately **no grant-offset field**: the granted buffer holds the
// image starting at offset 0. That is the rule the BDEV and FS bands already
// state — a field nothing sets is a field nothing validates.
// ---------------------------------------------------------------------------

/// Offset of the `SYS_EXEC` source selector (i32).
pub const EXEC_SRC_OFF: usize = 20;

/// Source selector: resolve the `4..20` name against the MXBI boot archive.
pub const EXEC_SRC_NAME: i32 = 1;
/// Source selector: read the image out of the grant described by
/// [`EXEC_GRANTER_OFF`] / [`EXEC_GRANT_OFF`] / [`EXEC_LEN_OFF`].
pub const EXEC_SRC_GRANT: i32 = 2;

/// Offset of the granting process's endpoint in an [`EXEC_SRC_GRANT`] payload
/// (i32).
///
/// Unlike every *device* request in this file, the granter **is** a payload field
/// here, and that is not a confused deputy: `SYS_EXEC`'s caller is PM, a
/// server-grade process the kernel already trusts with a target-taking call, and
/// the grant it names was issued *to PM* by VFS. The kernel re-validates the whole
/// grant with the ordinary `verify_grant` — `who_to` must be PM's own stored
/// endpoint — so naming someone else's grant here buys nothing. The rule those
/// device bands state is about a *server* taking a granter from its own client;
/// this is the kernel taking one from a caller holding the kernel-call bit.
pub const EXEC_GRANTER_OFF: usize = 24;
/// Offset of the grant id in an [`EXEC_SRC_GRANT`] payload (i32).
pub const EXEC_GRANT_OFF: usize = 28;
/// Offset of the image's byte length in an [`EXEC_SRC_GRANT`] payload (u64, so
/// 8-aligned relative to the message base — the payload starts at message offset
/// 8, hence 32; the reasoning [`CDEV_OFFSET_OFF`] documents).
pub const EXEC_LEN_OFF: usize = 32;

// The `SYS_EXEC` payload fields are ordered, non-overlapping, and fit the 96-byte
// payload. `EXEC_LEN_OFF` is 8 wide (u64); the rest are 4 (i32), except the name
// field, which is `EXEC_NAME_LEN`.
const _: () = assert!(4 + EXEC_NAME_LEN <= EXEC_SRC_OFF);
const _: () = assert!(EXEC_SRC_OFF + 4 <= EXEC_GRANTER_OFF);
const _: () = assert!(EXEC_GRANTER_OFF + 4 <= EXEC_GRANT_OFF);
const _: () = assert!(EXEC_GRANT_OFF + 4 <= EXEC_LEN_OFF);
const _: () = assert!(EXEC_LEN_OFF + 8 <= 96);
const _: () = assert!((8 + EXEC_LEN_OFF).is_multiple_of(8));
// Selector 0 must stay invalid, so neither form may take it.
const _: () = assert!(EXEC_SRC_NAME != 0 && EXEC_SRC_GRANT != 0);
const _: () = assert!(EXEC_SRC_NAME != EXEC_SRC_GRANT);

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
// back.
//
// **Provenance, stated accurately.** Modern MINIX 3 keeps these selectors in
// `include/minix/com.h`, numbered `0..=25`, where `GET_WHOAMI` is 19 — not 12.
// minix.rs's 12 is its own value and always was; the comment that used to claim
// otherwise (and named `include/minix/sysinfo.h`, which carries the *libsys*
// wrappers rather than the numbers) was simply wrong. The value stays as it is
// past the slice-5.6 ABI freeze — C in the musl fork depends on it — so the fix
// is to the claim, not to the number.
//
// `0..=31` is reserved for selectors that mirror a MINIX 3 one, so a future
// import can keep MINIX's number. minix.rs-specific selectors start at 32;
// [`GET_RAMDISK`] is the first, at 64.
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

/// `SYS_GETINFO` request: report where the kernel mapped the boot ramdisk into
/// the caller's address space, and how long it is (slice 5.7).
///
/// Reply payload: the VA in [`GETINFO_RAMDISK_VA_OFF`]`..+8` (u64) and the byte
/// length in [`GETINFO_RAMDISK_LEN_OFF`]`..+8` (u64), with `m_type == OK`.
///
/// **Gated on the caller being the `memory` driver** (`MEM_PROC_NR`); anyone
/// else gets `EPERM`. The ramdisk is pre-mapped into exactly one address space,
/// so the VA is meaningless — and actively misleading — anywhere else. There is
/// no new kernel call and no new kernel state behind this: the VA is a constant
/// (`uspace::RAMDISK_VA`) and the length is the `rootfs` MXBI module's.
///
/// minix.rs-specific, hence 64 rather than a number in the MINIX-mirrored
/// `0..=31` block.
pub const GET_RAMDISK: i32 = 64;

/// Offset of the ramdisk VA in a [`GET_RAMDISK`] reply payload (u64).
pub const GETINFO_RAMDISK_VA_OFF: usize = 0;
/// Offset of the ramdisk byte length in a [`GET_RAMDISK`] reply payload (u64).
pub const GETINFO_RAMDISK_LEN_OFF: usize = 8;

// minix.rs-specific selectors live clear of the `0..=31` block reserved for
// MINIX 3-mirrored numbers.
const _: () = assert!(GET_RAMDISK > 31);
const _: () = assert!(GET_RAMDISK != GET_WHOAMI);

// Both reply fields are u64 and must be 8-aligned within the message, whose
// payload starts at byte 8.
const _: () = assert!(GETINFO_RAMDISK_VA_OFF + 8 <= GETINFO_RAMDISK_LEN_OFF);
const _: () = assert!(GETINFO_RAMDISK_LEN_OFF + 8 <= 96);
const _: () = assert!((8 + GETINFO_RAMDISK_VA_OFF).is_multiple_of(8));
const _: () = assert!((8 + GETINFO_RAMDISK_LEN_OFF).is_multiple_of(8));

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

/// User → PM: `execve()`. The caller names its own target inline, at
/// [`PM_EXEC_PATH_OFF`]`..+`[`PM_EXEC_PATH_MAX`], NUL-padded. An all-NUL field is
/// `EINVAL`; a field with no NUL anywhere is `ENAMETOOLONG`, never a truncation
/// that could resolve somewhere else.
///
/// **A leading `/` is the discriminator** (slice 5.9): an absolute path names a
/// file in the root filesystem, and anything else names a boot-image module. One
/// field rather than a path plus a form flag, because two fields can disagree —
/// and it is already the FS band's rule, where `walk::parse_path` answers
/// `EINVAL` to a relative path since minix.rs has no working directory. It also
/// settles the warning on `com::ROOTFS_MODULE_NAME`: module names and paths are
/// disjoint namespaces, so nothing that resolves a *path* can ever name the
/// `rootfs` blob.
///
/// For a path, PM asks VFS to stage the file ([`VFS_EXEC_STAGE`]) and then hands
/// the kernel the resulting grant; for a module name it forwards the name as it
/// has since 4.7. Either way `argv[0]` is the **basename**, which is why the
/// kernel's [`EXEC_NAME_LEN`] field does not have to grow.
///
/// PM sends **no** reply on success (the kernel resumes the caller at the new
/// image's entry point); on failure the reply `m_type` carries a negative errno
/// and the caller continues in its old image — so a failed exec is also the
/// rollback proof.
///
/// Slice 4.7 had no payload at all (PM hardcoded `"worker"`); 5.6 moved the
/// choice to the caller as a 16-byte module name; 5.9 widened that field to a
/// path. (slice 5.9)
pub const PM_EXEC: i32 = PM_RQ_BASE + 4;

/// Offset of the target's path or module name in a [`PM_EXEC`] payload.
pub const PM_EXEC_PATH_OFF: usize = 0;

/// Width of that field, NUL-padded — so the longest path that can travel is
/// `PM_EXEC_PATH_MAX - 1` bytes.
///
/// Equal to [`FS_PATH_MAX`] deliberately: the path's next hop is
/// [`VFS_EXEC_STAGE`] and then `FS_LOOKUP`, both of which carry it inline in a
/// field of exactly that width, so a path that fits here fits the whole way down
/// and PM never has to refuse one the filesystem would have accepted.
pub const PM_EXEC_PATH_MAX: usize = FS_PATH_MAX;

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

// The `PM_EXEC` path fills its own field and fits the 96-byte payload. It shares
// `FS_PATH_MAX` with the FS band, so a path PM accepts survives every hop down to
// `FS_LOOKUP` without being re-framed or truncated.
const _: () = assert!(PM_EXEC_PATH_OFF + PM_EXEC_PATH_MAX <= 96);
const _: () = assert!(PM_EXEC_PATH_MAX == FS_PATH_MAX);
// A module name still has to fit the kernel's `SYS_EXEC` field, and so does a
// path's basename — PM refuses a longer one rather than truncating it.
const _: () = assert!(EXEC_NAME_LEN <= PM_EXEC_PATH_MAX);

// ---------------------------------------------------------------------------
// VFS (virtual file system) request numbers — `m_type` values for messages
// addressed to VFS, the POSIX file-descriptor layer (slice 5.4).
//
// Like the PM/FS/CDEV/VM/DS/SEF/SCHED ranges these are *server IPC requests*,
// not kernel calls. `0x800` was reserved for VFS when the CDEV band claimed
// `0xB00`, keeping the bands in numeric order between PM (`0x700`) and CDEV.
// With the FS band claiming `0x900` in slice 5.8 the `0x700..0xC00` span is now
// **fully allocated** — PM, VFS, FS, BDEV, CDEV — so a new band needs a new home
// rather than a reserved slot. Numbering is minix.rs-specific — MINIX 3 carries
// VFS's call numbers in `include/minix/callnr.h` as POSIX syscall numbers, a
// layer minix.rs does not have (a libc wrapper builds the server request
// directly).
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

/// User → VFS: `open(path, flags)`.
///
/// Payload: the path buffer's address *in the caller's own address space* in
/// [`VFS_PATH_OFF`]`..+8` (u64), its length in [`VFS_PATH_LEN_OFF`]`..+4` (i32),
/// and the open flags in [`VFS_FLAGS_OFF`]`..+4` (i32, [`crate::fcntl`]'s
/// values). VFS reads the path bytes out with `SYS_COPY`; a user process has no
/// grant table to describe them with.
///
/// Reply `m_type` is the **new descriptor** (`>= 0`), or a negative errno:
/// `ENOENT` for a path that does not exist and `O_CREAT` was not given,
/// `EISDIR` for a directory (there is no `O_DIRECTORY` and nothing that could
/// read one), `ENOTDIR` for an intermediate component that is not a directory,
/// `ENAMETOOLONG` past [`FS_PATH_MAX`], `EINVAL` for a relative or empty path
/// or a flag bit outside [`crate::fcntl::O_KNOWN`], `EMFILE` when the caller's
/// descriptor row is full, `ENODEV` when no filesystem is mounted.
///
/// **Every descriptor this request hands out is readable and writable alike**
/// — slice 5.10a made writes real without needing a flags field, because the
/// file it wrote to was already in the image. The flags field arrives with
/// slice 5.10b: `O_CREAT` names a file that does not exist yet (dispatched to
/// [`FS_CREATE`] after a lookup answers `ENOENT`), and `O_TRUNC` discards an
/// existing file's contents (dispatched to [`FS_TRUNC`]) — both acted on by
/// VFS, not by the FS server's [`FS_LOOKUP`] path. The access-mode bits
/// (`O_RDONLY`/`O_WRONLY`/`O_RDWR`) are accepted and ignored, per
/// [`crate::fcntl::O_KNOWN`]'s doc comment. The lowest free descriptor is
/// returned, which is POSIX's rule and what makes the `fs.fd` boot probe (open,
/// open, close both, re-open) mean anything.
pub const VFS_OPEN: i32 = VFS_RQ_BASE + 1;

/// User → VFS: `read(fd, buf, len)`.
///
/// Payload is **[`VFS_WRITE`]'s, field for field** — descriptor, byte count,
/// buffer address — because the two requests differ only in which way the bytes
/// travel. One set of offsets, one parser, one validator.
///
/// Reply `m_type` is the **number of bytes read** (`>= 0`), or a negative errno.
/// `0` is end of file, not an error: no size is cached anywhere along the path, so
/// this is the only way a client learns where a file ends. The descriptor's
/// position advances by the count returned.
///
/// A short read is possible and legal — `FS_MAX_IO` bounds one FS transfer — and
/// unlike [`VFS_WRITE`], VFS does **not** loop to hide it. `read()` is allowed to
/// return less than asked for; `write()` is not, which is the whole asymmetry.
pub const VFS_READ: i32 = VFS_RQ_BASE + 2;

/// User → VFS: `close(fd)`.
///
/// Payload: the descriptor in [`VFS_FD_OFF`]`..+4` (i32). Reply `m_type` is `OK`
/// or `EBADF`.
///
/// Closing a descriptor frees its slot for the next [`VFS_OPEN`], which is what
/// makes "the lowest free descriptor" observable at all. Nothing else happens:
/// MFS keeps no per-open state, so there is no FS-side request to send.
pub const VFS_CLOSE: i32 = VFS_RQ_BASE + 3;

/// PM → VFS: read a whole executable into VFS's staging buffer and grant it
/// back (slice 5.9, decision D6).
///
/// Payload: the path inline at [`VFS_EXEC_PATH_OFF`], NUL-padded to
/// [`FS_PATH_MAX`]. **Inline, unlike [`VFS_OPEN`]'s pointer-and-length**, and the
/// reason is the client: PM already holds the path inline in the `PM_EXEC` it is
/// serving, so passing it by value costs no `SYS_COPY` — and it deletes the
/// confused-deputy question outright, because there is no source process for a
/// caller to misname.
///
/// Reply `m_type` is the **file's byte count** (`>= 0`, the band's rule since
/// slice 5.4), with the grant id at [`VFS_EXEC_GRANT_OFF`] in the reply payload.
/// The grant is a **direct** one over VFS's own staging buffer carrying
/// `CPF_READ`, whose grantee is the kernel-stamped `m_source` — there is no
/// payload field for the grantee and there must never be one.
///
/// Errors: `EPERM` for any caller but PM (nothing else has business asking VFS to
/// stage an image), `EINVAL` for a relative or empty path, `ENAMETOOLONG` for a
/// field with no NUL, `ENOENT` / `ENOTDIR` from the lookup, `EISDIR` for a
/// directory, `ENOMEM` for a file larger than [`VFS_EXEC_MAX`], `ENODEV` when
/// nothing is mounted, and `EIO` for a stream that ended early.
///
/// **Nothing releases the grant, and nothing needs to.** Each request re-grants
/// the same buffer, which bumps the sequence and kills the previous id; PM
/// serialises exec, so there is never a second staged image alive at once.
pub const VFS_EXEC_STAGE: i32 = VFS_RQ_BASE + 4;

/// Number of VFS server requests defined so far. Locks VFS's dispatch coverage
/// the way `NR_DS_REQUESTS` locks the DS server.
pub const NR_VFS_MSGS: usize = 5;

/// Offset of the file descriptor in a `VFS_WRITE` / `VFS_READ` / `VFS_CLOSE`
/// payload (i32).
pub const VFS_FD_OFF: usize = 0;
/// Offset of the requested byte count in a `VFS_WRITE` / `VFS_READ` payload (i32).
pub const VFS_LEN_OFF: usize = 4;
/// Offset of the caller's buffer address in a `VFS_WRITE` / `VFS_READ` payload
/// (u64, so 8-aligned relative to the message base — the payload itself starts at
/// message offset 8, and 8 + 8 is a multiple of 8; the same reasoning
/// [`CDEV_OFFSET_OFF`] documents).
pub const VFS_BUF_OFF: usize = 8;

/// Offset of the inline path in a [`VFS_EXEC_STAGE`] payload ([`FS_PATH_MAX`]
/// NUL-padded bytes). A separate message again, so byte 0 is free.
pub const VFS_EXEC_PATH_OFF: usize = 0;
/// Offset of the grant id in a [`VFS_EXEC_STAGE`] **reply** (i32).
pub const VFS_EXEC_GRANT_OFF: usize = 0;

/// Largest executable VFS will stage for a [`VFS_EXEC_STAGE`].
///
/// 256 KiB — a cap on a `.bss` buffer VFS carries for the whole run, so it is
/// sized against the largest thing that has to fit rather than against what would
/// be convenient. That is the **musl-flavour** `hello` at ~200 KB, not the SDK
/// one at ~46 KB: no CI job installs the SDK, so the musl flavour is what
/// `qemu-smoke` actually builds and the only one this number may be tuned to.
/// `kernel/build.rs` asserts the built bytes fit and names this constant when
/// they do not, the `ROOTFS_IMAGE_BLOCKS` precedent.
///
/// Sits beside [`CDEV_MAX_IO`] / [`BDEV_MAX_IO`] / [`FS_MAX_IO`] as the fourth
/// transfer cap in this file, and is the only one that is not a staging chunk: a
/// short stage is useless, because an ELF cannot be loaded in pieces by a loader
/// that has no filesystem.
pub const VFS_EXEC_MAX: usize = 256 * 1024;

/// Offset of the caller's path-buffer address in a `VFS_OPEN` payload (u64, and
/// 8-aligned within the message for the reason [`VFS_BUF_OFF`] gives).
///
/// `VFS_OPEN` is a different message from `VFS_WRITE`, so sharing byte 0 with
/// [`VFS_FD_OFF`] is not an overlap — the FS band's `FS_SUPER_MINOR_OFF` /
/// `FS_SUPER_ROOT_OFF` pair does the same thing.
pub const VFS_PATH_OFF: usize = 0;
/// Offset of the path's length in a `VFS_OPEN` payload (i32).
pub const VFS_PATH_LEN_OFF: usize = 8;
/// Offset of the open flags in a `VFS_OPEN` payload (i32).
///
/// Values are [`crate::fcntl`]'s, which are musl's. A new *field* on an existing
/// request rather than a new request, so [`NR_VFS_MSGS`] does not move.
pub const VFS_FLAGS_OFF: usize = 12;

// The VFS range sits strictly above the PM range and strictly below FS's (and
// therefore every other server request range) and the NOTIFY marker.
const _: () = assert!(VFS_RQ_BASE > PM_RQ_BASE + (NR_PM_MSGS as i32 - 1));
const _: () = assert!(VFS_RQ_BASE + (NR_VFS_MSGS as i32 - 1) < FS_RQ_BASE);
const _: () = assert!(VFS_RQ_BASE + (NR_VFS_MSGS as i32 - 1) < crate::ipc_const::NOTIFY_MESSAGE);

// The `VFS_WRITE` / `VFS_READ` payload fields are ordered, non-overlapping, and
// fit the 96-byte payload. `VFS_BUF_OFF` is 8 wide (u64); the rest are 4 (i32).
const _: () = assert!(VFS_FD_OFF + 4 <= VFS_LEN_OFF);
const _: () = assert!(VFS_LEN_OFF + 4 <= VFS_BUF_OFF);
const _: () = assert!(VFS_BUF_OFF + 8 <= 96);
// The u64 buffer address must be 8-aligned within the message, whose payload
// starts at byte 8.
const _: () = assert!((8 + VFS_BUF_OFF).is_multiple_of(8));

// The `VFS_OPEN` payload, likewise. A separate message, so it may reuse byte 0.
const _: () = assert!(VFS_PATH_OFF + 8 <= VFS_PATH_LEN_OFF);
const _: () = assert!(VFS_PATH_LEN_OFF + 4 <= VFS_FLAGS_OFF);
const _: () = assert!(VFS_FLAGS_OFF + 4 <= 96);
const _: () = assert!((8 + VFS_PATH_OFF).is_multiple_of(8));

// The `VFS_EXEC_STAGE` request's inline path and its reply's grant id each fill
// their own message, so both may start at byte 0.
const _: () = assert!(VFS_EXEC_PATH_OFF + FS_PATH_MAX <= 96);
const _: () = assert!(VFS_EXEC_GRANT_OFF + 4 <= 96);
// A staged image's byte count must round-trip through the i32 reply `m_type`, or
// it would land in the negative, errno-shaped band and read as a failure...
const _: () = assert!(VFS_EXEC_MAX <= i32::MAX as usize);
// ...and the kernel must be willing to map what VFS is willing to stage, or a
// file inside this cap could still be refused with `EINVAL` by `do_exec`.
const _: () = assert!(VFS_EXEC_MAX <= crate::execimage::MAX_IMAGE_BYTES);

// ---------------------------------------------------------------------------
// FS (file system) request numbers — `m_type` values for messages addressed to a
// file-system server, MFS being the first and only one (slice 5.8).
//
// Like the PM/VFS/BDEV/CDEV/VM/DS/SEF/SCHED ranges these are *server IPC
// requests*, not kernel calls. `0x900` was reserved for this band when VFS took
// `0x800` and BDEV `0xA00`, so it slots in between them and the numeric ordering
// the whole scheme rests on is preserved. Numbering is minix.rs-specific — MINIX
// 3 carries its VFS↔FS protocol in `include/minix/vfsif.h` with a far larger
// request set (PUTNODE, STAT, GETDENTS, the whole write path), none of which
// minix.rs inherits: this band is exactly the three requests a read-only open →
// read → close needs, on the `CDEV_READ` precedent that a request absent until it
// has a consumer is better than a request stubbed out.
//
// Two shapes travel here, and the split is deliberate:
//
// **The path travels inline** ([`FS_PATH_OFF`], NUL-padded to [`FS_PATH_MAX`]),
// not by grant. It is control plane, not the data path D4 provisioned grants
// for — the same call the `PM_EXEC` name and the `DS_PUBLISH` key already make.
// It costs the FS server no staging buffer, which is its scarcest resource (one
// page of stack, all of it already spoken for by the block buffer), and it
// deletes the confused-deputy question outright, because there is no granter to
// name. The cost is the cap: a path longer than [`FS_PATH_MAX`] is
// `ENAMETOOLONG`, refused by VFS before it reaches the wire.
//
// **The data travels by grant** ([`FS_GRANT_OFF`]), and as everywhere else in
// this file there is **no granter field and no grant-offset field** — the server
// takes the granter from the kernel-stamped `m_source` (the confused-deputy rule
// the CDEV band's comment states in full), and VFS issues a fresh grant over the
// remaining tail each round rather than advancing an offset.
// ---------------------------------------------------------------------------

/// Base for file-system server request `m_type` values.
pub const FS_RQ_BASE: i32 = 0x900;

/// VFS → FS: mount a block-device minor and report the geometry of what is on it.
///
/// Payload: the block-device minor in [`FS_SUPER_MINOR_OFF`]`..+4` (i32).
///
/// Reply `m_type` is `OK` or a negative errno (`EINVAL` for a device that holds
/// no filesystem this build reads, `EIO` for a device that could not be read).
/// On success the reply payload carries the root inode number in
/// [`FS_SUPER_ROOT_OFF`]`..+4` (i32), the filesystem's block size in
/// [`FS_SUPER_BLOCK_SIZE_OFF`]`..+4` (i32), and its size in blocks in
/// [`FS_SUPER_BLOCKS_OFF`]`..+4` (i32).
///
/// Those three are what make the reply worth having: a client that only wanted
/// "did it mount" would take the `m_type`. They are the numbers a boot marker can
/// cross-check against the *device*'s independently derived geometry.
pub const FS_READSUPER: i32 = FS_RQ_BASE;

/// VFS → FS: resolve an absolute path to an inode.
///
/// Payload: the path in [`FS_PATH_OFF`]`..+`[`FS_PATH_MAX`], NUL-**padded** (not
/// merely NUL-terminated: the server reads the whole fixed field and stops at the
/// first NUL, so trailing bytes cannot leak between requests).
///
/// Reply `m_type` is `OK` or a negative errno — `ENOENT` for a component that
/// does not exist, `ENOTDIR` for an intermediate component that is not a
/// directory, `ENAMETOOLONG` for a path that fills the field with no NUL,
/// `EINVAL` for a relative path, `ENODEV` if nothing is mounted. On success the
/// reply carries the inode number in [`FS_INO_OFF`]`..+4` (i32), the inode's mode
/// in [`FS_MODE_OFF`]`..+4` (i32, widened from MinixFS's `u16`), and its size in
/// [`FS_SIZE_OFF`]`..+4` (i32).
///
/// Mode and size ride along so there is **no separate stat request**: the two
/// facts a client needs before reading — is this a directory, and how big is it —
/// are exactly what a lookup already had to decode.
pub const FS_LOOKUP: i32 = FS_RQ_BASE + 1;

/// VFS → FS: read from an inode into the client's granted buffer.
///
/// Payload: the inode number in [`FS_INO_OFF`]`..+4` (i32), the grant id in
/// [`FS_GRANT_OFF`]`..+4` (i32), the byte count in [`FS_LEN_OFF`]`..+4` (i32), and
/// the file offset in [`FS_POS_OFF`]`..+8` (u64). The grant must carry `CPF_WRITE`
/// and name the FS server as its grantee.
///
/// Reply `m_type` is the **number of bytes read** (`>= 0`), or a negative errno.
/// `0` means end of file — not an error, and the only way a client learns where
/// the file ends, since no size is cached anywhere along the path.
///
/// **A request longer than [`FS_MAX_IO`] is clamped — a short read, not
/// `EINVAL`.** This is the deliberate departure from [`BDEV_READ`] and back
/// towards [`CDEV_WRITE`], and both halves of the reasoning matter. BDEV refuses
/// because its client is a *filesystem*, which cannot interpret half a block, so
/// clamping there would push a pointless retry loop into every FS caller. Here
/// the client is VFS, whose entire job is hiding staging details from POSIX — and
/// a short `read()` is what `read()` means. A file read is also short at EOF
/// regardless, so a client that could not cope with a short read could not use
/// this request at all.
pub const FS_READ: i32 = FS_RQ_BASE + 2;

/// VFS → FS server: write bytes into a file.
///
/// **Payload is [`FS_READ`]'s, field for field** — inode, grant id, byte count,
/// position — because it is the same question in the other direction, and one
/// wire codec and one clamp serve both. The reply `m_type` is the byte count
/// written (`>= 0`), or a negative errno.
///
/// **A short write is normal here, not an error.** The FS server clamps every
/// request to the end of the block containing `pos`, so one call moves at most
/// [`FS_MAX_IO`] and usually less; VFS loops. That is [`CDEV_WRITE`]'s stance and
/// deliberately *not* [`BDEV_READ`]'s refuse-or-nothing — BDEV refuses because
/// its client is a filesystem that cannot interpret a fraction of a block, while
/// this request's client is VFS, whose whole job is hiding staging from POSIX.
///
/// The grant must carry `CPF_READ` (where [`FS_READ`]'s carries `CPF_WRITE`). The
/// kernel checks that in `verify_grant`; no server re-implements it. There is no
/// granter field and no grant-offset field: the granter is the kernel-stamped
/// `m_source`, and VFS issues a fresh grant over exactly the round's bytes.
pub const FS_WRITE: i32 = FS_RQ_BASE + 3;

/// VFS → FS server: create a regular file.
///
/// **Payload is [`FS_LOOKUP`]'s, field for field** — the path inline at
/// [`FS_PATH_OFF`], NUL-padded to [`FS_PATH_MAX`] — and **so is the reply**:
/// [`FS_INO_OFF`] / [`FS_MODE_OFF`] / [`FS_SIZE_OFF`], with `m_type = OK`. One
/// wire codec serves both, and VFS classifies either answer through the same
/// function. That is [`FS_WRITE`]-reuses-[`FS_READ`] applied to the control
/// plane.
///
/// The FS server resolves the parent itself, because the band's rule since slice
/// 5.8 is that the control plane travels inline and a create is a path operation.
/// Splitting the path in VFS and sending `{parent_ino, name}` would put path
/// syntax in two servers and cost an extra `FS_LOOKUP`.
///
/// **There is no mode field.** There is no uid, no gid and no permission logic
/// anywhere in the tree, so a mode would be a value nothing reads — and a field
/// with one legal value is worse than no field. The server creates
/// `I_REGULAR | 0o644` with `nlinks = 1`. `open(2)`'s `mode_t` argument is
/// dropped by VFS until a permission model exists.
///
/// **An existing name is `EEXIST`**, not the existing inode: VFS only sends this
/// after a lookup answered `ENOENT`, so the strict answer costs nothing and is
/// what `O_EXCL` will need. Returning the existing inode would make "created" and
/// "found" indistinguishable on the wire and hide a duplicate-entry bug behind a
/// success. `ENOENT` when the parent is missing, `ENOTDIR` when it is a file,
/// `ENOSPC` when there is no free inode or the directory cannot grow.
pub const FS_CREATE: i32 = FS_RQ_BASE + 4;

/// VFS → FS server: discard a regular file's contents.
///
/// Payload: the inode number at [`FS_INO_OFF`]`..+4` (i32). Reply `m_type` is
/// `OK`, with no payload.
///
/// **It truncates to zero and has no length field.** `O_TRUNC` is the only
/// client, and there is no `ftruncate()` anywhere in the tree — no VFS request,
/// no musl wrapper — so a length field would ship five unreachable behaviours
/// (shrink-to-N, extend, no-op, past-EOF, negative) to serve one reachable one.
/// It is a request of its own rather than a flag on [`FS_CREATE`] because VFS
/// must be able to truncate a file that already exists, which is precisely what
/// `O_TRUNC` means.
///
/// `EISDIR` for a directory and `EINVAL` for any other non-regular inode — the
/// same guards, with the same wording, `FS_WRITE` applies.
pub const FS_TRUNC: i32 = FS_RQ_BASE + 5;

/// Number of file-system requests defined so far. Locks an FS server's dispatch
/// coverage the way `NR_DS_REQUESTS` locks the DS server.
pub const NR_FS_MSGS: usize = 6;

/// Offset of the block-device minor in an `FS_READSUPER` payload (i32).
pub const FS_SUPER_MINOR_OFF: usize = 0;
/// Offset of the root inode number in an `FS_READSUPER` **reply** (i32).
pub const FS_SUPER_ROOT_OFF: usize = 0;
/// Offset of the filesystem block size in an `FS_READSUPER` **reply** (i32).
pub const FS_SUPER_BLOCK_SIZE_OFF: usize = 4;
/// Offset of the filesystem size in blocks in an `FS_READSUPER` **reply** (i32).
pub const FS_SUPER_BLOCKS_OFF: usize = 8;

/// Offset of the path in an `FS_LOOKUP` payload ([`FS_PATH_MAX`] NUL-padded bytes).
pub const FS_PATH_OFF: usize = 0;

/// Offset of the inode number — in an `FS_LOOKUP` **reply** and in an `FS_READ`
/// **request** (i32). One constant because it is one field: the number `FS_LOOKUP`
/// hands out is the number `FS_READ` takes back.
pub const FS_INO_OFF: usize = 0;
/// Offset of the inode's mode in an `FS_LOOKUP` reply (i32, widened from `u16`).
pub const FS_MODE_OFF: usize = 4;
/// Offset of the inode's size in an `FS_LOOKUP` reply (i32).
pub const FS_SIZE_OFF: usize = 8;

/// Offset of the grant id in an `FS_READ` payload (i32).
pub const FS_GRANT_OFF: usize = 4;
/// Offset of the requested byte count in an `FS_READ` payload (i32).
pub const FS_LEN_OFF: usize = 8;
/// Offset of the file position in an `FS_READ` payload (u64, so 8-aligned
/// relative to the message base — the payload itself starts at message offset 8,
/// hence 16 rather than 12; the same reasoning [`CDEV_OFFSET_OFF`] documents).
pub const FS_POS_OFF: usize = 16;

/// Width of the inline path field in an [`FS_LOOKUP`] payload.
///
/// The path is NUL-**padded** into it, so the longest path that can travel is
/// `FS_PATH_MAX - 1` bytes: a field with no NUL anywhere is `ENAMETOOLONG` rather
/// than a silently truncated path that could resolve to a different file. VFS
/// applies the same cap before building the request, so a client hears the errno
/// without a round trip.
pub const FS_PATH_MAX: usize = 64;

/// Largest byte count an FS server moves in one [`FS_READ`]. One block, the
/// staging buffer a server on a one-page stack can afford. A longer request is
/// **clamped** (a short read), not refused.
pub const FS_MAX_IO: usize = BDEV_BLOCK_SIZE;

// The FS range sits strictly above the VFS range and strictly below BDEV's (and
// therefore every other server request range) and the NOTIFY marker.
const _: () = assert!(FS_RQ_BASE > VFS_RQ_BASE + (NR_VFS_MSGS as i32 - 1));
const _: () = assert!(FS_RQ_BASE + (NR_FS_MSGS as i32 - 1) < BDEV_RQ_BASE);
const _: () = assert!(FS_RQ_BASE + (NR_FS_MSGS as i32 - 1) < crate::ipc_const::NOTIFY_MESSAGE);

// The `FS_READSUPER` request and reply fields fit the 96-byte payload. Request
// and reply are separate messages, so `FS_SUPER_MINOR_OFF` and
// `FS_SUPER_ROOT_OFF` sharing offset 0 is not an overlap.
const _: () = assert!(FS_SUPER_MINOR_OFF + 4 <= 96);
const _: () = assert!(FS_SUPER_ROOT_OFF + 4 <= FS_SUPER_BLOCK_SIZE_OFF);
const _: () = assert!(FS_SUPER_BLOCK_SIZE_OFF + 4 <= FS_SUPER_BLOCKS_OFF);
const _: () = assert!(FS_SUPER_BLOCKS_OFF + 4 <= 96);

// The `FS_LOOKUP` path fills its own field and fits the payload; its reply's
// three i32s are ordered and disjoint.
const _: () = assert!(FS_PATH_OFF + FS_PATH_MAX <= 96);
const _: () = assert!(FS_INO_OFF + 4 <= FS_MODE_OFF);
const _: () = assert!(FS_MODE_OFF + 4 <= FS_SIZE_OFF);
const _: () = assert!(FS_SIZE_OFF + 4 <= 96);

// The `FS_READ` payload fields are ordered, non-overlapping, and fit the 96-byte
// payload. `FS_POS_OFF` is 8 wide (u64); the rest are 4 (i32).
const _: () = assert!(FS_INO_OFF + 4 <= FS_GRANT_OFF);
const _: () = assert!(FS_GRANT_OFF + 4 <= FS_LEN_OFF);
const _: () = assert!(FS_LEN_OFF + 4 <= FS_POS_OFF);
const _: () = assert!(FS_POS_OFF + 8 <= 96);
// The u64 file position must be 8-aligned within the message, whose payload
// starts at byte 8.
const _: () = assert!((8 + FS_POS_OFF).is_multiple_of(8));

// A full-size transfer must round-trip through the i32 reply `m_type`, or the
// count would land in the negative, errno-shaped band and read as a failure.
const _: () = assert!(FS_MAX_IO <= i32::MAX as usize);
const _: () = assert!(FS_PATH_MAX > 0);

// ---------------------------------------------------------------------------
// BDEV (block device) request numbers — `m_type` values for messages addressed
// to a block-device driver, the `memory` ramdisk being the first (slice 5.7).
//
// Like the PM/VFS/FS/CDEV/VM/DS/SEF/SCHED ranges these are *server IPC
// requests*, not kernel calls. `0xA00` keeps the bands in numeric order between
// the FS band (`0x900`, claimed in slice 5.8) and CDEV (`0xB00`). Numbering is
// minix.rs-specific — MINIX 3 carries `BDEV_*` in
// `include/minix/com.h` with its own values, which minix.rs does not inherit
// because its device protocol is narrower (no `BDEV_REPLY` message class and no
// request id; a driver replies to the SENDREC).
//
// The payload carries a **grant id**, not a buffer address — a driver's client
// lives in a different address space, so the bytes move via `SYS_SAFECOPY`. As in
// the CDEV band the **granter is deliberately not a payload field**: the driver
// takes it from the kernel-stamped `m_source`, because a caller-supplied granter
// would turn any grant-holding driver into a confused deputy. There is no
// grant-*offset* field either — every client through slice 5.9 grants a buffer
// whose block starts at offset 0, and a field nothing sets is a field nothing
// validates.
// ---------------------------------------------------------------------------

/// Base for block-device driver request `m_type` values.
pub const BDEV_RQ_BASE: i32 = 0xA00;

/// Client → block driver: read one block into the client's granted buffer.
///
/// Payload: minor number in [`BDEV_MINOR_OFF`]`..+4` (i32), grant id in
/// [`BDEV_GRANT_OFF`]`..+4` (i32), byte count in [`BDEV_LEN_OFF`]`..+4` (i32), and
/// the block number in [`BDEV_BLOCK_OFF`]`..+8` (u64). The grant must carry
/// `CPF_WRITE` and name the driver as its grantee; the driver pushes the bytes
/// with `SYS_SAFECOPY(SAFECOPY_TO, m_source, …)`.
///
/// Reply `m_type` is the **number of bytes read** (`>= 0`; `0` is legal), or a
/// negative errno.
///
/// **A request longer than [`BDEV_MAX_IO`] is `EINVAL`, not a short read** — a
/// deliberate departure from [`CDEV_WRITE`]'s clamp. A short *write* is a POSIX
/// contract every client already loops over; a short *block read* is useless,
/// because a filesystem cannot interpret half a block, so clamping here would
/// push a retry loop into every FS caller for no gain. An out-of-range `block` is
/// `EINVAL` for the same reason: a block device's size is known to its client
/// (MFS reads it from the superblock's `s_zones`), so asking past the end is a
/// caller bug rather than a media condition. `EIO` stays reserved for Phase 6's
/// real media errors, where the request was well-formed and the *device* failed.
pub const BDEV_READ: i32 = BDEV_RQ_BASE;

/// Client → block driver: write one block. Payload is [`BDEV_READ`]'s, with the
/// grant carrying `CPF_READ` instead.
///
/// **A real store as of slice 5.10a.** From slice 5.7 until then it was defined
/// and answered `EROFS` — deliberately not folded into the unknown-`m_type`
/// `ENOSYS` arm, because that would have made "this driver has never heard of
/// writes" and "this driver knows about writes and refuses them" indistinguishable
/// to a client. Defining the request that early is precisely what made turning it
/// real a one-arm change inside the driver rather than a new call number past the
/// slice-5.6 ABI freeze: the geometry validation, the dispatch arm and the denial
/// probes were already there, and 5.10a replaced the refusal with a
/// `SAFECOPY_FROM`.
///
/// A driver may of course still answer `EROFS` — a read-only medium is a real
/// thing — but the `memory` ramdisk no longer does, and no client may assume it.
pub const BDEV_WRITE: i32 = BDEV_RQ_BASE + 1;

/// Number of block-device requests defined so far. Locks a driver's dispatch
/// coverage the way `NR_DS_REQUESTS` locks the DS server.
pub const NR_BDEV_MSGS: usize = 2;

/// Offset of the device minor number in a `BDEV_READ` / `BDEV_WRITE` payload (i32).
pub const BDEV_MINOR_OFF: usize = 0;
/// Offset of the grant id in a `BDEV_READ` / `BDEV_WRITE` payload (i32).
pub const BDEV_GRANT_OFF: usize = 4;
/// Offset of the requested byte count in a `BDEV_READ` / `BDEV_WRITE` payload (i32).
pub const BDEV_LEN_OFF: usize = 8;
/// Offset of the block number in a `BDEV_READ` / `BDEV_WRITE` payload (u64, so
/// 8-aligned relative to the message base — the payload itself starts at message
/// offset 8, hence 16 rather than 12; the same reasoning [`CDEV_OFFSET_OFF`]
/// documents).
pub const BDEV_BLOCK_OFF: usize = 16;

/// Block size of every minix.rs block device, and MinixFS v3's. Equal to
/// `USER_PAGE_SIZE` so a driver can serve a block straight out of one mapped
/// frame, which is what makes the ramdisk's copy loop a page loop.
pub const BDEV_BLOCK_SIZE: usize = 4096;

/// Largest byte count a block driver moves in one request. Equal to
/// [`BDEV_BLOCK_SIZE`]: one request is one block. A longer request is `EINVAL`
/// (see [`BDEV_READ`]), *not* clamped the way [`CDEV_MAX_IO`] is.
pub const BDEV_MAX_IO: usize = BDEV_BLOCK_SIZE;

/// The ramdisk minor: the `memory` driver's boot-image-backed root filesystem.
/// Any other minor is `ENXIO`.
pub const BDEV_MINOR_RAMDISK: i32 = 0;

// The BDEV range sits strictly above the FS range and strictly below CDEV's (and
// therefore every other server request range) and the NOTIFY marker.
const _: () = assert!(BDEV_RQ_BASE > FS_RQ_BASE + (NR_FS_MSGS as i32 - 1));
const _: () = assert!(BDEV_RQ_BASE + (NR_BDEV_MSGS as i32 - 1) < CDEV_RQ_BASE);
const _: () = assert!(BDEV_RQ_BASE + (NR_BDEV_MSGS as i32 - 1) < crate::ipc_const::NOTIFY_MESSAGE);

// The payload fields are ordered, non-overlapping, and fit the 96-byte payload.
// `BDEV_BLOCK_OFF` is 8 wide (u64); the rest are 4 (i32).
const _: () = assert!(BDEV_MINOR_OFF + 4 <= BDEV_GRANT_OFF);
const _: () = assert!(BDEV_GRANT_OFF + 4 <= BDEV_LEN_OFF);
const _: () = assert!(BDEV_LEN_OFF + 4 <= BDEV_BLOCK_OFF);
const _: () = assert!(BDEV_BLOCK_OFF + 8 <= 96);
// The u64 block number must be 8-aligned within the message, whose payload starts
// at byte 8.
const _: () = assert!((8 + BDEV_BLOCK_OFF).is_multiple_of(8));

// One block is one page: the ramdisk driver serves a block by safecopying out of a
// single mapped frame, and MFS's block size is const-asserted equal to this.
const _: () = assert!(BDEV_BLOCK_SIZE == crate::message::USER_PAGE_SIZE as usize);
// A full-size transfer must round-trip through the i32 reply `m_type`, or the
// count would land in the negative, errno-shaped band and read as a failure.
const _: () = assert!(BDEV_MAX_IO <= i32::MAX as usize);

// ---------------------------------------------------------------------------
// CDEV (character device) request numbers — `m_type` values for messages
// addressed to a character-device driver, TTY being the first (slice 5.3).
//
// Like the PM/VFS/FS/BDEV/VM/DS/SEF/SCHED ranges these are *server IPC
// requests*, not kernel calls. `0xB00` keeps the bands in numeric order between
// BDEV (`0xA00`, claimed in slice 5.7) and VM (`0xC00`). With the FS band taking
// `0x900` in slice 5.8, `0x700..0xC00` is now fully allocated — PM, VFS, FS,
// BDEV, CDEV. Numbering is minix.rs-specific — MINIX 3
// carries `CDEV_*` in `include/minix/com.h` with its own values, which minix.rs
// does not inherit because its device protocol is narrower (no `CDEV_REPLY`
// message class; a driver replies to the SENDREC).
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

// The CDEV range sits strictly above the BDEV range and strictly below VM's (and
// therefore every other server request range) and the NOTIFY marker.
const _: () = assert!(CDEV_RQ_BASE > BDEV_RQ_BASE + (NR_BDEV_MSGS as i32 - 1));
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
    fn sys_exec_payload_offsets_are_ordered_and_disjoint() {
        // Every field the request defines, in declaration order, with its width.
        // The `fields.len()` assertion is the point: slice 5.9 added three, and a
        // fourth must be a *visible* edit rather than a quiet one — this payload
        // is past the slice-5.6 ABI freeze and there is C in another repository
        // reading the same numbers.
        let fields = [
            ("target", 0usize, 4usize),
            ("name", 4, EXEC_NAME_LEN),
            ("src", EXEC_SRC_OFF, 4),
            ("granter", EXEC_GRANTER_OFF, 4),
            ("grant", EXEC_GRANT_OFF, 4),
            ("len", EXEC_LEN_OFF, 8),
        ];
        assert_eq!(fields.len(), 6, "a SYS_EXEC payload field was added");
        assert_ordered_and_disjoint(&fields);

        // The u64 length must be 8-aligned *within the message*, whose payload
        // starts at byte 8.
        assert_eq!((8 + EXEC_LEN_OFF) % 8, 0);
    }

    #[test]
    fn the_exec_source_selector_reserves_zero() {
        // A zeroed payload must not name a form. Same convention as
        // `SAFECOPY_FROM`/`SAFECOPY_TO`, `VMCTL_*` and `PRIVCTL_*`, and the reason
        // `do_exec` validates the selector before it looks at anything else.
        assert_eq!(EXEC_SRC_NAME, 1);
        assert_eq!(EXEC_SRC_GRANT, 2);
        assert_ne!(EXEC_SRC_NAME, EXEC_SRC_GRANT);
        for sel in [EXEC_SRC_NAME, EXEC_SRC_GRANT] {
            assert_ne!(sel, 0);
        }
    }

    #[test]
    fn get_whoami_is_frozen_at_twelve() {
        // NOT a MINIX 3 value, despite what this test used to claim (and be
        // named): modern MINIX 3 numbers `GET_WHOAMI` 19, in `include/minix/com.h`.
        // 12 is minix.rs's own, and it is frozen past slice 5.6 — `server-rt`'s
        // `sef_startup`, every server's `sef ready` marker, and the musl fork's
        // generated `minixrs/callnr.h` all depend on it.
        assert_eq!(GET_WHOAMI, 12);
    }

    #[test]
    fn getinfo_selectors_are_distinct() {
        // The two selectors share one kernel call, so a collision would route
        // `GET_RAMDISK` into `fill_whoami` and hand the caller a name where it
        // expected a VA. `GET_RAMDISK` also stays clear of the `0..=31` block
        // reserved for selectors that mirror a MINIX 3 number.
        assert_ne!(GET_WHOAMI, GET_RAMDISK);
        assert_eq!(GET_RAMDISK, 64);
        // Phrased as a diagnosis, not a claim: this message prints only when the
        // assert *fails*, which happens exactly when `GET_RAMDISK < 32`. Writing
        // it as "GET_RAMDISK is inside 0..=31" is true at failure time but reads
        // like the property being asserted, which is the opposite.
        assert_eq!(
            GET_RAMDISK.min(32),
            32,
            "GET_RAMDISK must stay clear of the 0..=31 MINIX-mirrored block"
        );
        assert_eq!(GET_WHOAMI.min(31), GET_WHOAMI);

        // The reply's two u64 fields are ordered, disjoint, in the payload, and
        // 8-aligned within the message (whose payload starts at byte 8).
        assert_eq!(GETINFO_RAMDISK_VA_OFF, 0);
        assert_eq!(GETINFO_RAMDISK_VA_OFF + 8, GETINFO_RAMDISK_LEN_OFF);
        // `min` rather than `<=`: an all-constant `assert!` trips clippy's
        // `assertions_on_constants`.
        let end = GETINFO_RAMDISK_LEN_OFF + 8;
        assert_eq!(end.min(96), end);
        assert_eq!((8 + GETINFO_RAMDISK_VA_OFF) % 8, 0);
        assert_eq!((8 + GETINFO_RAMDISK_LEN_OFF) % 8, 0);
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
            assert_ne!(r, VFS_OPEN);
            assert_ne!(r, VFS_READ);
            assert_ne!(r, VFS_CLOSE);
            assert_ne!(r, FS_READSUPER);
            assert_ne!(r, FS_LOOKUP);
            assert_ne!(r, FS_READ);
            assert_ne!(r, FS_WRITE);
            assert_ne!(r, BDEV_READ);
            assert_ne!(r, BDEV_WRITE);
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
                VFS_OPEN,
                VFS_READ,
                VFS_CLOSE,
                VFS_EXEC_STAGE,
                FS_READSUPER,
                FS_LOOKUP,
                FS_READ,
                FS_WRITE,
                BDEV_READ,
                BDEV_WRITE,
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
                VFS_OPEN,
                VFS_READ,
                VFS_CLOSE,
                VFS_EXEC_STAGE,
                FS_READSUPER,
                FS_LOOKUP,
                FS_READ,
                FS_WRITE,
                BDEV_READ,
                BDEV_WRITE,
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
        let msgs = [VFS_WRITE, VFS_OPEN, VFS_READ, VFS_CLOSE, VFS_EXEC_STAGE];
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
        for m in [VFS_WRITE, VFS_OPEN, VFS_READ, VFS_CLOSE, VFS_EXEC_STAGE] {
            for other in [
                PM_GETPID,
                PM_FORK,
                PM_EXIT,
                PM_WAIT,
                PM_EXEC,
                PM_GRANT_TEST,
                FS_READSUPER,
                FS_LOOKUP,
                FS_READ,
                FS_WRITE,
                BDEV_READ,
                BDEV_WRITE,
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
            assert!(m < FS_RQ_BASE);
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
    fn vfs_open_payload_offsets_are_ordered_and_disjoint() {
        // Like `VFS_WRITE`, this payload carries a **raw buffer address** rather
        // than a grant id — VFS's client is an ordinary user process with no
        // grant table. Unlike `VFS_WRITE`, VFS reads these bytes itself, with
        // `SYS_COPY` out of the kernel-stamped `m_source`. There is deliberately
        // no source-process field here for the sharpest form of the 5.2 rule:
        // `SYS_COPY` has *no* per-target authorization at all, so a
        // payload-supplied source would let any client read any process's memory
        // through VFS.
        //
        // Slice 5.10b added `flags` in [`VFS_FLAGS_OFF`]`..+4` (i32,
        // `crate::fcntl`'s bits) for `O_CREAT` / `O_TRUNC`. Through 5.10a every
        // descriptor was readable and writable alike; the length assertion
        // below is what makes adding a fourth field a visible change rather
        // than a quiet one.
        let fields = [
            ("path", VFS_PATH_OFF, 8),
            ("path_len", VFS_PATH_LEN_OFF, 4),
            ("flags", VFS_FLAGS_OFF, 4),
        ];
        assert_eq!(fields.len(), 3, "a VFS_OPEN payload field was added");
        assert_eq!(VFS_PATH_OFF, 0, "the first field must start the payload");
        assert_ordered_and_disjoint(&fields);

        // The u64 field must be 8-aligned *within the message*, whose payload
        // starts at byte 8 — so an even multiple of 8 here.
        assert_eq!((8 + VFS_PATH_OFF) % 8, 0);
    }

    #[test]
    fn vfs_exec_stage_payload_offsets_are_ordered_and_disjoint() {
        // One field each way, and that is the record: the path travels **inline**
        // rather than as a pointer, because the client is PM — which already
        // holds it inline in the `PM_EXEC` it is serving — so passing it by value
        // costs no `SYS_COPY` and there is no source process for a caller to
        // misname. There is deliberately no grantee field in the reply either:
        // VFS grants the staged bytes to the kernel-stamped `m_source`.
        let request = [("path", VFS_EXEC_PATH_OFF, FS_PATH_MAX)];
        let reply = [("grant", VFS_EXEC_GRANT_OFF, 4usize)];
        assert_eq!(request.len(), 1, "a VFS_EXEC_STAGE request field was added");
        assert_eq!(reply.len(), 1, "a VFS_EXEC_STAGE reply field was added");
        assert_ordered_and_disjoint(&request);
        assert_ordered_and_disjoint(&reply);
        assert_eq!(VFS_EXEC_PATH_OFF, 0);
        assert_eq!(VFS_EXEC_GRANT_OFF, 0, "the FS band's reuse-byte-0 shape");
    }

    #[test]
    fn the_exec_staging_cap_clears_the_musl_hello() {
        // Sized against the flavour CI actually builds. The musl `hello` is
        // ~200 KB and the SDK one ~46 KB, and no CI job installs an SDK — so a
        // cap tuned to the smaller number would pass locally and fail
        // `qemu-smoke`. `kernel/build.rs` asserts the built bytes fit; this is
        // the standing headroom claim beside it.
        assert_eq!(VFS_EXEC_MAX, 256 * 1024);
        assert_eq!(VFS_EXEC_MAX.max(200_152), VFS_EXEC_MAX);

        // The count comes back as the reply `m_type`, so it must stay positive
        // in an i32; and the kernel must be willing to map what VFS will stage,
        // or a file inside this cap could still be refused by `do_exec`.
        assert_eq!(VFS_EXEC_MAX.min(i32::MAX as usize), VFS_EXEC_MAX);
        assert_eq!(
            VFS_EXEC_MAX.min(crate::execimage::MAX_IMAGE_BYTES),
            VFS_EXEC_MAX
        );
    }

    #[test]
    fn the_pm_exec_field_carries_a_whole_path() {
        // Slice 5.9 widened it from a 16-byte module name. It matches the FS
        // band's width so a path PM accepts survives every hop down to
        // `FS_LOOKUP` unrefrained, and `EXEC_NAME_LEN` still fits inside it
        // because `argv[0]` is the path's *basename*, not the path.
        assert_eq!(PM_EXEC_PATH_OFF, 0);
        assert_eq!(PM_EXEC_PATH_MAX, FS_PATH_MAX);
        assert_eq!(EXEC_NAME_LEN.min(PM_EXEC_PATH_MAX), EXEC_NAME_LEN);
        let end = PM_EXEC_PATH_OFF + PM_EXEC_PATH_MAX;
        assert_eq!(end.min(96), end);
    }

    #[test]
    fn vfs_read_reuses_the_write_payload_verbatim() {
        // Not a tautology: it is the record of a decision. `read` and `write`
        // differ only in which way the bytes travel, so they share one set of
        // offsets, one parser, and one validator — and this test is what turns
        // "give read its own offsets" into a failure rather than a quiet
        // divergence that only shows up as a mis-parsed request at run time.
        assert_eq!(VFS_READ, VFS_RQ_BASE + 2);
        for (name, off) in [
            ("fd", VFS_FD_OFF),
            ("len", VFS_LEN_OFF),
            ("buf", VFS_BUF_OFF),
        ] {
            assert!(off + 4 <= 96, "{name} runs past the payload");
        }
        // `VFS_CLOSE` takes only the descriptor, at the same offset again.
        assert_eq!(VFS_FD_OFF, 0);
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
                VFS_OPEN,
                VFS_READ,
                VFS_CLOSE,
                VFS_EXEC_STAGE,
                FS_READSUPER,
                FS_LOOKUP,
                FS_READ,
                FS_WRITE,
                BDEV_READ,
                BDEV_WRITE,
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
            assert!(m > BDEV_RQ_BASE + (NR_BDEV_MSGS as i32 - 1));
            assert!(m > KERNEL_CALL + NR_KERN_CALLS as i32 - 1);
            assert!(m < VM_RQ_BASE);
            assert_ne!(m, crate::ipc_const::NOTIFY_MESSAGE);
            assert!(m < crate::ipc_const::NOTIFY_MESSAGE);
        }
    }

    #[test]
    fn the_server_band_space_below_vm_is_fully_allocated() {
        // The successor to `cdev_band_sits_at_0xb00_leaving_one_band_free`, whose
        // premise — that `0x900` was still free — slice 5.8 made false by putting
        // the FS band there.
        //
        // Every `0x100`-aligned slot from PM's base up to VM's now belongs to a
        // named band, in ascending numeric order. That is the record: a new band
        // has no reserved slot to take, so it must either find a home outside this
        // span or move one of these — and either way this test is what says so out
        // loud rather than leaving it to be rediscovered.
        let bands = [
            ("PM", PM_RQ_BASE),
            ("VFS", VFS_RQ_BASE),
            ("FS", FS_RQ_BASE),
            ("BDEV", BDEV_RQ_BASE),
            ("CDEV", CDEV_RQ_BASE),
        ];
        assert_eq!(PM_RQ_BASE, 0x700);
        assert_eq!(CDEV_RQ_BASE, 0xB00);
        assert_eq!(VM_RQ_BASE, 0xC00);

        // Contiguous, ascending, and exactly tiling `0x700..0xC00`.
        for (i, (name, base)) in bands.iter().enumerate() {
            assert_eq!(
                *base,
                PM_RQ_BASE + (i as i32) * 0x100,
                "the {name} band is not the {i}th slot from PM's base"
            );
        }
        assert_eq!(
            bands.last().unwrap().1 + 0x100,
            VM_RQ_BASE,
            "a gap or an overlap opened between the CDEV band and VM's"
        );
    }

    #[test]
    fn fs_msgs_contiguous_from_base() {
        // FS requests are contiguous from FS_RQ_BASE; NR_FS_MSGS locks an FS
        // server's dispatch coverage.
        let msgs = [
            FS_READSUPER,
            FS_LOOKUP,
            FS_READ,
            FS_WRITE,
            FS_CREATE,
            FS_TRUNC,
        ];
        for (i, m) in msgs.iter().enumerate() {
            assert_eq!(*m, FS_RQ_BASE + i as i32);
        }
        assert_eq!(msgs.len(), NR_FS_MSGS);
        assert_eq!(FS_RQ_BASE, 0x900);
    }

    #[test]
    fn fs_msgs_distinct_from_other_ranges() {
        // Each FS request must stay distinct from every other band and the
        // KERNEL_CALL range, and below NOTIFY_MESSAGE — so a server's m_type
        // dispatcher and the SEF classifier never collide. FS sits between VFS
        // and BDEV, which is what its two bounds assert.
        for m in [
            FS_READSUPER,
            FS_LOOKUP,
            FS_READ,
            FS_WRITE,
            FS_CREATE,
            FS_TRUNC,
        ] {
            for other in [
                PM_GETPID,
                PM_FORK,
                PM_EXIT,
                PM_WAIT,
                PM_EXEC,
                PM_GRANT_TEST,
                VFS_WRITE,
                VFS_OPEN,
                VFS_READ,
                VFS_CLOSE,
                VFS_EXEC_STAGE,
                BDEV_READ,
                BDEV_WRITE,
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
            assert!(m > VFS_RQ_BASE + (NR_VFS_MSGS as i32 - 1));
            assert!(m > KERNEL_CALL + NR_KERN_CALLS as i32 - 1);
            assert!(m < BDEV_RQ_BASE);
            assert_ne!(m, crate::ipc_const::NOTIFY_MESSAGE);
            assert!(m < crate::ipc_const::NOTIFY_MESSAGE);
        }
    }

    #[test]
    fn fs_readsuper_payload_offsets_are_ordered_and_disjoint() {
        // The request is one field; the *reply* is three, and they are what make
        // the request worth more than its `m_type`. Request and reply are separate
        // messages, which is why `minor` and `root` may share offset 0.
        let request = [("minor", FS_SUPER_MINOR_OFF, 4)];
        assert_eq!(request.len(), 1, "an FS_READSUPER payload field was added");
        assert_eq!(
            FS_SUPER_MINOR_OFF, 0,
            "the first field must start the payload"
        );

        let reply = [
            ("root", FS_SUPER_ROOT_OFF, 4),
            ("block_size", FS_SUPER_BLOCK_SIZE_OFF, 4),
            ("blocks", FS_SUPER_BLOCKS_OFF, 4),
        ];
        assert_eq!(reply.len(), 3, "an FS_READSUPER reply field was added");
        assert_ordered_and_disjoint(&reply);
    }

    #[test]
    fn fs_lookup_payload_offsets_are_ordered_and_disjoint() {
        // The path travels INLINE, NUL-padded to FS_PATH_MAX — not by grant.
        // It is control plane rather than the data path grants were provisioned
        // for (the `PM_EXEC` name and the `DS_PUBLISH` key make the same call), it
        // costs the FS server no staging buffer, and there being no granter is
        // what deletes the confused-deputy question outright. This list is the
        // record of that: turning the path into a grant means editing it, and the
        // length assertion is what makes such a change visible.
        let request = [("path", FS_PATH_OFF, FS_PATH_MAX)];
        assert_eq!(request.len(), 1, "an FS_LOOKUP payload field was added");
        assert_eq!(FS_PATH_OFF, 0, "the first field must start the payload");
        assert_ordered_and_disjoint(&request);

        let reply = [
            ("ino", FS_INO_OFF, 4),
            ("mode", FS_MODE_OFF, 4),
            ("size", FS_SIZE_OFF, 4),
        ];
        assert_eq!(reply.len(), 3, "an FS_LOOKUP reply field was added");
        assert_ordered_and_disjoint(&reply);
    }

    #[test]
    fn fs_read_payload_offsets_are_ordered_and_disjoint() {
        // Every field the request defines, in declaration order, with its width:
        // i32 fields are 4 bytes, the u64 file position is 8.
        //
        // There is deliberately NO granter field — the server takes the granter
        // from the kernel-stamped `m_source`, because a payload granter would let
        // a client aim the server's privileged `SYS_SAFECOPY` at a third party's
        // address space (a confused deputy). And no grant-*offset* field either:
        // VFS issues a fresh grant over the remaining tail each round, which is
        // what makes the offset unnecessary rather than merely unused.
        let fields = [
            ("ino", FS_INO_OFF, 4),
            ("grant", FS_GRANT_OFF, 4),
            ("len", FS_LEN_OFF, 4),
            ("pos", FS_POS_OFF, 8),
        ];
        assert_eq!(fields.len(), 4, "an FS_READ payload field was added");
        assert_eq!(FS_INO_OFF, 0, "the first field must start the payload");
        assert_ordered_and_disjoint(&fields);

        // The u64 field must be 8-aligned *within the message*, whose payload
        // starts at byte 8 — so an even multiple of 8 here.
        assert_eq!((8 + FS_POS_OFF) % 8, 0);
    }

    #[test]
    fn fs_write_reuses_the_read_payload_offsets() {
        // W1: the same four fields at the same offsets. Not a coincidence to be
        // re-derived — the number `FS_LOOKUP` hands out is the number both
        // `FS_READ` and `FS_WRITE` take back, and one clamp/parse serves both.
        assert_eq!(FS_WRITE, FS_RQ_BASE + 3);
        assert_eq!(FS_INO_OFF, 0);
        assert_eq!(FS_GRANT_OFF, 4);
        assert_eq!(FS_LEN_OFF, 8);
        assert_eq!(FS_POS_OFF, 16);
        // The four fields are ordered, non-overlapping, and fit the 96-byte payload.
        let fields = [
            (FS_INO_OFF, 4),
            (FS_GRANT_OFF, 4),
            (FS_LEN_OFF, 4),
            (FS_POS_OFF, 8),
        ];
        assert_eq!(fields.len(), 4, "an FS_WRITE payload field was added");
        let mut end = 0usize;
        for (off, width) in fields {
            assert!(off >= end);
            end = off + width;
        }
        assert!(end <= 96);
    }

    #[test]
    fn the_fs_transfer_and_path_limits_fit_their_wire_fields() {
        // One FS_READ moves at most one block — the staging buffer a server on a
        // one-page stack can afford — and the reply `m_type` carries that count as
        // an i32, so it must round-trip or it would land in the negative,
        // errno-shaped band and read as a failure.
        assert_eq!(FS_MAX_IO, BDEV_BLOCK_SIZE);
        assert_eq!(i32::try_from(FS_MAX_IO), Ok(4096));
        // ...and the path must fit its own inline field with room for the rest of
        // the payload, which is what makes the inline choice viable at all.
        assert_eq!(FS_PATH_MAX, 64);
        assert_eq!(
            (FS_PATH_OFF + FS_PATH_MAX).min(96),
            FS_PATH_OFF + FS_PATH_MAX
        );
    }

    /// Assert a `(name, offset, width)` field list is ordered, non-overlapping,
    /// and inside the 96-byte payload.
    ///
    /// Factored out because slice 5.8 added four more of these lists at once; the
    /// older per-request tests keep their inlined copies rather than churning
    /// them, since the assertion text is what a failure prints.
    fn assert_ordered_and_disjoint(fields: &[(&str, usize, usize)]) {
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
    }

    #[test]
    fn bdev_msgs_contiguous_from_base() {
        // BDEV requests are contiguous from BDEV_RQ_BASE; NR_BDEV_MSGS locks a
        // block driver's dispatch coverage.
        let msgs = [BDEV_READ, BDEV_WRITE];
        for (i, m) in msgs.iter().enumerate() {
            assert_eq!(*m, BDEV_RQ_BASE + i as i32);
        }
        assert_eq!(msgs.len(), NR_BDEV_MSGS);
        assert_eq!(BDEV_RQ_BASE, 0xA00);
    }

    #[test]
    fn bdev_msgs_distinct_from_other_ranges() {
        // Each BDEV request must stay distinct from every other band and the
        // KERNEL_CALL range, and below NOTIFY_MESSAGE — so a driver's m_type
        // dispatcher and the SEF classifier never collide. BDEV sits between VFS
        // and CDEV, which is what its two bounds assert.
        for m in [BDEV_READ, BDEV_WRITE] {
            for other in [
                PM_GETPID,
                PM_FORK,
                PM_EXIT,
                PM_WAIT,
                PM_EXEC,
                PM_GRANT_TEST,
                VFS_WRITE,
                VFS_OPEN,
                VFS_READ,
                VFS_CLOSE,
                VFS_EXEC_STAGE,
                FS_READSUPER,
                FS_LOOKUP,
                FS_READ,
                FS_WRITE,
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
            assert!(m > FS_RQ_BASE + (NR_FS_MSGS as i32 - 1));
            assert!(m > KERNEL_CALL + NR_KERN_CALLS as i32 - 1);
            assert!(m < CDEV_RQ_BASE);
            assert_ne!(m, crate::ipc_const::NOTIFY_MESSAGE);
            assert!(m < crate::ipc_const::NOTIFY_MESSAGE);
        }
    }

    #[test]
    fn bdev_read_payload_offsets_are_ordered_and_disjoint() {
        // Every field the request defines, in declaration order, with its width:
        // i32 fields are 4 bytes, the u64 block number is 8.
        //
        // There is deliberately NO granter field — the driver takes the granter
        // from the kernel-stamped `m_source`, because a payload granter would let
        // a client aim the driver's privileged `SYS_SAFECOPY` at a third party's
        // address space (a confused deputy). And no grant-*offset* field either:
        // every client through slice 5.9 grants a buffer whose block starts at
        // offset 0, so an offset would be a field nothing sets and nothing
        // validates. This list is the record of both — adding a field means
        // editing it, and the length assertion below is what makes that a visible
        // change rather than a quiet one.
        let fields = [
            ("minor", BDEV_MINOR_OFF, 4),
            ("grant", BDEV_GRANT_OFF, 4),
            ("len", BDEV_LEN_OFF, 4),
            ("block", BDEV_BLOCK_OFF, 8),
        ];
        assert_eq!(fields.len(), 4, "a BDEV_READ payload field was added");
        assert_eq!(BDEV_MINOR_OFF, 0, "the first field must start the payload");

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
        assert_eq!((8 + BDEV_BLOCK_OFF) % 8, 0);
    }

    #[test]
    fn bdev_block_size_is_one_page_and_is_the_whole_transfer_unit() {
        // One block is one page: the ramdisk serves a block out of a single
        // mapped frame. `BDEV_MAX_IO == BDEV_BLOCK_SIZE` is what makes an
        // over-long request a *malformed* request (EINVAL) rather than a short
        // read — there is nothing between a block and a clamp to report.
        assert_eq!(BDEV_BLOCK_SIZE, 4096);
        assert_eq!(BDEV_BLOCK_SIZE, crate::message::USER_PAGE_SIZE as usize);
        assert_eq!(BDEV_MAX_IO, BDEV_BLOCK_SIZE);
        // The reply `m_type` carries the byte count as an i32.
        assert_eq!(i32::try_from(BDEV_MAX_IO), Ok(4096));
        assert_eq!(BDEV_MINOR_RAMDISK, 0);
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
            assert_ne!(m, VFS_OPEN);
            assert_ne!(m, VFS_READ);
            assert_ne!(m, VFS_CLOSE);
            assert_ne!(m, FS_READSUPER);
            assert_ne!(m, FS_LOOKUP);
            assert_ne!(m, FS_READ);
            assert_ne!(m, FS_WRITE);
            assert_ne!(m, BDEV_READ);
            assert_ne!(m, BDEV_WRITE);
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
