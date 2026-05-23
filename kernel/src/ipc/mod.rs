//! IPC subsystem.
//!
//! Slice 2.3 ships only `do_ipc` as an observable stub — it prints the
//! caller's IPC arguments to the UART and stores `OK` in the caller's `x0`,
//! proving that the EL0 → SVC → kernel → eret round-trip works end-to-end.
//!
//! Slice 2.5 replaces this with the real IPC engine: `mini_send`,
//! `mini_receive`, `mini_notify`, the `caller_q` traversal, `MF_DELIVERMSG`
//! delivery, and trap-mask enforcement.

use core::fmt::Write;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::arch::ArchRegisterFrame;
use crate::uart::Uart;

static CALL_COUNT: AtomicU32 = AtomicU32::new(0);

/// IPC dispatch.
///
/// Called from `trap.S` immediately after the SVC entry stub has saved the
/// caller's registers into `frame`. MINIX-style argument convention:
///
/// | reg | meaning                       |
/// |-----|-------------------------------|
/// | x0  | source/destination endpoint   |
/// | x1  | call number (SEND, SENDREC …) |
/// | x2  | pointer to the user's Message |
///
/// The slice-2.3 stub doesn't actually deliver a message anywhere — it just
/// logs the call and synthesizes an `OK` return value in `frame.x[0]`.
#[unsafe(no_mangle)]
pub extern "C" fn do_ipc(frame: &mut ArchRegisterFrame) {
    let src_dst = frame.x[0] as i32;
    let call_nr = frame.x[1] as i32;
    let msg_addr = frame.x[2];

    let n = CALL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    let mut uart = Uart::new();
    let _ = writeln!(
        uart,
        "do_ipc[{n}]: call_nr={call_nr} src_dst={src_dst} msg={msg_addr:#018x}",
    );

    // OK return code, MINIX-style. The SVC restore path puts this in the
    // caller's x0.
    frame.x[0] = 0;
}
