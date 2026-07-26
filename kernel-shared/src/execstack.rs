// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The SysV/Linux initial process stack that `exec` hands a new image.
//!
//! musl's `crt1` → `__libc_start_main` → `__init_libc` reads `argc`, `argv`,
//! `envp` and the auxiliary vector straight off the stack in the Linux/SysV
//! shape, and takes `libc.page_size` out of `AT_PAGESZ`; `__init_tls` walks the
//! program headers from `AT_PHDR`. Phase 5's standing principle (D13) is to keep
//! the musl fork's diff to `arch/aarch64/syscall_arch.h` + `src/minix/` — nothing
//! in crt — which only holds if the *kernel* hands over a real SysV frame. This
//! module is where that frame's byte layout is defined and proved.
//!
//! The frame is laid out upward from `sp`, which the AAPCS64 requires to be
//! 16-byte aligned at entry (`SCTLR_EL1.SA0` turns a violation into an EL0
//! alignment abort):
//!
//! ```text
//! sp  + 0   argc                            (u64)
//!     + 8   argv[0] -> the name string      (u64, a VA in the new image)
//!     +16   argv NULL terminator
//!     +24   envp NULL terminator            (the environment is empty)
//!     +32   auxv pairs, 16 bytes each: (a_type, a_val)
//!     +...  AT_NULL, 0                      (auxv terminator)
//!     +...  the NUL-terminated name string
//!     +...  zero padding up to `stack_top`
//! ```
//!
//! There is exactly one argument (`argc == 1`, `argv[0]` the exec name) and no
//! environment: `SYS_EXEC` names a boot-embedded binary, and a user-supplied
//! `argv`/`envp` arrives with the Phase-5 filesystem. Auxv order is fixed by the
//! caller rather than inherited from Linux's incidental ordering, so the kernel
//! trace and these tests stay deterministic.
//!
//! Pure and allocation-free, with zero `unsafe`: the kernel crate has no
//! `#[cfg(test)]` (it is bare-metal only), so host-runnable logic over a shared
//! ABI shape belongs here — the [`crate::message::user_va_ok`] /
//! [`crate::message::page_chunks`] precedent. The kernel builds the frame into a
//! stack buffer and copies it into the new address space with
//! `mm::uaccess::copy_to_user_as`.
//!
//! The `AT_*` values are the Linux/SysV ones (`elf.h`), which is the whole point
//! — musl defines them itself, so they are deliberately **not** emitted by
//! `tools/gen-c-headers`.
//!
//! ## What is deliberately *not* in the vector
//!
//! musl's `__init_libc` decodes the auxv into `size_t aux[AUX_CNT] = { 0 }`, so
//! an entry the kernel omits simply reads back as 0. Checked against the fork's
//! `src/env/__libc_start_main.c`, that makes the following safe to leave out for
//! now — but each is a real entry a later slice may need to add:
//!
//!   - `AT_RANDOM` — musl passes it straight to `__init_ssp`, which falls back
//!     to `(uintptr_t)&__stack_chk_guard * 1103515245` when it is null. So the
//!     stack-protector canary works, but it is *derived from an address*, not
//!     entropy. Revisit once there is a randomness source (slice 5.6 confirms
//!     the SSP path actually runs; hardening is later).
//!   - `AT_UID`/`AT_EUID`/`AT_GID`/`AT_EGID`/`AT_SECURE` — all-zero means
//!     uid == euid and `!AT_SECURE`, so `__init_libc` takes its early return and
//!     skips the secure-mode descriptor fixup. Correct for a system with no
//!     credentials yet; it becomes wrong the moment set-uid exists.
//!   - `AT_HWCAP`, `AT_SYSINFO`, `AT_EXECFN`, `AT_BASE`, `AT_ENTRY` — unused,
//!     guarded, or (for `AT_EXECFN`) covered because `crt1` passes `argv[0]` as
//!     the program name explicitly.
//!
//! [`AT_PAGESZ`], by contrast, is **not** optional: `libc.page_size =
//! aux[AT_PAGESZ]` is unconditional, so omitting it leaves musl believing the
//! page size is 0. It is always emitted, and a boot marker counts the pairs.

use crate::callnr::EXEC_NAME_LEN;

/// End of the auxiliary vector. The pair `(AT_NULL, 0)` terminates it.
pub const AT_NULL: u64 = 0;
/// Address of the program headers in the new image (`__init_tls` walks these).
pub const AT_PHDR: u64 = 3;
/// Size of one program header entry (`e_phentsize`; 56 for ELF64).
pub const AT_PHENT: u64 = 4;
/// Number of program header entries (`e_phnum`).
pub const AT_PHNUM: u64 = 5;
/// System page size — musl copies this into `libc.page_size`.
pub const AT_PAGESZ: u64 = 6;

