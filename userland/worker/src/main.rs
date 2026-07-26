// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! minix.rs `worker` — the slice-4.7 exec target, and the slice-5.5 exec-ABI probe.
//!
//! A tiny freestanding EL0 program that is *not* a boot server: it is packed
//! into the boot-image archive only so `SYS_EXEC` can resolve it by name, and it
//! is never loaded at boot (`kernel/build.rs` tags its MXBI record with
//! `com::EXEC_ONLY_PROC_NR`). PM's `handle_exec` selects it; the kernel loads it
//! into a forked child's fresh address space, replacing the child's image.
//!
//! Since EL0 has no console, the worker proves it ran through observable IPC: a
//! few `PM_GETPID` round-trips (visible as `[ipc] caller=<child nr> target=0x0`,
//! returning the child's preserved pid) followed by `PM_EXIT`, which tears it
//! down so the parent's `wait()` reaps it and the fork loop recycles the slot.
//!
//! ## The exec ABI probe (slice 5.5)
//!
//! `SYS_EXEC` now hands a new image a real SysV/Linux initial stack so musl's
//! crt can run unpatched. A kernel `[exec]` trace only says the kernel *thinks*
//! it wrote that frame — so the worker reads its own `sp` and checks the frame
//! byte for byte ([`validate`]). That is what [`_start`] is `naked` for: a
//! prologue would perturb `sp` before Rust could see it, and taking the value in
//! `x0` from the kernel would prove nothing about `SP_EL0`.
//!
//! The verdict travels out as the **exit status**, not as a console line: the
//! worker runs once per init fork cycle, so an unconditional success line would
//! flood the console. `init` reaps the first child and reports that one status.
//! A failure additionally writes to fd 2 directly — belt and braces, since the
//! status path could itself be what broke.
//!
//! Built as a freestanding aarch64 ELF (`userland/worker/user.ld`, which uses the
//! `FILEHDR PHDRS` idiom so the first `PT_LOAD` covers the ELF header and the
//! kernel has an `AT_PHDR` to report). It uses `minix-ipc` directly — no
//! `server-rt`/SEF, because it is a plain user program, not a server. The
//! `_start` shim and panic handler are gated to `not(test)`.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

minixrs_abi_note::brand!();

use minixrs_ipc::ipc_sendrec;
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{
    PM_EXIT, PM_GETPID, VFS_BUF_OFF, VFS_FD_OFF, VFS_LEN_OFF, VFS_WRITE,
};
use minixrs_kernel_shared::com::{PM_PROC_NR, VFS_PROC_NR, boot_endpoint};
use minixrs_kernel_shared::endpoint::Endpoint;
use minixrs_kernel_shared::execstack::{
    AT_NULL, AT_PAGESZ, AT_PHDR, AT_PHENT, AT_PHNUM, EXEC_STACK_PROBE_PASS, STACK_ALIGN,
};
use minixrs_kernel_shared::message::USER_PAGE_SIZE;

/// Number of observable `PM_GETPID` round-trips before exiting — enough to make
/// the worker's activity unmistakable in the boot trace without flooding it.
const GETPID_ROUNDS: usize = 3;

/// Standard error, pre-opened by VFS to the console.
const STDERR: i32 = 2;

/// What `argv[0]` must say: PM's `handle_exec` hardcodes this as its
/// `EXEC_TARGET`, and the kernel copies the `SYS_EXEC` name field into the frame.
const ARGV0: &[u8] = b"worker";

/// Link base of this image (`user.ld`). `AT_PHDR` must point into the first
/// `PT_LOAD`, whose `p_vaddr` is exactly this.
const LOAD_BASE: u64 = 0x0010_0000;

/// `PT_LOAD`, the `p_type` of the segment `AT_PHDR` must describe.
const PT_LOAD: u32 = 1;

/// Bytes per ELF64 program header — what `AT_PHENT` must report.
const PHENT: u64 = 56;

/// Upper bound on the auxv scan. The frame the kernel builds carries four real
/// pairs; a bound well above that keeps a corrupt, unterminated vector from
/// walking off the stack page instead of being reported.
const AUXV_SCAN_MAX: usize = 16;

/// ELF entry point. `SYS_EXEC` primes `SP_EL0` to point at the initial stack
/// frame it built, so `_start` hands `sp` straight to [`main`] and dives into
/// Rust without setting up a stack itself.
///
/// Naked on purpose: an ordinary function's prologue may adjust `sp` before the
/// body runs, and reading a perturbed value would prove nothing about what the
/// kernel handed over. `mov x0, sp` is the first instruction the process
/// executes.
#[cfg(all(not(test), target_arch = "aarch64"))]
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "minixrs", unsafe(link_section = ".text._start"))]
#[unsafe(naked)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "mov x0, sp",
        "b {main}",
        main = sym main,
    )
}

