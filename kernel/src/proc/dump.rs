// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Formatted UART dump of the populated process and privilege tables.
//!
//! The slice-2.2 milestone: after `proc::init()`, the kernel writes this
//! tabular view to the console so the operator can confirm the boot image
//! materialized correctly. Output style matches the spirit of the existing
//! exception-entry decode in `arch::aarch64::exception` (compact columns,
//! mnemonic decode of flag bits).

use core::fmt::{self, Write};
use core::sync::atomic::Ordering;

use arrayvec::ArrayString;

use super::flags::{
    BILLABLE, PREEMPTIBLE, ROOT_SYS_PROC, RTS_BOOTINHIBIT, RTS_NO_ENDPOINT, RTS_NO_PRIV,
    RTS_PAGEFAULT, RTS_PREEMPTED, RTS_PROC_STOP, RTS_RECEIVING, RTS_SENDING, RTS_SIGNALED,
    RTS_SIG_PENDING, RTS_SLOT_FREE, RTS_VMINHIBIT, SYS_PROC, VM_SYS_PROC,
};
use super::priv_struct::Priv;
use super::proc_struct::Proc;
use super::table::{n_image_slots, n_proc_slots, priv_table_ref, proc_table_ref};

const NAME_WIDTH: usize = 16;
const RTS_WIDTH: usize = 24;
const PRIV_FLAGS_WIDTH: usize = 24;

/// Dump the populated process + privilege tables to `out`.
///
/// SAFETY-adjacent: takes shared references to the static tables; callers
/// must ensure no exclusive borrow is live for the call. Slice 2.2 invokes
/// this once after `proc::init()` returns and never re-enters init, so the
/// requirement is trivially met.
pub fn dump_tables<W: Write>(out: &mut W) -> fmt::Result {
    // SAFETY: single-threaded boot context; `proc::init()` has returned, so
    // no `&mut` reference into PROC_TABLE / PRIV_TABLE is live.
    let procs = unsafe { proc_table_ref() };
    let privs = unsafe { priv_table_ref() };

    let active = procs.iter().filter(|p| !is_free(p)).count();
    writeln!(
        out,
        "Process table ({} slots, {} active):",
        n_proc_slots(),
        active,
    )?;
    writeln!(
        out,
        "   nr  endpoint    {:<NAME_WIDTH$}  priv  {:<RTS_WIDTH$}  prio  q_ms",
        "name", "rts",
    )?;
    for p in procs.iter() {
        if is_free(p) {
            continue;
        }
        dump_proc(out, p)?;
    }

    let used = privs.iter().filter(|pr| pr.proc_nr.is_some()).count();
    writeln!(out)?;
    writeln!(
        out,
        "Privilege table ({} used / {} total, image = {}):",
        used,
        privs.len(),
        n_image_slots(),
    )?;
    writeln!(
        out,
        "   id  proc-nr  {:<PRIV_FLAGS_WIDTH$}  trap    ipc_to                  k_call",
        "flags",
    )?;
    for pr in privs.iter() {
        if pr.proc_nr.is_none() {
            continue;
        }
        dump_priv(out, pr)?;
    }

    Ok(())
}

fn is_free(p: &Proc) -> bool {
    p.rts_flags.load(Ordering::Relaxed) & RTS_SLOT_FREE != 0
}

fn dump_proc<W: Write>(out: &mut W, p: &Proc) -> fmt::Result {
    writeln!(
        out,
        "  {:>3}  {:#010x}  {:<NAME_WIDTH$}  {:>4}  {:<RTS_WIDTH$}  {:>4}  {:>4}",
        p.nr.get(),
        p.endpoint as u32,
        NameDecoder(&p.name),
        PrivIdDisplay(p.priv_id),
        RtsDecoder(p.rts_flags.load(Ordering::Relaxed)),
        p.priority,
        p.quantum_ms,
    )
}

