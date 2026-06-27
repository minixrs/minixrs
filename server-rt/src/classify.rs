// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! SEF message classification — the pure, host-testable core of the framework.
//!
//! [`classify`] decides whether an incoming message is a SEF control event the
//! runtime handles itself or ordinary application traffic for the server. It
//! does no IPC, so (unlike the `sef` glue that traps into the kernel) it is
//! fully exercised by host unit tests — the same "testable logic lives in a
//! measured submodule" split `servers/vm/src/region.rs` uses.

use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{SEF_INIT, SEF_SIGNAL};
use minixrs_kernel_shared::com::{PM_PROC_NR, RS_PROC_NR, boot_endpoint};
use minixrs_kernel_shared::ipc_const::NOTIFY_MESSAGE;

/// Classification of an incoming message: a SEF control event the framework
/// handles itself, or ordinary application traffic for the server.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SefEvent {
    /// RS heartbeat — delivered as a NOTIFY from RS. Reply with a notify back.
    Ping,
    /// A signal carrying number `signo` (decoded from `payload[0..4]`).
    Signal(i32),
    /// A (re-)init request from RS.
    Init,
    /// Not a SEF control message — hand it back to the server.
    Application,
}

/// Classify `msg` as a SEF control event or application traffic. Pure (no IPC),
/// so it is the host-unit-tested core of the framework.
///
/// Every control event is gated on `m_source`, not `m_type` alone, so a client
/// that merely holds an `ipc_to` bit to the server cannot spoof one:
///
/// - **Ping** — an RS-sourced [`NOTIFY_MESSAGE`]. NOTIFY carries no payload to
///   distinguish senders, so the source *is* the discriminator; any other
///   NOTIFY is application traffic.
/// - **Signal** — `SEF_SIGNAL` from PM (POSIX signal delivery, slice 4.5) or RS
///   (lifecycle, e.g. a shutdown `SIGTERM`). From any other source it is treated
///   as application traffic, so an untrusted client cannot drive the registered
///   signal handler with an attacker-chosen `signo`.
/// - **Init** — `SEF_INIT` from RS alone, which owns server lifecycle.
///
/// SEF control `m_type`s live in a range (`0xD00`) distinct from every VM
/// request (`0xC00`), DS request (`0xE00`), and SCHED request (`0xF00`, incl.
/// the kernel-originated `SCHEDULING_NO_QUANTUM`), and below the NOTIFY marker,
/// so real server traffic — including a kernel→SCHED no-quantum message — falls
/// through to [`SefEvent::Application`] even before the source check.
pub fn classify(msg: &Message) -> SefEvent {
    let rs = boot_endpoint(RS_PROC_NR);
    let pm = boot_endpoint(PM_PROC_NR);
    match msg.m_type {
        NOTIFY_MESSAGE if msg.m_source == rs => SefEvent::Ping,
        SEF_SIGNAL if msg.m_source == pm || msg.m_source == rs => {
            let signo = i32::from_ne_bytes(
                msg.payload[0..4]
                    .try_into()
                    .expect("payload is at least 4 bytes"),
            );
            SefEvent::Signal(signo)
        }
        SEF_INIT if msg.m_source == rs => SefEvent::Init,
        _ => SefEvent::Application,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minixrs_kernel_shared::callnr::{VM_BRK, VM_MMAP, VM_MUNMAP, VM_PAGEFAULT};
    use minixrs_kernel_shared::com::VM_PROC_NR;
    use minixrs_kernel_shared::endpoint::Endpoint;

    fn msg(m_source: Endpoint, m_type: i32) -> Message {
        Message {
            m_source,
            m_type,
            payload: [0u8; 96],
        }
    }

    fn signal_msg(m_source: Endpoint, signo: i32) -> Message {
        let mut m = msg(m_source, SEF_SIGNAL);
        m.payload[0..4].copy_from_slice(&signo.to_ne_bytes());
        m
    }

    #[test]
    fn ping_is_notify_from_rs() {
        let m = msg(boot_endpoint(RS_PROC_NR), NOTIFY_MESSAGE);
        assert_eq!(classify(&m), SefEvent::Ping);
    }

    #[test]
    fn notify_from_non_rs_is_application() {
        // Same NOTIFY marker, different source — not a heartbeat.
        let m = msg(boot_endpoint(VM_PROC_NR), NOTIFY_MESSAGE);
        assert_eq!(classify(&m), SefEvent::Application);
    }

    #[test]
    fn vm_requests_are_application() {
        for t in [VM_PAGEFAULT, VM_BRK, VM_MMAP, VM_MUNMAP] {
            assert_eq!(
                classify(&msg(boot_endpoint(VM_PROC_NR), t)),
                SefEvent::Application,
            );
        }
    }

    #[test]
    fn sef_signal_from_pm_decodes_signo() {
        assert_eq!(
            classify(&signal_msg(boot_endpoint(PM_PROC_NR), 9)),
            SefEvent::Signal(9),
        );
    }

    #[test]
    fn sef_signal_from_rs_decodes_signo() {
        // RS may also raise signals (e.g. lifecycle SIGTERM on shutdown).
        assert_eq!(
            classify(&signal_msg(boot_endpoint(RS_PROC_NR), 15)),
            SefEvent::Signal(15),
        );
    }

    #[test]
    fn sef_signal_from_untrusted_source_is_application() {
        // A client that merely holds an ipc_to bit to the server (e.g. VM's
        // page-fault client) must not be able to spoof a signal.
        assert_eq!(
            classify(&signal_msg(boot_endpoint(VM_PROC_NR), 9)),
            SefEvent::Application,
        );
    }

    #[test]
    fn sef_init_from_rs_is_init() {
        let m = msg(boot_endpoint(RS_PROC_NR), SEF_INIT);
        assert_eq!(classify(&m), SefEvent::Init);
    }

    #[test]
    fn sef_init_from_non_rs_is_application() {
        // Only RS owns server lifecycle; a SEF_INIT from anyone else (even PM)
        // is application traffic, not a real re-init.
        let m = msg(boot_endpoint(PM_PROC_NR), SEF_INIT);
        assert_eq!(classify(&m), SefEvent::Application);
    }
}
