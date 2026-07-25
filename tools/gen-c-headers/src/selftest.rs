// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `abi-selftest.c` — the translation unit that makes the CI gate real.
//!
//! A header is never a translation unit on its own, so its `_Static_assert`s
//! never fire and `clang -fsyntax-only` has nothing to compile. This file
//! includes every generated header (twice, to exercise the include guards) and
//! adds the cross-header invariants no single header can express.

use crate::builder::CFile;

/// The generated headers, in dependency order.
pub const HEADERS: [&str; 4] = [
    "minix/ipc.h",
    "minix/com.h",
    "minix/callnr.h",
    "minix/errno.h",
];

/// Render `abi-selftest.c`.
pub fn render() -> String {
    let mut f = CFile::new(
        "Compile-time self-check for the generated minix.rs ABI headers.",
        &["kernel-shared/src/"],
    );

    f.blank();
    for header in HEADERS {
        f.line(&format!("#include <{header}>"));
    }

    f.block_comment(&[
        "Included a second time on purpose: proves every include guard actually",
        "guards, which a single-include check cannot.",
    ]);
    for header in HEADERS {
        f.line(&format!("#include <{header}>"));
    }

    f.block_comment(&[
        "Cross-header invariants. Each of these spans two generated headers, so",
        "none of them can live in a header of its own. (The single-header",
        "assertions -- message layout, endpoint round-trips, the errno band --",
        "fire from the includes above.)",
    ]);
    f.static_assert(
        "PM_RQ_BASE > KERNEL_CALL + NR_KERN_CALLS - 1",
        "the PM request band overlaps the kernel calls",
    );
    f.static_assert(
        "SCHED_RQ_BASE + NR_SCHED_MSGS - 1 < NOTIFY_MESSAGE",
        "a server request number collides with NOTIFY_MESSAGE",
    );
    f.static_assert(
        "ANY != NONE && NONE != SELF && ANY != SELF",
        "the endpoint sentinels are not distinct",
    );
    f.static_assert(
        "SYSTEM_EP != HARDWARE_EP",
        "two kernel tasks share a boot endpoint",
    );
    f.static_assert("PM_EP != VFS_EP", "two boot servers share a boot endpoint");
    f.static_assert(
        "sizeof(message) == sizeof(struct message)",
        "the message typedef and struct disagree",
    );
    f.static_assert(
        "OK == 0",
        "OK must stay 0 -- servers check IPC success against it",
    );

    f.block_comment(&["ISO C forbids an empty translation unit."]);
    f.line("typedef int minixrs_abi_selftest_tu_;");

    f.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_every_generated_header_twice() {
        let text = render();
        for header in HEADERS {
            let needle = format!("#include <{header}>");
            assert_eq!(
                text.matches(&needle).count(),
                2,
                "{header} is not included twice"
            );
        }
    }

    #[test]
    fn is_not_an_empty_translation_unit() {
        assert!(render().contains("typedef int minixrs_abi_selftest_tu_;"));
    }

    #[test]
    fn asserts_span_more_than_one_header() {
        let text = render();
        // KERNEL_CALL comes from callnr.h, NOTIFY_MESSAGE and `message` from
        // ipc.h, SYSTEM_EP and OK from com.h.
        for macro_name in [
            "KERNEL_CALL",
            "NOTIFY_MESSAGE",
            "SYSTEM_EP",
            "sizeof(message)",
        ] {
            assert!(text.contains(macro_name), "{macro_name} unreferenced");
        }
    }
}