fn dump_priv<W: Write>(out: &mut W, pr: &Priv) -> fmt::Result {
    let proc_nr = pr.proc_nr.map(|n| n.get()).unwrap_or(i32::MIN);
    writeln!(
        out,
        "  {:>3}  {:>7}  {:<PRIV_FLAGS_WIDTH$}  {:#06x}  {:#010x} {:#010x}  {:#010x}",
        pr.id.get(),
        proc_nr,
        PrivFlagDecoder(pr.flags),
        pr.trap_mask,
        pr.ipc_to[0],
        pr.ipc_to[1],
        pr.k_call_mask[0],
    )
}

// ---------------------------------------------------------------------------
// Width-aware Display wrappers.
//
// `f.pad(s)` is the supported path for honouring `{:<width$}` from a custom
// Display impl: render into a fixed-size stack buffer, then hand the buffer
// to the formatter. ArrayString here keeps the dump zero-alloc.
// ---------------------------------------------------------------------------

const RENDER_BUF: usize = 96;

fn render_padded<F>(f: &mut fmt::Formatter<'_>, render: F) -> fmt::Result
where
    F: FnOnce(&mut ArrayString<RENDER_BUF>) -> fmt::Result,
{
    let mut buf: ArrayString<RENDER_BUF> = ArrayString::new();
    // Ignore overflow — the buffer is sized to fit every label we emit; if a
    // future label trips this, the truncated output still lands but signals
    // we need a bigger buffer.
    let _ = render(&mut buf);
    f.pad(buf.as_str())
}

struct NameDecoder<'a>(&'a [u8]);

impl<'a> fmt::Display for NameDecoder<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        render_padded(f, |buf| {
            for &b in self.0 {
                if b == 0 {
                    break;
                }
                let c = if b.is_ascii() && !b.is_ascii_control() {
                    b as char
                } else {
                    '?'
                };
                let _ = buf.try_push(c);
            }
            Ok(())
        })
    }
}

struct PrivIdDisplay(Option<minixrs_kernel_shared::PrivId>);

impl fmt::Display for PrivIdDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        render_padded(f, |buf| match self.0 {
            Some(id) => write!(buf, "{}", id.get()),
            None => buf.write_str("-"),
        })
    }
}

struct RtsDecoder(u32);

impl fmt::Display for RtsDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = self.0;
        render_padded(f, |buf| {
            if v == 0 {
                return buf.write_str("RUN");
            }
            let mut first = true;
            let pairs: [(u32, &str); 12] = [
                (RTS_PROC_STOP, "STOP"),
                (RTS_SENDING, "SEND"),
                (RTS_RECEIVING, "RECV"),
                (RTS_SIGNALED, "SIGD"),
                (RTS_SIG_PENDING, "SIGP"),
                (RTS_NO_PRIV, "NOPRIV"),
                (RTS_NO_ENDPOINT, "NOEPT"),
                (RTS_VMINHIBIT, "VMINH"),
                (RTS_PAGEFAULT, "PGFLT"),
                (RTS_PREEMPTED, "PREEMPT"),
                (RTS_BOOTINHIBIT, "BOOTINH"),
                (RTS_SLOT_FREE, "FREE"),
            ];
            for (bit, name) in pairs {
                if v & bit != 0 {
                    if !first {
                        buf.write_char('|')?;
                    }
                    buf.write_str(name)?;
                    first = false;
                }
            }
            Ok(())
        })
    }
}

struct PrivFlagDecoder(u16);

impl fmt::Display for PrivFlagDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = self.0;
        render_padded(f, |buf| {
            if v == 0 {
                return buf.write_str("-");
            }
            let mut first = true;
            let pairs: [(u16, &str); 5] = [
                (SYS_PROC, "SYS"),
                (PREEMPTIBLE, "PREEMPT"),
                (BILLABLE, "BILL"),
                (ROOT_SYS_PROC, "ROOT"),
                (VM_SYS_PROC, "VMSYS"),
            ];
            for (bit, name) in pairs {
                if v & bit != 0 {
                    if !first {
                        buf.write_char('|')?;
                    }
                    buf.write_str(name)?;
                    first = false;
                }
            }
            Ok(())
        })
    }
}