/// Non-aarch64 fallback so the crate stays `cargo check --workspace`-able on the
/// host (CI's blocking clippy job runs on x86_64). The worker only ever runs at
/// EL0 on aarch64; calling this off-target is a build-configuration bug. It
/// still calls [`main`] so the real entry path stays type-checked rather than
/// becoming dead code.
#[cfg(all(not(test), not(target_arch = "aarch64")))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    main(0)
}

#[cfg_attr(test, allow(dead_code))]
extern "C" fn main(sp: u64) -> ! {
    let pm = boot_endpoint(PM_PROC_NR);

    // Check the frame first, while nothing has run that could disturb it.
    let verdict = validate(sp);
    if verdict != 0 {
        // The status path is the primary channel; this line is the backup, for
        // the case where the status path is itself what regressed.
        let vfs = boot_endpoint(VFS_PROC_NR);
        vfs_write(vfs, STDERR, b"minix.rs worker: stack FAIL\n");
    }

    // Prove the new image is running: a few getpid round-trips. PM replies with
    // this proc's preserved pid (exec keeps the fork child's identity).
    for _ in 0..GETPID_ROUNDS {
        let mut msg = Message {
            m_source: 0,
            m_type: PM_GETPID,
            payload: [0u8; 96],
        };
        let _ = ipc_sendrec(pm, &mut msg);
    }

    // exit(status): `EXEC_STACK_PROBE_PASS` when the frame checked out, else the
    // number of the first failing check. A *positive* pass value on purpose —
    // dying before this point leaves the zombie's status at 0, which must not be
    // readable as success. PM encodes it as `W_EXITCODE` and hands it to whoever
    // `wait()`s — init, which reports the first one it reaps. PM tears us down
    // via SYS_EXIT rather than replying, so this SENDREC never returns.
    let status = if verdict == 0 {
        EXEC_STACK_PROBE_PASS
    } else {
        verdict
    };
    let mut msg = Message {
        m_source: 0,
        m_type: PM_EXIT,
        payload: [0u8; 96],
    };
    msg.payload[0..4].copy_from_slice(&status.to_ne_bytes());
    let _ = ipc_sendrec(pm, &mut msg);

    // Unreachable: PM never replies to a dead child.
    loop {
        core::hint::spin_loop()
    }
}

/// Check the SysV initial stack frame `SYS_EXEC` built, reading it off the real
/// `sp`.
///
/// Returns `0` when every check passes, else the **1-based number of the first
/// failing check** — which [`main`] turns into the exit status (a pass becomes
/// [`EXEC_STACK_PROBE_PASS`], never 0), so a boot log says not just that the
/// frame was wrong but which property broke:
///
/// 1. `sp` is 16-byte aligned (AAPCS64 at entry; `SCTLR_EL1.SA0` would otherwise
///    make the first stack access an alignment abort).
/// 2. `argc == 1`.
/// 3. `argv[0]` is the NUL-terminated exec name — the pointer *and* the bytes,
///    so an off-by-one on the string's VA is caught.
/// 4. `argv` is NULL-terminated.
/// 5. `envp` is empty (its NULL sits immediately after `argv`'s).
/// 6. `AT_PAGESZ` is the page size — musl copies this into `libc.page_size`.
/// 7. `AT_PHENT`/`AT_PHNUM` are sane and `AT_PHDR` really points at this image's
///    program headers (the first is a `PT_LOAD` at the link base) — what
///    `__init_tls` walks.
/// 8. The auxv terminates with `AT_NULL`.
#[cfg_attr(test, allow(dead_code))]
fn validate(sp: u64) -> i32 {
    if !sp.is_multiple_of(STACK_ALIGN as u64) {
        return 1;
    }
    // SAFETY (this call and every one below): `sp` is the stack pointer the
    // kernel installed, pointing at a frame it wrote into this process's own
    // mapped stack page, and the check above proved it 16-aligned — hence
    // 8-aligned, which is what each `u64` read here needs. The frame is at most
    // a few hundred bytes and every offset read below lies within it, so all of
    // these are reads of mapped, readable memory.
    if unsafe { rd_u64(sp) } != 1 {
        return 2;
    }
    let argv0 = unsafe { rd_u64(sp + 8) };
    if !unsafe { str_eq(argv0, ARGV0) } {
        return 3;
    }
    if unsafe { rd_u64(sp + 16) } != 0 {
        return 4;
    }
    if unsafe { rd_u64(sp + 24) } != 0 {
        return 5;
    }

    let auxv = sp + 32;
    if unsafe { auxv_get(auxv, AT_PAGESZ) } != Some(USER_PAGE_SIZE) {
        return 6;
    }
    if !unsafe { phdrs_are_readable(auxv) } {
        return 7;
    }
    if !unsafe { auxv_terminated(auxv) } {
        return 8;
    }
    0
}