/// Stack alignment the AAPCS64 requires of `sp` at process entry.
pub const STACK_ALIGN: usize = 16;

/// Exit status `userland/worker` uses to say "I checked the initial stack and it
/// was correct" — the boot probe's **positive** verdict.
///
/// Not part of the exec ABI: it is a handshake between the two userland probe
/// programs, and it lives here only because `kernel-shared` is the one crate
/// both `userland/init` and `userland/worker` already depend on.
///
/// It is a distinctive non-zero value rather than the obvious `0` because
/// *silence must not read as success*. PM's kill path (`mproc::handle_kill_in`)
/// marks a signalled process a zombie **without touching `exit_status`**, so a
/// worker that died before reporting — say it faulted dereferencing a pointer
/// out of a malformed frame, which VM turns into a SIGSEGV rather than an
/// unhandled EL0 abort — is reaped with status 0. With `0` meaning "pass", that
/// is a boot log cheerfully announcing that the frame it never managed to read
/// was fine.
pub const EXEC_STACK_PROBE_PASS: i32 = 0x5A;

// The status round-trips through MINIX's `W_EXITCODE` (`(s & 0xff) << 8`), so it
// has to survive being squeezed into one byte, and 0 is the value it exists to
// be distinguishable from.
const _: () = assert!(EXEC_STACK_PROBE_PASS > 0 && EXEC_STACK_PROBE_PASS <= 0xff);

/// Fixed head of the frame: `argc`, one `argv` pointer, the `argv` NULL, and the
/// `envp` NULL — four 8-byte words before the auxiliary vector starts.
const HEAD_LEN: usize = 32;

/// Bytes per auxv entry: an `(a_type, a_val)` pair of `u64`s.
const AUXV_ENTRY_LEN: usize = 16;

/// Most real auxv pairs the kernel ever emits (`AT_PHDR`, `AT_PHNUM`,
/// `AT_PHENT`, `AT_PAGESZ`), excluding the `AT_NULL` terminator.
pub const MAX_AUXV: usize = 4;

/// Upper bound on the frame [`build_initial_stack`] can produce for any exec the
/// kernel can currently issue, so the caller can stage it through a fixed stack
/// buffer with no allocator. Covers [`MAX_AUXV`] pairs plus their terminator and
/// the longest name `SYS_EXEC` accepts.
pub const INITIAL_STACK_MAX: usize = 256;

const _: () = assert!(
    INITIAL_STACK_MAX
        >= HEAD_LEN + AUXV_ENTRY_LEN * (MAX_AUXV + 1) + EXEC_NAME_LEN + 1 + (STACK_ALIGN - 1)
);

/// Where [`build_initial_stack`] placed the frame it just wrote.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct InitialStack {
    /// Virtual address the frame begins at — the value `sp` must hold at entry,
    /// pointing at `argc`. 16-byte aligned.
    pub sp: u64,
    /// Bytes written, starting at `buf[0]`. `buf[0]` corresponds to VA [`sp`].
    pub len: usize,
    /// VA of the NUL-terminated `argv[0]` string within the frame — the value
    /// stored in the `argv[0]` slot.
    pub argv0_va: u64,
}

