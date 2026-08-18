// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `minixrs/callnr.h` — kernel-call numbers and the server request bands.

use minixrs_kernel_shared::{callnr, grant};

use crate::builder::CFile;

/// Include guard for the generated header.
pub const GUARD: &str = "_MINIXRS_CALLNR_H";

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

/// The bands, in ascending numeric order — `bands_are_in_ascending_numeric_order`
/// enforces that, so inserting a new band in the wrong place fails a test rather
/// than rendering a header whose sections disagree with its ordering asserts.
fn bands() -> [Band; 9] {
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
            title: "VFS requests",
            base_name: "VFS_RQ_BASE",
            base: callnr::VFS_RQ_BASE,
            count: Some(("NR_VFS_MSGS", callnr::NR_VFS_MSGS)),
            members: vec![
                ("VFS_WRITE", callnr::VFS_WRITE),
                ("VFS_OPEN", callnr::VFS_OPEN),
                ("VFS_READ", callnr::VFS_READ),
                ("VFS_CLOSE", callnr::VFS_CLOSE),
                ("VFS_EXEC_STAGE", callnr::VFS_EXEC_STAGE),
            ],
        },
        // Base + members + count only, like BDEV and CDEV below: no payload
        // offsets. No C builds an FS request — a C program calls `open`/`read`,
        // which musl turns into a VFS request — so the 5.4 "defer the offsets
        // until C needs them" stance holds here too. Emitting the numbers anyway
        // keeps the band-ordering `_Static_assert`s below able to name it.
        Band {
            title: "file-system requests",
            base_name: "FS_RQ_BASE",
            base: callnr::FS_RQ_BASE,
            count: Some(("NR_FS_MSGS", callnr::NR_FS_MSGS)),
            members: vec![
                ("FS_READSUPER", callnr::FS_READSUPER),
                ("FS_LOOKUP", callnr::FS_LOOKUP),
                ("FS_READ", callnr::FS_READ),
                ("FS_WRITE", callnr::FS_WRITE),
            ],
        },
        // Base + members + count only, mirroring exactly what CDEV emits: no
        // payload offsets. No C builds a BDEV request — musl's write() goes to
        // VFS — so the 5.4 "defer the offsets until C needs them" stance holds.
        Band {
            title: "block-device requests",
            base_name: "BDEV_RQ_BASE",
            base: callnr::BDEV_RQ_BASE,
            count: Some(("NR_BDEV_MSGS", callnr::NR_BDEV_MSGS)),
            members: vec![
                ("BDEV_READ", callnr::BDEV_READ),
                ("BDEV_WRITE", callnr::BDEV_WRITE),
            ],
        },
        Band {
            title: "character-device requests",
            base_name: "CDEV_RQ_BASE",
            base: callnr::CDEV_RQ_BASE,
            count: Some(("NR_CDEV_MSGS", callnr::NR_CDEV_MSGS)),
            members: vec![("CDEV_WRITE", callnr::CDEV_WRITE)],
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

/// Render `minixrs/callnr.h`.
pub fn render() -> String {
    let mut f = CFile::new(
        "Kernel-call numbers, server request bands, and their payload constants.",
        &["kernel-shared/src/callnr.rs"],
    );
    f.guard_open(GUARD);

    f.blank();
    f.include(
        "minixrs/ipc.h",
        "NOTIFY_MESSAGE, for the band ordering checks",
    );

    f.block_comment(&[
        "Deviation from MINIX 3: its <minix/callnr.h> holds POSIX syscall numbers",
        "and keeps server request numbers in <minix/com.h>. minix.rs has no POSIX",
        "call-number layer -- a libc wrapper builds a server request directly --",
        "so this header carries the kernel calls and the request bands together.",
        "",
        "Payload byte offsets ARE emitted, at the end of this header: slice 5.6's",
        "musl wrappers build VFS and PM requests by hand and need them. The grant",
        "ENTRY layout still is not -- see the grants section.",
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
        "<minixrs/safecopies.h> would need its own builder and CI syntax check. The",
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

    f.section("server-request payload offsets");
    f.block_comment(&[
        "Byte offsets within a message payload, which starts at message offset 8.",
        "Emitted since slice 5.6: src/minixrs/_syscall.c in the musl fork builds",
        "these requests by hand, so it needs the offsets as real constants rather",
        "than as Rust doc comments.",
        "",
        "VFS_WRITE carries a RAW BUFFER ADDRESS in the caller's own address space,",
        "not a grant id -- VFS's client is an ordinary user process with no grant",
        "table. VFS is what turns it into a magic grant, naming the owner from the",
        "kernel-stamped m_source and never from the payload.",
        "",
        "CDEV_WRITE, by contrast, carries a grant id and NO granter field, for the",
        "same anti-spoof reason. Its offsets are emitted for completeness of the",
        "band, not because C talks to a driver: musl's write() goes to VFS.",
    ]);
    f.define_dec("VFS_FD_OFF", callnr::VFS_FD_OFF as i64);
    f.define_dec("VFS_LEN_OFF", callnr::VFS_LEN_OFF as i64);
    f.define_dec("VFS_BUF_OFF", callnr::VFS_BUF_OFF as i64);
    f.blank();
    f.define_dec("PM_EXEC_PATH_OFF", callnr::PM_EXEC_PATH_OFF as i64);
    f.define_dec("PM_EXEC_PATH_MAX", callnr::PM_EXEC_PATH_MAX as i64);
    f.blank();
    f.define_dec("CDEV_MINOR_OFF", callnr::CDEV_MINOR_OFF as i64);
    f.define_dec("CDEV_GRANT_OFF", callnr::CDEV_GRANT_OFF as i64);
    f.define_dec("CDEV_LEN_OFF", callnr::CDEV_LEN_OFF as i64);
    f.define_dec("CDEV_OFFSET_OFF", callnr::CDEV_OFFSET_OFF as i64);
    f.define_dec("CDEV_MAX_IO", callnr::CDEV_MAX_IO as i64);
    f.define_dec("CDEV_MINOR_CONSOLE", callnr::CDEV_MINOR_CONSOLE.into());

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
        "PM_RQ_BASE + NR_PM_MSGS - 1 < VFS_RQ_BASE",
        "the PM band overlaps the VFS band",
    );
    f.static_assert(
        "VFS_RQ_BASE + NR_VFS_MSGS - 1 < FS_RQ_BASE",
        "the VFS band overlaps the FS band",
    );
    f.static_assert(
        "FS_RQ_BASE + NR_FS_MSGS - 1 < BDEV_RQ_BASE",
        "the FS band overlaps the BDEV band",
    );
    f.static_assert(
        "BDEV_RQ_BASE + NR_BDEV_MSGS - 1 < CDEV_RQ_BASE",
        "the BDEV band overlaps the CDEV band",
    );
    f.static_assert(
        "CDEV_RQ_BASE + NR_CDEV_MSGS - 1 < VM_RQ_BASE",
        "the CDEV band overlaps the VM band",
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

    /// The header renders one section per band in `bands()` order, and the
    /// ordering `_Static_assert`s below them chain each band to the previous one.
    /// A band inserted in the wrong slot would therefore render sections that
    /// disagree with the asserts — so pin the order here rather than in review.
    /// Slice 5.4 inserted VFS (`0x800`) between PM and CDEV on exactly this
    /// instruction, slice 5.7 inserted BDEV (`0xA00`) between VFS and CDEV on the
    /// same one, and slice 5.8 inserted the VFS↔FS band (`0x900`) between VFS and
    /// BDEV. That fills `0x700..0xC00`: a tenth band has no reserved slot left to
    /// take, so it must find a home outside this span — see
    /// `the_server_band_space_below_vm_is_fully_allocated` in `callnr.rs`.
    #[test]
    fn bands_are_in_ascending_numeric_order() {
        let bands = bands();
        for pair in bands.windows(2) {
            let (lo, hi) = (&pair[0], &pair[1]);
            let lo_last = lo.members.last().map(|(_, v)| *v).unwrap_or(lo.base);
            assert!(
                lo_last < hi.base,
                "the {} band (ends {lo_last:#x}) is not below the {} band ({:#x})",
                lo.title,
                hi.title,
                hi.base,
            );
        }
    }

    /// The payload offsets, emitted as of slice 5.6 — the deliberate act the
    /// preceding slices' negative assertion was holding the door on.
    ///
    /// `src/minixrs/_syscall.c` in the musl fork marshals `VFS_WRITE` by hand, so
    /// `VFS_FD_OFF` / `VFS_LEN_OFF` / `VFS_BUF_OFF` are load-bearing C now. Each
    /// value is checked against the live Rust constant rather than a literal, so
    /// this stays a wiring check and never becomes a second source of truth.
    #[test]
    fn payload_offsets_match_the_rust_constants() {
        let text = render();
        for request in ["CDEV_WRITE", "VFS_WRITE"] {
            assert!(
                builder::define_value(&text, request).is_some(),
                "{request} itself must be emitted"
            );
        }
        let offsets: [(&str, i64); 11] = [
            ("VFS_FD_OFF", callnr::VFS_FD_OFF as i64),
            ("VFS_LEN_OFF", callnr::VFS_LEN_OFF as i64),
            ("VFS_BUF_OFF", callnr::VFS_BUF_OFF as i64),
            ("PM_EXEC_PATH_OFF", callnr::PM_EXEC_PATH_OFF as i64),
            ("PM_EXEC_PATH_MAX", callnr::PM_EXEC_PATH_MAX as i64),
            ("CDEV_MINOR_OFF", callnr::CDEV_MINOR_OFF as i64),
            ("CDEV_GRANT_OFF", callnr::CDEV_GRANT_OFF as i64),
            ("CDEV_LEN_OFF", callnr::CDEV_LEN_OFF as i64),
            ("CDEV_OFFSET_OFF", callnr::CDEV_OFFSET_OFF as i64),
            ("CDEV_MAX_IO", callnr::CDEV_MAX_IO as i64),
            ("CDEV_MINOR_CONSOLE", callnr::CDEV_MINOR_CONSOLE as i64),
        ];
        for (name, want) in offsets {
            assert_eq!(
                builder::define_value(&text, name).as_deref(),
                Some(want.to_string().as_str()),
                "{name} disagrees with the Rust constant"
            );
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

    /// `GET_RAMDISK` and its reply offsets are deliberately **not** emitted, even
    /// though `GET_WHOAMI` beside them is.
    ///
    /// The asymmetry is the point. `GET_WHOAMI` describes any process; a C program
    /// linked against the fork could legitimately ask it. `GET_RAMDISK` is gated by
    /// the kernel to `MEM_PROC_NR`, a Rust driver — so no C can ever call it
    /// successfully, and past the slice-5.6 ABI freeze a header constant is a
    /// promise that costs a two-repo PR to retract. Emitting it for symmetry would
    /// buy nothing and freeze something.
    ///
    /// This test is the record of that decision, so the omission reads as
    /// deliberate rather than forgotten — the `no_grant_struct_is_emitted_yet`
    /// pattern. Emit it when a C caller exists, and delete this test in the same
    /// commit.
    #[test]
    fn no_ramdisk_selector_is_emitted_yet() {
        let text = render();
        assert!(
            builder::define_value(&text, "GET_WHOAMI").is_some(),
            "GET_WHOAMI is emitted; only GET_RAMDISK is held back"
        );
        for name in [
            "GET_RAMDISK",
            "GETINFO_RAMDISK_VA_OFF",
            "GETINFO_RAMDISK_LEN_OFF",
        ] {
            assert_eq!(
                builder::define_value(&text, name),
                None,
                "{name} is a kernel-to-Rust-driver selector and must not be frozen into the C ABI"
            );
        }
    }

    /// `GrantEntry` is deliberately not emitted as a C struct until a C caller
    /// needs it (slice 5.6). Nothing may quietly add one here — that belongs in
    /// its own `<minixrs/safecopies.h>` with its own CI syntax check.
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
