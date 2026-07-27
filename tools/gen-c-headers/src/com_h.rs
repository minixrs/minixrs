// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `minixrs/com.h` — process numbers and the endpoints derived from them.

use minixrs_kernel_shared::{ProcNr, com, error};

use crate::builder::CFile;

/// Include guard for the generated header.
pub const GUARD: &str = "_MINIXRS_COM_H";

/// Every process that gets a `_PROC_NR` / `_EP` macro pair, in slot order.
///
/// The C name is spelled here rather than taken from the Rust constant name
/// because the kernel tasks are `com::SYSTEM` (no suffix) on the Rust side but
/// must be `SYSTEM_PROC_NR` / `SYSTEM_EP` in C, where a bare `SYSTEM` would be
/// an unpleasant macro to inflict on a C translation unit.
///
/// The array length is the live `NR_TASKS + NR_BOOT_PROCS`, not a literal: a new
/// boot server in `com.rs` bumps `NR_BOOT_PROCS` and this array then fails to
/// compile until its row is added, so the C view of the process table cannot
/// silently lag the Rust one. `the_process_list_covers_every_task_and_boot_slot`
/// locks the slot values that fill it.
fn processes() -> [(&'static str, ProcNr); com::NR_TASKS + com::NR_BOOT_PROCS] {
    [
        ("ASYNCM", com::ASYNCM),
        ("IDLE", com::IDLE),
        ("CLOCK", com::CLOCK),
        ("SYSTEM", com::SYSTEM),
        ("HARDWARE", com::HARDWARE),
        ("PM", com::PM_PROC_NR),
        ("VFS", com::VFS_PROC_NR),
        ("RS", com::RS_PROC_NR),
        ("MEM", com::MEM_PROC_NR),
        ("TTY", com::TTY_PROC_NR),
        ("DS", com::DS_PROC_NR),
        ("MFS", com::MFS_PROC_NR),
        ("VM", com::VM_PROC_NR),
        ("PFS", com::PFS_PROC_NR),
        ("SCHED", com::SCHED_PROC_NR),
        ("INIT", com::INIT_PROC_NR),
    ]
}

/// Render `minixrs/com.h`.
pub fn render() -> String {
    let mut f = CFile::new(
        "Process numbers, boot endpoints, and process-table capacities.",
        &["kernel-shared/src/com.rs", "kernel-shared/src/error.rs"],
    );
    f.guard_open(GUARD);

    f.blank();
    f.include(
        "minixrs/ipc.h",
        "endpoint_t and the _ENDPOINT* packing macros",
    );

    f.section("process-table capacities");
    f.define_dec("NR_TASKS", com::NR_TASKS as i64);
    f.define_dec("NR_PROCS", com::NR_PROCS as i64);
    f.define_dec("NR_SYS_PROCS", com::NR_SYS_PROCS as i64);
    f.define_dec("NR_BOOT_PROCS", com::NR_BOOT_PROCS as i64);

    f.section("success sentinel");
    f.block_comment(&[
        "MINIX 3 keeps OK in <minix/const.h>; minix.rs defines it alongside the",
        "errnos in kernel-shared/src/error.rs. It lives here so a C file that",
        "checks an IPC result does not have to include <minixrs/errno.h> as well.",
    ]);
    f.define_dec("OK", error::OK.into());

    f.section("processes");
    f.block_comment(&[
        "Every process below has TWO macros, and they are NOT interchangeable:",
        "",
        "    <NAME>_PROC_NR   the process-table slot (NEGATIVE for kernel tasks)",
        "    <NAME>_EP        the boot endpoint -- what belongs in an IPC call",
        "",
        "They differ for the kernel tasks because minix.rs sign-extends the",
        "endpoint proc field instead of using MINIX 3's offset bias (see",
        "minixrs/ipc.h). Naming a task by its _PROC_NR in an IPC call is a bug.",
        "",
        "The _EP values are computed by calling com::boot_endpoint() in the",
        "generator: no equivalent Rust constant exists, and deriving them here is",
        "exactly what this tool is for.",
    ]);
    for (name, nr) in processes() {
        f.define_dec(&format!("{name}_PROC_NR"), nr.get().into());
        f.define_ep(&format!("{name}_EP"), com::boot_endpoint(nr));
    }

    f.block_comment(&[
        "Sentinel proc number for an MXBI archive module that is resolvable by",
        "name but never boot-loaded (the exec target). Not a process.",
    ]);
    f.define_dec("EXEC_ONLY_PROC_NR", com::EXEC_ONLY_PROC_NR.into());

    f.block_comment(&[
        "Deliberately not emitted, each for want of a C consumer:",
        "  * STUB_[A-D]_PROC_NR / NR_STUB_PROCS -- the demo stubs are scoped to",
        "    the kernel's `boot-stubs` cargo feature.",
        "  * NR_SERVED_PROCS -- sizes the Rust servers' per-process tables.",
        "  * message::USER_VA_TOP and sys_limits::* -- kernel-internal caps.",
        "  * signal numbers -- musl's <signal.h> is authoritative and already",
        "    agrees with kernel-shared/src/signal.rs.",
    ]);

    f.section("endpoint packing self-check");
    f.block_comment(&[
        "The C decode macros in minixrs/ipc.h are hand-written; the _EP values are",
        "Rust-computed. These assertions check one against the other at compile",
        "time, in every translation unit, with no libc and no sysroot.",
    ]);
    for name in ["SYSTEM", "HARDWARE", "CLOCK", "PM", "VFS", "INIT"] {
        f.static_assert(
            &format!("{name}_EP == _ENDPOINT(0, {name}_PROC_NR)"),
            &format!("{name}_EP disagrees with the _ENDPOINT packing macro"),
        );
        f.static_assert(
            &format!("_ENDPOINT_P({name}_EP) == {name}_PROC_NR"),
            &format!("_ENDPOINT_P does not round-trip {name}_EP"),
        );
        f.static_assert(
            &format!("_ENDPOINT_G({name}_EP) == 0"),
            &format!("{name}_EP is not a generation-0 boot endpoint"),
        );
    }

    f.guard_close(GUARD);
    f.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{builder, ipc_h};

    /// Completeness, not just correctness: the array's *length* is locked by its
    /// return type, and this pins what has to be in it — the `NR_TASKS` negative
    /// task slots followed by boot slots `0..NR_BOOT_PROCS`, contiguously. A new
    /// boot server that gets a Rust endpoint and an MXBI entry but no row here
    /// fails one of the two rather than quietly missing its C macros.
    #[test]
    fn the_process_list_covers_every_task_and_boot_slot() {
        let procs = processes();
        assert_eq!(procs.len(), com::NR_TASKS + com::NR_BOOT_PROCS);
        for (i, (name, nr)) in procs.into_iter().enumerate() {
            assert_eq!(
                nr.get(),
                i as i32 - com::NR_TASKS as i32,
                "{name} is out of slot order"
            );
        }
        // Restated at the ends so the intent survives a refactor of the loop:
        // the list spans exactly [-NR_TASKS, NR_BOOT_PROCS).
        assert_eq!(procs[0].1.get(), -(com::NR_TASKS as i32));
        assert_eq!(
            procs[procs.len() - 1].1.get(),
            com::NR_BOOT_PROCS as i32 - 1
        );
    }

    #[test]
    fn every_process_has_both_a_proc_nr_and_an_ep() {
        let text = render();
        for (name, _) in processes() {
            assert!(
                text.contains(&format!("#define {name}_PROC_NR")),
                "{name}_PROC_NR missing"
            );
            assert!(
                text.contains(&format!("#define {name}_EP")),
                "{name}_EP missing"
            );
        }
    }

    #[test]
    fn endpoints_match_boot_endpoint() {
        let text = render();
        for (name, nr) in processes() {
            let ep = com::boot_endpoint(nr);
            assert_eq!(
                builder::define_value(&text, &format!("{name}_EP")).as_deref(),
                Some(format!("((endpoint_t) {ep})").as_str()),
                "{name}_EP is not boot_endpoint({nr})"
            );
            assert_eq!(
                builder::define_value(&text, &format!("{name}_PROC_NR")).as_deref(),
                Some(fmt_dec(nr.get()).as_str()),
                "{name}_PROC_NR is not the table slot"
            );
        }
    }

    /// Mirror of `CFile::define_dec`'s negative-parenthesizing rule.
    fn fmt_dec(v: i32) -> String {
        if v < 0 {
            format!("({v})")
        } else {
            v.to_string()
        }
    }

    /// The whole point of the two-macro split: for kernel tasks the endpoint is
    /// not the proc number, so emitting only one of them would be a footgun.
    #[test]
    fn task_endpoints_differ_from_their_proc_numbers() {
        for name in ["ASYNCM", "IDLE", "CLOCK", "SYSTEM", "HARDWARE"] {
            let (_, nr) = processes().into_iter().find(|p| p.0 == name).unwrap();
            assert_ne!(com::boot_endpoint(nr), nr.get(), "{name}");
        }
        // ...while server slots do coincide, which is why the distinction is
        // easy to miss without the comment the header carries.
        let (_, pm) = processes().into_iter().find(|p| p.0 == "PM").unwrap();
        assert_eq!(com::boot_endpoint(pm), pm.get());
    }

    #[test]
    fn negative_proc_numbers_are_parenthesized() {
        let text = render();
        assert!(text.contains(&format!("({})", com::SYSTEM.get())));
        assert!(text.contains(&format!("({})", com::EXEC_ONLY_PROC_NR)));
    }

    #[test]
    fn includes_ipc_h_for_the_packing_macros() {
        assert!(render().contains("#include <minixrs/ipc.h>"));
        // The guard name the include resolves to, kept in sync by construction.
        assert_eq!(ipc_h::GUARD, "_MINIXRS_IPC_H");
    }

    #[test]
    fn capacities_render_from_live_constants() {
        let text = render();
        for (name, want) in [
            ("NR_TASKS", com::NR_TASKS),
            ("NR_PROCS", com::NR_PROCS),
            ("NR_SYS_PROCS", com::NR_SYS_PROCS),
            ("NR_BOOT_PROCS", com::NR_BOOT_PROCS),
        ] {
            assert_eq!(
                builder::define_value(&text, name).as_deref(),
                Some(want.to_string().as_str())
            );
        }
        assert_eq!(
            builder::define_value(&text, "OK").as_deref(),
            Some(error::OK.to_string().as_str())
        );
    }
}