/// Build the initial stack frame for a process starting at `stack_top`.
///
/// Writes the frame into `buf[..len]`, where `buf[0]` corresponds to the
/// returned `sp` and `sp + len == stack_top`. `argv0` becomes the single
/// argument; `auxv` is emitted verbatim, in the given order, before the
/// `AT_NULL` terminator this function appends.
///
/// Returns `None` — writing nothing — when the frame cannot be built:
///
///   - `stack_top` is not [`STACK_ALIGN`]-aligned (the frame's own alignment is
///     derived from it, so a misaligned top cannot yield an aligned `sp`);
///   - `argv0` is empty or contains a NUL (it is stored NUL-terminated, so an
///     embedded NUL would silently truncate the name the program sees);
///   - the frame does not fit in `buf`, or `stack_top` is too low to hold it.
pub fn build_initial_stack(
    buf: &mut [u8],
    stack_top: u64,
    argv0: &str,
    auxv: &[(u64, u64)],
) -> Option<InitialStack> {
    if !stack_top.is_multiple_of(STACK_ALIGN as u64) {
        return None;
    }
    let name = argv0.as_bytes();
    if name.is_empty() || name.contains(&0) {
        return None;
    }

    // Everything up to the name string: the fixed head, the caller's auxv pairs,
    // and the AT_NULL terminator.
    let name_off = auxv
        .len()
        .checked_add(1)?
        .checked_mul(AUXV_ENTRY_LEN)?
        .checked_add(HEAD_LEN)?;
    let unpadded = name_off.checked_add(name.len())?.checked_add(1)?;
    // Round the whole frame up so `sp = stack_top - total` stays 16-aligned; the
    // padding lands *above* the name string, past everything the program reads.
    let total = unpadded
        .checked_add(STACK_ALIGN - 1)?
        .checked_div(STACK_ALIGN)?
        .checked_mul(STACK_ALIGN)?;
    if total > buf.len() {
        return None;
    }
    let sp = stack_top.checked_sub(total as u64)?;
    let argv0_va = sp.checked_add(name_off as u64)?;

    let frame = buf.get_mut(..total)?;
    // Zero first: the argv/envp NULLs, the AT_NULL pair, the name's terminator,
    // and the tail padding are all holes this fill leaves correct.
    frame.fill(0);
    wr_u64(frame, 0, 1)?;
    wr_u64(frame, 8, argv0_va)?;
    // frame[16..24] argv NULL, frame[24..32] envp NULL — already zero.
    for (i, &(a_type, a_val)) in auxv.iter().enumerate() {
        let off = HEAD_LEN.checked_add(i.checked_mul(AUXV_ENTRY_LEN)?)?;
        wr_u64(frame, off, a_type)?;
        wr_u64(frame, off.checked_add(8)?, a_val)?;
    }
    // The (AT_NULL, 0) pair at `name_off - AUXV_ENTRY_LEN` — already zero.
    frame
        .get_mut(name_off..name_off.checked_add(name.len())?)?
        .copy_from_slice(name);

    Some(InitialStack {
        sp,
        len: total,
        argv0_va,
    })
}

