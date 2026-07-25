// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `minix/callnr.h` — kernel-call numbers and the server request bands.

use minixrs_kernel_shared::{callnr, grant};

use crate::builder::CFile;

/// Include guard for the generated header.
pub const GUARD: &str = "_MINIX_CALLNR_H";

/// The 18 kernel calls, in numeric order.
fn kernel_calls() -> [(&'static str, i32); 18] {
    [
        ("SYS_GETINFO", callnr::SYS_GETINFO),
        ("SYS_PRIVCTL", callnr::SYS_PRIVCTL),
        ("SYS_FORK", callnr::SYS_FORK),
        ("SYS_EXEC", callnr::SYS_EXEC),
        ("SYS_EXIT", callnr::SYS_EXIT),
        ("SYS_COPY", callnr::SYS_COPY),
        ("SYS_SAFECOPY", callnr::SYS_SAFECOPY),
        ("SYS_IRQCTL", callnr::SYS_IRQCTL),
        ("SYS_VMCTL", callnr::SYS_VMCTL),
        ("SYS_SCHEDULE", callnr::SYS_SCHEDULE),
        ("SYS_SETALARM", callnr::SYS_SETALARM),
        ("SYS_TIMES", callnr::SYS_TIMES),
        ("SYS_DIAGCTL", callnr::SYS_DIAGCTL),
        ("SYS_SETGRANT", callnr::SYS_SETGRANT),
        ("SYS_SCHEDCTL", callnr::SYS_SCHEDCTL),
        ("SYS_KILL", callnr::SYS_KILL),
        ("SYS_GETKSIG", callnr::SYS_GETKSIG),
        ("SYS_ENDKSIG", callnr::SYS_ENDKSIG),
    ]
}

/// The CPF grant flags, in numeric order.
fn cpf_flags() -> [(&'static str, u32); 8] {
    [
        ("CPF_READ", grant::CPF_READ),
        ("CPF_WRITE", grant::CPF_WRITE),
        ("CPF_TRY", grant::CPF_TRY),
        ("CPF_USED", grant::CPF_USED),
        ("CPF_DIRECT", grant::CPF_DIRECT),
        ("CPF_INDIRECT", grant::CPF_INDIRECT),
        ("CPF_MAGIC", grant::CPF_MAGIC),
        ("CPF_VALID", grant::CPF_VALID),
    ]
}

/// An arbitrary `(idx, seq)` pair and its packed id, used by the header's
/// round-trip `_Static_assert`s. Both fields are non-zero and unequal, so a
/// macro that dropped or swapped one would fail the check.
const GRANT_PROBE_IDX: u32 = 1234;
const GRANT_PROBE_SEQ: u32 = 56;
const GRANT_PROBE_ID: i32 = grant::grant_id(GRANT_PROBE_IDX, GRANT_PROBE_SEQ);

/// A server request band: base constant, its members, and the count constant
/// that bounds it (`None` for VM, which has no `NR_*` on the Rust side).
///
/// `members` is checked against `count` by
/// `every_band_member_list_matches_its_count`: a new request that bumps the Rust
/// `NR_*` without gaining a row here would otherwise render a larger count and
/// no macro for the request itself.
struct Band {
    title: &'static str,
    base_name: &'static str,
    base: i32,
    count: Option<(&'static str, usize)>,
    members: Vec<(&'static str, i32)>,
}

fn bands() -> [Band; 5] {
    [
        Band {
            title: "PM requests",
            base_name: "PM_RQ_BASE",
            base: callnr::PM_RQ_BASE,
            count: Some(("NR_PM_MSGS", callnr::NR_PM_MSGS)),
            members: vec![
                ("PM_GETPID", callnr::PM_GETPID),
                ("PM_FORK", callnr::PM_FORK),
                ("PM_EXIT", callnr::PM_EXIT),
                ("PM_WAIT", callnr::PM_WAIT),
                ("PM_EXEC", callnr::PM_EXEC),
                ("PM_GRANT_TEST", callnr::PM_GRANT_TEST),
            ],
        },
        Band {
            title: "VM requests",
            base_name: "VM_RQ_BASE",
            base: callnr::VM_RQ_BASE,
            count: None,
            members: vec![
                ("VM_PAGEFAULT", callnr::VM_PAGEFAULT),
                ("VM_BRK", callnr::VM_BRK),
                ("VM_MMAP", callnr::VM_MMAP),
                ("VM_MUNMAP", callnr::VM_MUNMAP),
                ("VM_FORK", callnr::VM_FORK),
            ],
        },
        Band {
            title: "SEF control messages",
            base_name: "SEF_RQ_BASE",
            base: callnr::SEF_RQ_BASE,
            count: Some(("NR_SEF_MSGS", callnr::NR_SEF_MSGS)),
            members: vec![
                ("SEF_INIT", callnr::SEF_INIT),
                ("SEF_SIGNAL", callnr::SEF_SIGNAL),
            ],
        },
        Band {
            title: "DS requests",
            base_name: "DS_RQ_BASE",
            base: callnr::DS_RQ_BASE,
            count: Some(("NR_DS_REQUESTS", callnr::NR_DS_REQUESTS)),
            members: vec![
                ("DS_PUBLISH", callnr::DS_PUBLISH),
                ("DS_RETRIEVE", callnr::DS_RETRIEVE),
                ("DS_CHECK", callnr::DS_CHECK),
            ],
        },
        Band {
            title: "SCHED requests",
            base_name: "SCHED_RQ_BASE",
            base: callnr::SCHED_RQ_BASE,
            count: Some(("NR_SCHED_MSGS", callnr::NR_SCHED_MSGS)),
            members: vec![
                ("SCHEDULING_NO_QUANTUM", callnr::SCHEDULING_NO_QUANTUM),
                ("SCHEDULING_START", callnr::SCHEDULING_START),
                ("SCHEDULING_STOP", callnr::SCHEDULING_STOP),
                ("SCHEDULING_SET_NICE", callnr::SCHEDULING_SET_NICE),
            ],
        },
    ]
}

/// Render `minix/callnr.h`.
pub fn render() -> String {
    let mut f = CFile::new(
        "Kernel-call numbers, server request bands, and their payload constants.",
        &["kernel-shared/src/callnr.rs"],
    );
    f.guard_open(GUARD);

    f.blank();
    f.include(
        "minix/ipc.h",
        "NOTIFY_MESSAGE, for the band ordering checks",
    );

    f.block_comment(&[
        "Deviation from MINIX 3: its <minix/callnr.h> holds POSIX syscall numbers",
        "and keeps server request numbers in <minix/com.h>. minix.rs has no POSIX",
        "call-number layer -- a libc wrapper builds a server request directly --",
        "so this header carries the kernel calls and the request bands together.",
        "",
        "Payload byte offsets are not emitted yet: on the Rust side they live in",
        "doc comments rather than constants. The first musl wrapper (slice 5.6) is",
        "the forcing function for promoting them to real constants.",
    ]);

    f.section("kernel calls");
    f.define_hex("KERNEL_CALL", callnr::KERNEL_CALL.into());
    f.blank();
    for (name, value) in kernel_calls() {
        f.define_hex(name, value.into());
    }
    f.blank();
    // Name-matched with the Rust constant as of slice 5.1 (it was
    // `NR_KERN_CALLS_PHASE4` there, and carried a provenance comment here to
    // bridge the gap). Keep the two spellings identical past the 5.6 ABI
    // freeze — `nr_kern_calls_is_not_phase_suffixed` guards it.
    f.define_dec("NR_KERN_CALLS", callnr::NR_KERN_CALLS as i64);
    f.define_dec("NR_SYS_CALLS", callnr::NR_SYS_CALLS as i64);

    f.section("kernel-call payload constants");
    f.define_dec("GET_WHOAMI", callnr::GET_WHOAMI.into());
    f.define_dec("SYS_GETINFO_NAME_LEN", callnr::SYS_GETINFO_NAME_LEN as i64);
    f.define_dec("EXEC_NAME_LEN", callnr::EXEC_NAME_LEN as i64);
    f.define_dec("PRIVCTL_SET_USER", callnr::PRIVCTL_SET_USER.into());
    f.define_dec("SCHEDCTL_FLAG_KERNEL", callnr::SCHEDCTL_FLAG_KERNEL.into());

    f.section("SYS_DIAGCTL subcodes + inline-text payload");
    f.define_dec("DIAGCTL_CODE_DIAG", callnr::DIAGCTL_CODE_DIAG.into());
    f.define_dec(
        "DIAGCTL_CODE_STACKTRACE",
        callnr::DIAGCTL_CODE_STACKTRACE.into(),
    );
    f.define_dec(
        "DIAGCTL_CODE_REGISTER",
        callnr::DIAGCTL_CODE_REGISTER.into(),
    );
    f.define_dec(
        "DIAGCTL_CODE_UNREGISTER",
        callnr::DIAGCTL_CODE_UNREGISTER.into(),
    );
    f.define_dec("DIAG_TEXT_OFF", callnr::DIAG_TEXT_OFF as i64);
    f.define_dec("DIAG_TEXT_MAX", callnr::DIAG_TEXT_MAX as i64);

    f.section("grants: SYS_SETGRANT / SYS_SAFECOPY");
    f.block_comment(&[
        "The grant ABI (slice 5.2). The grant-entry layout itself is deliberately",
        "not emitted yet: no C consumes it before the musl slice, and a dedicated",
        "<minix/safecopies.h> would need its own builder and CI syntax check. The",
        "flag values and the id packing are here because a C caller needs them to",
        "read a grant id out of a message payload.",
    ]);
    f.define_dec("SAFECOPY_FROM", callnr::SAFECOPY_FROM.into());
    f.define_dec("SAFECOPY_TO", callnr::SAFECOPY_TO.into());
    f.blank();
    for (name, value) in cpf_flags() {
        f.define_hex(name, value.into());
    }
    f.blank();
    f.define_dec("GRANT_SHIFT", grant::GRANT_SHIFT.into());
    f.define_hex("GRANT_MAX_IDX", grant::GRANT_MAX_IDX.into());
    f.define_hex("GRANT_MAX_SEQ", grant::GRANT_MAX_SEQ.into());
    f.define_dec("GRANT_INVALID", grant::GRANT_INVALID.into());
    f.blank();
    f.define_raw(
        "GRANT_ID(idx, seq)",
        "((int) ((((unsigned) (seq) & GRANT_MAX_SEQ) << GRANT_SHIFT) \\\n                                 | ((unsigned) (idx) & GRANT_MAX_IDX)))",
    );
    f.define_raw("GRANT_IDX(g)", "((unsigned) (g) & GRANT_MAX_IDX)");
    f.define_raw(
        "GRANT_SEQ(g)",
        "(((unsigned) (g) >> GRANT_SHIFT) & GRANT_MAX_SEQ)",
    );
    f.define_raw("GRANT_VALID(g)", "((g) >= 0)");
    f.blank();
    // Check the C packing against the Rust one rather than trusting that the
    // two expressions were transcribed the same way.
    f.static_assert(
        &format!(
            "GRANT_ID(GRANT_MAX_IDX, GRANT_MAX_SEQ) == {}",
            grant::grant_id(grant::GRANT_MAX_IDX, grant::GRANT_MAX_SEQ)
        ),
        "the C grant-id packing disagrees with the Rust one",
    );
    f.static_assert(
        &format!("GRANT_IDX({GRANT_PROBE_ID}) == {GRANT_PROBE_IDX}"),
        "GRANT_IDX does not invert GRANT_ID",
    );
    f.static_assert(
        &format!("GRANT_SEQ({GRANT_PROBE_ID}) == {GRANT_PROBE_SEQ}"),
        "GRANT_SEQ does not invert GRANT_ID",
    );

    f.section("SYS_VMCTL subcalls");
    f.define_dec("VMCTL_PT_MAP", callnr::VMCTL_PT_MAP.into());
    f.define_dec("VMCTL_PT_UNMAP", callnr::VMCTL_PT_UNMAP.into());
    f.define_dec(
        "VMCTL_CLEAR_PAGEFAULT",
        callnr::VMCTL_CLEAR_PAGEFAULT.into(),
    );
    f.define_dec("VMCTL_GET_PAGEFAULT", callnr::VMCTL_GET_PAGEFAULT.into());
    f.define_dec("VMCTL_VMINHIBIT_SET", callnr::VMCTL_VMINHIBIT_SET.into());
    f.define_dec(
        "VMCTL_VMINHIBIT_CLEAR",
        callnr::VMCTL_VMINHIBIT_CLEAR.into(),
    );
    f.define_dec("NR_VMCTL_SUBCALLS", callnr::NR_VMCTL_SUBCALLS as i64);
    f.blank();
    f.define_dec("VMCTL_PROT_WRITE", callnr::VMCTL_PROT_WRITE.into());
    f.define_dec("VMCTL_PROT_EXEC", callnr::VMCTL_PROT_EXEC.into());

    for band in bands() {
        f.section(band.title);
        f.define_hex(band.base_name, band.base.into());
        for (name, value) in &band.members {
            f.define_hex(name, (*value).into());
        }
        if let Some((count_name, count)) = band.count {
            f.define_dec(count_name, count as i64);
        }
    }

    f.section("band ordering");
    f.block_comment(&[
        "Mirrors the `const _: () = assert!(..)` guards in callnr.rs: every band",
        "sits above the previous one and entirely below NOTIFY_MESSAGE, so no",
        "server request can ever be mistaken for a kernel NOTIFY.",
    ]);
    f.static_assert(
        "NR_SYS_CALLS >= NR_KERN_CALLS",
        "the privilege call mask is narrower than the kernel-call set",
    );
    f.static_assert(
        "PM_RQ_BASE > KERNEL_CALL + NR_KERN_CALLS - 1",
        "the PM band overlaps the kernel calls",
    );
    f.static_assert(
        "PM_RQ_BASE + NR_PM_MSGS - 1 < VM_RQ_BASE",
        "the PM band overlaps the VM band",
    );
    f.static_assert("SEF_RQ_BASE > VM_FORK", "the SEF band overlaps the VM band");
    f.static_assert(
        "DS_RQ_BASE > SEF_RQ_BASE + NR_SEF_MSGS - 1",
        "the DS band overlaps the SEF band",
    );
    f.static_assert(
        "SCHED_RQ_BASE > DS_RQ_BASE + NR_DS_REQUESTS - 1",
        "the SCHED band overlaps the DS band",
    );
    f.static_assert(
        "SCHED_RQ_BASE + NR_SCHED_MSGS - 1 < NOTIFY_MESSAGE",
        "a server request number collides with NOTIFY_MESSAGE",
    );

    f.guard_close(GUARD);
    f.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder;

    #[test]
    fn kernel_calls_render_from_live_constants() {
        let text = render();
        for (name, value) in kernel_calls() {
            assert_eq!(
                builder::define_value(&text, name).as_deref(),
                Some(format!("0x{value:X}").as_str()),
                "{name} did not render from its Rust constant"
            );
        }
    }

    #[test]
    fn the_kernel_call_list_is_complete_and_contiguous() {
        let calls = kernel_calls();
        assert_eq!(calls.len(), callnr::NR_KERN_CALLS);
        for (i, (name, value)) in calls.into_iter().enumerate() {
            assert_eq!(
                value,
                callnr::KERNEL_CALL + i as i32,
                "{name} is out of order"
            );
        }
    }

    #[test]
    fn every_band_base_member_and_count_appears() {
        let text = render();
        for band in bands() {
            assert_eq!(
                builder::define_value(&text, band.base_name).as_deref(),
                Some(format!("0x{:X}", band.base).as_str())
            );
            for (name, value) in &band.members {
                assert_eq!(
                    builder::define_value(&text, name).as_deref(),
                    Some(format!("0x{value:X}").as_str()),
                    "{name} missing from the {} band",
                    band.title
                );
            }
            if let Some((count_name, count)) = band.count {
                assert_eq!(
                    builder::define_value(&text, count_name).as_deref(),
                    Some(count.to_string().as_str())
                );
            }
        }
    }

    /// The completeness half of the band lock: `every_band_base_member_and_count_appears`
    /// only proves that whatever is listed renders, so on its own a new request
    /// that bumped `NR_PM_MSGS` would grow the C count and the ordering asserts
    /// while silently omitting its own macro. Each band's members must therefore
    /// be exactly `count` entries, contiguous from `base`.
    #[test]
    fn every_band_member_list_matches_its_count() {
        for band in bands() {
            if let Some((count_name, count)) = band.count {
                assert_eq!(
                    band.members.len(),
                    count,
                    "the {} band lists {} members but {count_name} is {count}",
                    band.title,
                    band.members.len()
                );
            }
            for (i, (name, value)) in band.members.iter().enumerate() {
                assert_eq!(
                    *value,
                    band.base + i as i32,
                    "{name} is out of order in the {} band",
                    band.title
                );
            }
        }
    }

    /// VM is the one band with no `NR_*` count on the Rust side, so its guard
    /// names the last member instead. Pin both the guard text and the member the
    /// guard names, so a future `NR_VM_MSGS` — or a `VM_RQ_BASE + 5` request —
    /// is a deliberate change here rather than a silent gap.
    #[test]
    fn the_vm_band_has_no_count_constant() {
        let vm = bands()
            .into_iter()
            .find(|b| b.base_name == "VM_RQ_BASE")
            .unwrap();
        assert!(vm.count.is_none());
        assert_eq!(
            vm.members.last().map(|(name, value)| (*name, *value)),
            Some(("VM_FORK", callnr::VM_FORK)),
            "the SEF ordering assert names the VM band's last member"
        );
        assert!(render().contains("SEF_RQ_BASE > VM_FORK"));
    }

    #[test]
    fn grant_constants_render_from_live_constants() {
        let text = render();
        for (name, value) in cpf_flags() {
            assert_eq!(
                builder::define_value(&text, name).as_deref(),
                Some(format!("0x{value:X}").as_str()),
                "{name} did not render from its Rust constant"
            );
        }
        assert_eq!(
            builder::define_value(&text, "GRANT_SHIFT").as_deref(),
            Some(grant::GRANT_SHIFT.to_string().as_str())
        );
        assert_eq!(
            builder::define_value(&text, "GRANT_MAX_IDX").as_deref(),
            Some(format!("0x{:X}", grant::GRANT_MAX_IDX).as_str())
        );
        assert_eq!(
            builder::define_value(&text, "GRANT_MAX_SEQ").as_deref(),
            Some(format!("0x{:X}", grant::GRANT_MAX_SEQ).as_str())
        );
        // Negative values render parenthesized, so a `-1` cannot be pasted into
        // a larger expression and change its meaning.
        assert_eq!(
            builder::define_value(&text, "GRANT_INVALID").as_deref(),
            Some("(-1)")
        );
    }

    /// The C grant-id macros are a second implementation of the Rust packing, so
    /// the header carries `_Static_assert`s tying them together. Check the
    /// asserted values come from the Rust helpers rather than being transcribed.
    #[test]
    fn the_grant_id_macro_asserts_use_rust_computed_values() {
        let text = render();
        let max = grant::grant_id(grant::GRANT_MAX_IDX, grant::GRANT_MAX_SEQ);
        assert!(text.contains(&format!("GRANT_ID(GRANT_MAX_IDX, GRANT_MAX_SEQ) == {max}")));
        assert!(text.contains(&format!("GRANT_IDX({GRANT_PROBE_ID}) == {GRANT_PROBE_IDX}")));
        assert!(text.contains(&format!("GRANT_SEQ({GRANT_PROBE_ID}) == {GRANT_PROBE_SEQ}")));
        // The probe must actually exercise both fields.
        assert_eq!(grant::grant_idx(GRANT_PROBE_ID), GRANT_PROBE_IDX);
        assert_eq!(grant::grant_seq(GRANT_PROBE_ID), GRANT_PROBE_SEQ);
        assert_ne!(GRANT_PROBE_IDX, GRANT_PROBE_SEQ);
    }

    #[test]
    fn safecopy_selectors_render_from_live_constants() {
        let text = render();
        assert_eq!(
            builder::define_value(&text, "SAFECOPY_FROM").as_deref(),
            Some(callnr::SAFECOPY_FROM.to_string().as_str())
        );
        assert_eq!(
            builder::define_value(&text, "SAFECOPY_TO").as_deref(),
            Some(callnr::SAFECOPY_TO.to_string().as_str())
        );
    }

    /// `GrantEntry` is deliberately not emitted as a C struct until a C caller
    /// needs it (slice 5.6). Nothing may quietly add one here — that belongs in
    /// its own `<minix/safecopies.h>` with its own CI syntax check.
    #[test]
    fn no_grant_struct_is_emitted_yet() {
        let text = render();
        assert!(!text.contains("cp_grant_entry"));
        assert!(!text.contains("struct "));
    }

    /// The C name and the Rust name must stay identical. The Rust constant was
    /// `NR_KERN_CALLS_PHASE4` until slice 5.1, which renamed it to match this
    /// header rather than let a phase-scoped name outlive Phase 4 and cross the
    /// slice-5.6 ABI freeze. Nothing may reintroduce a phase suffix on either
    /// side.
    #[test]
    fn nr_kern_calls_is_not_phase_suffixed() {
        let text = render();
        assert_eq!(
            builder::define_value(&text, "NR_KERN_CALLS").as_deref(),
            Some(callnr::NR_KERN_CALLS.to_string().as_str())
        );
        assert!(
            !text.contains("PHASE"),
            "the ABI header must carry no phase-scoped names"
        );
    }
}