/// Does `AT_PHDR` point at this image's program header table?
///
/// Checked against what the linker script pins rather than merely "some pointer
/// is present": `AT_PHENT` is ELF64's 56, `AT_PHNUM` is non-zero, and the first
/// entry at `AT_PHDR` is a `PT_LOAD` whose `p_vaddr` is the link base. That last
/// read is the real assertion — it dereferences the VA the kernel reported, so
/// it fails if the headers are not actually mapped there.
///
/// SAFETY: `auxv` addresses the frame's auxiliary vector (see [`validate`]). The
/// `AT_PHDR` dereference is guarded by the kernel's own contract: it emits
/// `AT_PHDR` only for a `PT_LOAD` whose file range covers the header table, so
/// the VA is inside a mapped segment of this image.
unsafe fn phdrs_are_readable(auxv: u64) -> bool {
    let (Some(phdr), Some(phent), Some(phnum)) = (
        unsafe { auxv_get(auxv, AT_PHDR) },
        unsafe { auxv_get(auxv, AT_PHENT) },
        unsafe { auxv_get(auxv, AT_PHNUM) },
    ) else {
        return false;
    };
    if phent != PHENT || phnum == 0 || !phdr.is_multiple_of(8) {
        return false;
    }
    // Elf64_Phdr: p_type at +0 (u32), p_vaddr at +16 (u64).
    unsafe { rd_u32(phdr) == PT_LOAD && rd_u64(phdr + 16) == LOAD_BASE }
}

/// Value of `want` in the auxiliary vector at `auxv`, or `None`.
///
/// SAFETY: `auxv` must address the frame's auxiliary vector; see [`validate`].
unsafe fn auxv_get(auxv: u64, want: u64) -> Option<u64> {
    for i in 0..AUXV_SCAN_MAX {
        let at = auxv + (i as u64) * 16;
        let a_type = unsafe { rd_u64(at) };
        if a_type == AT_NULL {
            return None;
        }
        if a_type == want {
            return Some(unsafe { rd_u64(at + 8) });
        }
    }
    None
}

/// Does the auxiliary vector at `auxv` terminate with `AT_NULL`?
///
/// SAFETY: `auxv` must address the frame's auxiliary vector; see [`validate`].
unsafe fn auxv_terminated(auxv: u64) -> bool {
    (0..AUXV_SCAN_MAX).any(|i| unsafe { rd_u64(auxv + (i as u64) * 16) } == AT_NULL)
}

/// Is the NUL-terminated string at `va` exactly `want`?
///
/// SAFETY: `va` must be a readable VA holding a NUL-terminated string; here it
/// is the `argv[0]` pointer out of the kernel-built frame, which points into
/// that same frame.
unsafe fn str_eq(va: u64, want: &[u8]) -> bool {
    for (i, &b) in want.iter().enumerate() {
        if unsafe { rd_u8(va + i as u64) } != b {
            return false;
        }
    }
    unsafe { rd_u8(va + want.len() as u64) == 0 }
}

/// SAFETY: `va` must be an 8-aligned, mapped, readable address.
unsafe fn rd_u64(va: u64) -> u64 {
    unsafe { core::ptr::read_volatile(va as *const u64) }
}

/// SAFETY: `va` must be a 4-aligned, mapped, readable address.
unsafe fn rd_u32(va: u64) -> u32 {
    unsafe { core::ptr::read_volatile(va as *const u32) }
}

/// SAFETY: `va` must be a mapped, readable address.
unsafe fn rd_u8(va: u64) -> u8 {
    unsafe { core::ptr::read_volatile(va as *const u8) }
}

/// `write(fd, buf, buf.len())` through VFS — the slice-5.4 path.
///
/// The buffer travels as a raw address in the worker's own address space: a user
/// process holds no grant table, and VFS is what turns the address into a magic
/// grant naming this process (taken from the kernel-stamped `m_source`, never
/// from the payload). Best-effort — the exit status is the primary channel.
#[cfg_attr(test, allow(dead_code))]
fn vfs_write(vfs: Endpoint, fd: i32, buf: &[u8]) {
    let mut m = Message {
        m_source: 0,
        m_type: VFS_WRITE,
        payload: [0u8; 96],
    };
    m.payload[VFS_FD_OFF..VFS_FD_OFF + 4].copy_from_slice(&fd.to_ne_bytes());
    m.payload[VFS_LEN_OFF..VFS_LEN_OFF + 4].copy_from_slice(&(buf.len() as i32).to_ne_bytes());
    m.payload[VFS_BUF_OFF..VFS_BUF_OFF + 8]
        .copy_from_slice(&(buf.as_ptr() as usize as u64).to_ne_bytes());
    let _ = ipc_sendrec(vfs, &mut m);
}

// The freestanding panic handler; under `cargo test` std supplies its own.
#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop()
    }
}