/// Store one native-endian `u64` at `off`, or `None` if it would not fit.
///
/// Native rather than explicitly little-endian: this is a *memory image* built
/// for the process that will read it back with ordinary loads, not a file or
/// wire format (contrast the ELF loader's `from_le_bytes`).
fn wr_u64(buf: &mut [u8], off: usize, v: u64) -> Option<()> {
    let end = off.checked_add(8)?;
    buf.get_mut(off..end)?.copy_from_slice(&v.to_ne_bytes());
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frame the kernel actually builds today: all four auxv pairs, and the
    /// stack top `load_exec_image` maps.
    const TOP: u64 = 0x0020_0000;
    const AUXV: [(u64, u64); 4] = [
        (AT_PHDR, 0x0010_0040),
        (AT_PHNUM, 3),
        (AT_PHENT, 56),
        (AT_PAGESZ, 4096),
    ];

    fn rd_u64(buf: &[u8], off: usize) -> u64 {
        u64::from_ne_bytes(buf[off..off + 8].try_into().unwrap())
    }

    #[test]
    fn layout_is_exactly_the_sysv_frame() {
        let mut buf = [0xAAu8; INITIAL_STACK_MAX];
        let f = build_initial_stack(&mut buf, TOP, "worker", &AUXV).unwrap();

        // 32 head + 5 auxv pairs (4 real + AT_NULL) + "worker\0" = 119 -> 128.
        assert_eq!(f.len, 128);
        assert_eq!(f.sp, TOP - 128);
        assert_eq!(f.sp + f.len as u64, TOP);

        assert_eq!(rd_u64(&buf, 0), 1, "argc");
        assert_eq!(rd_u64(&buf, 8), f.argv0_va, "argv[0]");
        assert_eq!(rd_u64(&buf, 16), 0, "argv NULL");
        assert_eq!(rd_u64(&buf, 24), 0, "envp NULL");

        for (i, &(a_type, a_val)) in AUXV.iter().enumerate() {
            assert_eq!(rd_u64(&buf, 32 + i * 16), a_type, "auxv[{i}].a_type");
            assert_eq!(rd_u64(&buf, 32 + i * 16 + 8), a_val, "auxv[{i}].a_val");
        }
        // The terminator, then the string right after it.
        assert_eq!(rd_u64(&buf, 32 + 4 * 16), AT_NULL);
        assert_eq!(rd_u64(&buf, 32 + 4 * 16 + 8), 0);
        assert_eq!(&buf[112..119], b"worker\0");
    }

    #[test]
    fn argv0_pointer_addresses_the_string_in_the_frame() {
        let mut buf = [0u8; INITIAL_STACK_MAX];
        let f = build_initial_stack(&mut buf, TOP, "worker", &AUXV).unwrap();

        // The stored pointer is a VA; its offset into the frame must index the
        // NUL-terminated name. An off-by-one here is what the boot marker catches.
        let off = (f.argv0_va - f.sp) as usize;
        assert_eq!(&buf[off..off + 7], b"worker\0");
        assert_eq!(rd_u64(&buf, 8), f.sp + off as u64);
    }

    #[test]
    fn sp_is_16_aligned_for_every_name_length() {
        // AAPCS64 requires it at entry, and SCTLR_EL1.SA0 makes a violation an
        // EL0 alignment abort — so this must hold whatever the name rounds to.
        for len in 1..=EXEC_NAME_LEN {
            let name = "w".repeat(len);
            let mut buf = [0u8; INITIAL_STACK_MAX];
            let f = build_initial_stack(&mut buf, TOP, &name, &AUXV).unwrap();
            assert_eq!(f.sp % STACK_ALIGN as u64, 0, "len={len}");
            assert_eq!(f.len % STACK_ALIGN, 0, "len={len}");
        }
    }

    #[test]
    fn padding_and_holes_are_zeroed_over_dirty_memory() {
        let mut buf = [0xFFu8; INITIAL_STACK_MAX];
        let f = build_initial_stack(&mut buf, TOP, "worker", &AUXV).unwrap();
        // Everything past the name up to the frame end is padding.
        assert!(buf[119..f.len].iter().all(|&b| b == 0));
        // ...and nothing was written past the frame.
        assert!(buf[f.len..].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn auxv_is_at_null_terminated_even_when_empty() {
        let mut buf = [0xFFu8; INITIAL_STACK_MAX];
        let f = build_initial_stack(&mut buf, TOP, "w", &[]).unwrap();
        assert_eq!(rd_u64(&buf, 32), AT_NULL);
        assert_eq!(rd_u64(&buf, 40), 0);
        assert_eq!(f.argv0_va, f.sp + 48);
        assert_eq!(&buf[48..50], b"w\0");
    }

    #[test]
    fn auxv_order_is_preserved_verbatim() {
        // The kernel picks the order so the trace and these tests are stable;
        // the builder must not sort or reorder.
        let reversed: [(u64, u64); 4] = [AUXV[3], AUXV[2], AUXV[1], AUXV[0]];
        let mut buf = [0u8; INITIAL_STACK_MAX];
        build_initial_stack(&mut buf, TOP, "worker", &reversed).unwrap();
        for (i, &(a_type, _)) in reversed.iter().enumerate() {
            assert_eq!(rd_u64(&buf, 32 + i * 16), a_type);
        }
    }

    #[test]
    fn rejects_a_buffer_too_small_for_the_frame() {
        let mut exact = [0u8; 128];
        assert!(build_initial_stack(&mut exact, TOP, "worker", &AUXV).is_some());
        let mut one_short = [0xFFu8; 127];
        assert_eq!(
            build_initial_stack(&mut one_short, TOP, "worker", &AUXV),
            None
        );
        // Nothing was written on the rejection.
        assert!(one_short.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn rejects_a_misaligned_stack_top() {
        let mut buf = [0u8; INITIAL_STACK_MAX];
        assert_eq!(
            build_initial_stack(&mut buf, TOP - 8, "worker", &AUXV),
            None
        );
        assert_eq!(
            build_initial_stack(&mut buf, TOP - 1, "worker", &AUXV),
            None
        );
    }

    #[test]
    fn rejects_an_empty_or_nul_bearing_name() {
        let mut buf = [0u8; INITIAL_STACK_MAX];
        assert_eq!(build_initial_stack(&mut buf, TOP, "", &AUXV), None);
        assert_eq!(build_initial_stack(&mut buf, TOP, "wor\0ker", &AUXV), None);
        assert_eq!(build_initial_stack(&mut buf, TOP, "worker\0", &AUXV), None);
    }

    #[test]
    fn rejects_a_stack_top_too_low_to_hold_the_frame() {
        let mut buf = [0u8; INITIAL_STACK_MAX];
        assert_eq!(build_initial_stack(&mut buf, 0, "worker", &AUXV), None);
        assert_eq!(build_initial_stack(&mut buf, 112, "worker", &AUXV), None);
        // ...and the smallest top that does fit works.
        assert_eq!(
            build_initial_stack(&mut buf, 128, "worker", &AUXV).map(|f| f.sp),
            Some(0)
        );
    }

    #[test]
    fn the_kernels_worst_case_frame_fits_the_staging_buffer() {
        // INITIAL_STACK_MAX exists so the kernel can stage the frame on its own
        // stack; prove the bound with the longest name SYS_EXEC accepts.
        let name = "n".repeat(EXEC_NAME_LEN);
        let auxv = [(AT_PHDR, u64::MAX); MAX_AUXV];
        let mut buf = [0u8; INITIAL_STACK_MAX];
        let f = build_initial_stack(&mut buf, TOP, &name, &auxv).unwrap();
        assert_eq!(f.len.min(INITIAL_STACK_MAX), f.len);
    }
}
