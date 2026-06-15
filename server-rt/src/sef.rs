// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! SEF startup handshake and control-aware receive loop.

use minixrs_ipc::{ipc_notify, ipc_receive, ipc_sendrec};
use minixrs_kernel_shared::Message;
use minixrs_kernel_shared::callnr::{GET_WHOAMI, SYS_GETINFO, SYS_GETINFO_NAME_LEN};
use minixrs_kernel_shared::com::{RS_PROC_NR, SYSTEM, boot_endpoint};
use minixrs_kernel_shared::endpoint::{ANY, Endpoint};
use minixrs_kernel_shared::error::OK;

use crate::classify::{SefEvent, classify};
use crate::init::SefInitCb;
use crate::signal::SefSignalCb;

/// Callbacks a server registers with the framework at startup. Both are
/// optional: a server with no init work or no signal handling passes `None`.
pub struct SefConfig {
    /// Run once after the startup handshake (see [`SefInitCb`]).
    pub init_fresh: Option<SefInitCb>,
    /// Invoked for each delivered signal (see [`SefSignalCb`]).
    pub signal_handler: Option<SefSignalCb>,
}

/// A live SEF session, returned by [`sef_startup`]. Carries the server's own
/// endpoint + name (learned via `GET_WHOAMI`) and its registered callbacks, so
/// [`Sef::receive`] needs no static state. Cheap to copy (an endpoint, a 16-byte
/// name, and a function pointer).
#[derive(Copy, Clone)]
pub struct Sef {
    endpoint: Endpoint,
    name: [u8; SYS_GETINFO_NAME_LEN],
    signal_handler: Option<SefSignalCb>,
}

/// Perform the SEF startup handshake: learn the server's own endpoint and name
/// from the kernel via `SYS_GETINFO(GET_WHOAMI)` (a SENDREC to `SYSTEM`), then
/// run the registered fresh-init callback. Returns the [`Sef`] handle to drive
/// the receive loop, or a negative error code if the handshake or init failed.
pub fn sef_startup(cfg: SefConfig) -> Result<Sef, i32> {
    // SYS_GETINFO(GET_WHOAMI): request selector goes in payload[0..4]; the
    // kernel replies in-place with our endpoint (0..4), priv flags (4..8),
    // init flags (8..12), and name (12..28), setting m_type = OK.
    let mut msg = Message {
        m_source: 0,
        m_type: SYS_GETINFO,
        payload: [0u8; 96],
    };
    msg.payload[0..4].copy_from_slice(&GET_WHOAMI.to_ne_bytes());

    let trap_rc = ipc_sendrec(boot_endpoint(SYSTEM), &mut msg);
    if trap_rc != OK {
        return Err(trap_rc);
    }
    if msg.m_type != OK {
        return Err(msg.m_type);
    }

    let endpoint = i32::from_ne_bytes(
        msg.payload[0..4]
            .try_into()
            .expect("payload is at least 4 bytes"),
    );
    let mut name = [0u8; SYS_GETINFO_NAME_LEN];
    name.copy_from_slice(&msg.payload[12..12 + SYS_GETINFO_NAME_LEN]);

    let sef = Sef {
        endpoint,
        name,
        signal_handler: cfg.signal_handler,
    };

    if let Some(init) = cfg.init_fresh {
        let rc = init(endpoint, &name);
        if rc != OK {
            return Err(rc);
        }
    }

    Ok(sef)
}

impl Sef {
    /// This server's own endpoint, as learned at startup.
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// This server's name (NUL-padded), as learned at startup.
    pub fn name(&self) -> &[u8; SYS_GETINFO_NAME_LEN] {
        &self.name
    }

    /// Receive the next *application* message into `msg`, transparently handling
    /// any SEF control messages that arrive first. Returns `OK` (0) once an
    /// application message is in `msg`, or a negative error code if the
    /// underlying receive failed (the server typically just loops and retries,
    /// matching the old hand-rolled `if ipc_receive(..) != 0 { continue }`).
    pub fn receive(&self, msg: &mut Message) -> i32 {
        loop {
            let rc = ipc_receive(ANY, msg);
            if rc != OK {
                return rc;
            }
            match classify(msg) {
                SefEvent::Application => return OK,
                SefEvent::Ping => {
                    // Acknowledge the RS heartbeat. A deferred notify still
                    // returns OK; any error just means RS re-pings later, so we
                    // ignore the result and wait for the next message.
                    //
                    // TODO(phase 4, RS heartbeat): this ack needs the server's
                    // own `ipc_to` bit for RS to be open (cf. `install_stub_d_priv`
                    // opening VM→D). Until RS actually pings, that reverse edge is
                    // unwired, so the ack would return `ECALLDENIED` — harmless
                    // while the heartbeat is dormant, but the wiring must land
                    // with the heartbeat or RS will conclude the server is dead.
                    let _ = ipc_notify(boot_endpoint(RS_PROC_NR));
                }
                SefEvent::Signal(signo) => {
                    if let Some(handler) = self.signal_handler {
                        handler(signo);
                    }
                }
                SefEvent::Init => {
                    // Fresh init already ran in sef_startup; the minimal
                    // Phase-4 SEF has nothing to do for a re-init request.
                    //
                    // TODO(phase 4, RS-driven re-init): nail down the reply
                    // contract before RS sends this. If RS issues SEF_INIT as a
                    // SENDREC it blocks until the server replies — this arm would
                    // then have to send an ack or RS hangs. Nothing sends SEF_INIT
                    // yet, so swallowing it is correct for now. The same caveat
                    // applies to a SENDREC-delivered SEF_SIGNAL.
                }
            }
        }
    }
}
