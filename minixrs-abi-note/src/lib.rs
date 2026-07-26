// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The minixrs ELF identity brand (tooling/docs/abi-note.md).

#![no_std]

/// Emit the minixrs ELF identity note (tooling/docs/abi-note.md) into the
/// current binary. Invoke once at the crate root of every user-space binary
/// crate — a library's asm object can be dropped by archive-member
/// selection, so the note must live in the binary crate itself.
#[macro_export]
macro_rules! brand {
    () => {
        #[cfg(target_os = "minixrs")]
        ::core::arch::global_asm!(
            r#"
            .pushsection .note.minixrs.ident, "a", %note
            .p2align 2
            .long 8
            .long 8
            .long 1
            .asciz "minixrs"
            .long 1
            .long 0
            .popsection
            "#
        );
    };
}
