// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `SYS_DIAGCTL` — the servers' debug channel (slice 5.1, decision D2).
//!
//! Replaces the slice-2.6 `ENOSYS` placeholder. User-space servers run at EL0
//! with no console of their own; before this call the only way to observe them
//! was indirectly, through kernel-side `[ipc]` / `[ksys]` / `[pf]` traces. Every
//! remaining Phase 5 slice needs first-party server output while grants, TTY,
//! VFS and MFS are still being built, so the debug channel lands early.
//!
//! MINIX 3 (`kernel/system/do_diagctl.c`) passes a `(buf, len)` user pointer and
//! `data_copy`s the text in. minix.rs carries the text **inline in the message
//! payload** instead: the channel then needs no user-copy machinery, cannot
//! fault, and works even while the copy engine itself is under construction.
//! The cost is a per-call text budget of [`DIAG_TEXT_MAX`] bytes;
//! `server-rt::diag_print` splits longer strings across successive calls.
//!
//! This is a *caller-local* kernel call — it acts only on the caller — so it
//! takes a single `&mut Proc` and is dispatched through
//! [`dispatch_caller_local`](super::dispatch_caller_local), unlike the
//! target-taking `do_vmctl` / `do_schedule`.
//!
//! ## Message payload layout (offsets within `Message::payload`)
//!
//! | offset                              | field                        | direction |
//! |-------------------------------------|------------------------------|-----------|
//! | 0..4                                | subcode (i32), `DIAGCTL_CODE_*` | in     |
//! | 4..8                                | text length (i32)            | in        |
//! | `DIAG_TEXT_OFF..DIAG_TEXT_OFF+len`  | text bytes                   | in        |
//!
//! No reply payload; `m_type` is `OK` or `EINVAL`.
//!
//! ## Trust
//!
//! The `[diag <name>]` prefix is the caller's name **as the kernel knows it**
//! (`Proc::name`, set at load time), never anything from the payload — a server
//! can only ever identify itself, the same anti-spoof property DS relies on for
//! `m_source`. Text bytes are restricted to printable ASCII, so a caller cannot
//! inject newlines or escapes that would break the one-call-one-line framing
//! that `tests/qemu-boot.expected` greps against.
//!
//! No privilege gate beyond the usual one: `Priv::k_call_mask` already limits
//! kernel calls to server-grade privs, and the shared USER priv has an empty
//! mask, so ordinary user processes cannot reach this call at all.

use core::fmt::Write;

use minixrs_kernel_shared::callnr::{
    DIAG_TEXT_MAX, DIAG_TEXT_OFF, DIAGCTL_CODE_DIAG, DIAGCTL_CODE_REGISTER,
    DIAGCTL_CODE_STACKTRACE, DIAGCTL_CODE_UNREGISTER,
};
use minixrs_kernel_shared::error::{EINVAL, OK};
use minixrs_kernel_shared::message::Message;

use crate::proc::{Priv, Proc};
use crate::uart::Uart;

/// `SYS_DIAGCTL`. Prints one console line of caller-supplied text.
pub(super) fn do_diagctl(caller: &mut Proc, _caller_priv: &Priv, msg: &mut Message) -> i32 {
    match read_i32(msg, 0) {
        DIAGCTL_CODE_DIAG => diag_print(caller, msg),
        // Reserved MINIX 3 subcodes. Kernel message-log subscription
        // (REGISTER/UNREGISTER) and stack traces need machinery minix.rs does
        // not have yet; the numbers are held so they cannot be reused.
        DIAGCTL_CODE_STACKTRACE | DIAGCTL_CODE_REGISTER | DIAGCTL_CODE_UNREGISTER => EINVAL,
        _ => EINVAL,
    }
}

/// `DIAGCTL_CODE_DIAG`: emit `[diag <name>] <text>`.
///
/// Deliberately unsampled — a sampled debug channel is useless. The rate is
/// caller-controlled, so a runaway server can saturate the PL011 and crowd out
/// the boot log; that is the same bargain MINIX makes, and the alternative
/// (dropping lines) would make the channel untrustworthy.
fn diag_print(caller: &Proc, msg: &Message) -> i32 {
    let len = read_i32(msg, 4);
    if len < 0 || len as usize > DIAG_TEXT_MAX {
        return EINVAL;
    }
    let len = len as usize;

    let mut uart = Uart::new();
    let _ = write!(uart, "[diag {}] ", NameStr(&caller.name));
    for &b in &msg.payload[DIAG_TEXT_OFF..DIAG_TEXT_OFF + len] {
        // Printable ASCII only; anything else (newline, CR, escape, high bit)
        // becomes '.' so one call is always exactly one line.
        let c = if (0x20..=0x7E).contains(&b) { b } else { b'.' };
        let _ = uart.write_char(c as char);
    }
    let _ = uart.write_char('\n');
    OK
}

/// Render a NUL-padded `Proc::name` as text, stopping at the first NUL and
/// substituting `?` for any non-printable byte.
///
/// A `Display` wrapper rather than a `str` conversion so a name that is not
/// valid UTF-8 still prints something identifying instead of collapsing the
/// whole prefix. Slice 4.7's `do_exec` does the equivalent inline; this call
/// prints far more often, so it gets a helper.
struct NameStr<'a>(&'a [u8]);

impl core::fmt::Display for NameStr<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for &b in self.0 {
            if b == 0 {
                break;
            }
            let c = if (0x20..=0x7E).contains(&b) { b } else { b'?' };
            f.write_char(c as char)?;
        }
        Ok(())
    }
}

/// Read a native-endian i32 from payload `off..off+4`.
fn read_i32(m: &Message, off: usize) -> i32 {
    i32::from_ne_bytes(
        m.payload[off..off + 4]
            .try_into()
            .expect("payload in range"),
    )
}
