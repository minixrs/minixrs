// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! `minix/ipc.h` — the message struct, endpoint packing, and IPC primitives.

use minixrs_kernel_shared::{Message, endpoint, ipc_const};

use crate::builder::CFile;

/// Include guard for the generated header.
pub const GUARD: &str = "_MINIX_IPC_H";

/// Layout of [`Message`], measured rather than assumed.
///
/// Every number the header pins comes from here, so the generator can never
/// hardcode a stale 104/8/8 and a Rust-side layout change surfaces in the C.
struct MessageLayout {
    size: usize,
    align: usize,
    m_source_off: usize,
    m_type_off: usize,
    payload_off: usize,
    payload_len: usize,
}

impl MessageLayout {
    fn measure() -> Self {
        let msg = Message {
            m_source: 0,
            m_type: 0,
            payload: [0; 96],
        };
        let base = (&raw const msg) as usize;
        Self {
            size: size_of::<Message>(),
            align: align_of::<Message>(),
            m_source_off: (&raw const msg.m_source) as usize - base,
            m_type_off: (&raw const msg.m_type) as usize - base,
            payload_off: (&raw const msg.payload) as usize - base,
            payload_len: msg.payload.len(),
        }
    }
}

/// Render `minix/ipc.h`.
pub fn render() -> String {
    let l = MessageLayout::measure();
    let mut f = CFile::new(
        "IPC message format, endpoint packing, and the IPC primitive numbers.",
        &[
            "kernel-shared/src/message.rs",
            "kernel-shared/src/endpoint.rs",
            "kernel-shared/src/ipc_const.rs",
        ],
    );
    f.guard_open(GUARD);

    f.block_comment(&[
        "This header deliberately includes nothing in the common case: it",
        "describes a kernel ABI and must be usable from freestanding C as well as",
        "from musl user space. The one thing it needs is offsetof, which comes",
        "from a compiler builtin under a private name -- both to avoid depending",
        "on <stddef.h> and to avoid ever colliding with the C library's own",
        "offsetof macro.",
        "",
        "The fallback exists for a hypothetical non-GNU-flavoured compiler; the",
        "supported toolchain is clang (phase-5 decision D10).",
    ]);
    f.line("#if defined(__GNUC__) || defined(__clang__)");
    f.define_raw("_MINIX_OFFSETOF(t, m)", "__builtin_offsetof(t, m)");
    f.line("#else");
    f.include("stddef.h", "offsetof");
    f.define_raw("_MINIX_OFFSETOF(t, m)", "offsetof(t, m)");
    f.line("#endif");

    f.section("endpoints");
    f.line("typedef int endpoint_t;");
    f.static_assert(
        "sizeof(int) == 4",
        "the minix.rs ABI assumes a 32-bit int for endpoint_t",
    );

    f.blank();
    f.define_dec("ENDPOINT_GEN_SHIFT", endpoint::ENDPOINT_GEN_SHIFT.into());
    f.define_raw(
        "ENDPOINT_PROC_MASK",
        &format!("0x{:x}", endpoint::ENDPOINT_PROC_MASK),
    );
    f.define_dec(
        "ENDPOINT_MAX_GENERATION",
        endpoint::ENDPOINT_MAX_GENERATION.into(),
    );
    f.define_dec(
        "ENDPOINT_SLOT_TOP",
        endpoint::ENDPOINT_SLOT_TOP.get().into(),
    );

    f.blank();
    f.define_ep("ANY", endpoint::ANY);
    f.define_ep("NONE", endpoint::NONE);
    f.define_ep("SELF", endpoint::SELF);

    f.block_comment(&[
        "Endpoint packing. minix.rs deviates from MINIX 3 deliberately: the proc",
        "field is SIGN-EXTENDED from bit ENDPOINT_GEN_SHIFT-1 to recover negative",
        "kernel-task slots, rather than MINIX 3's MAX_NR_TASKS offset bias. These",
        "mirror endpoint.rs::{make_endpoint, endpoint_proc, endpoint_gen}.",
        "",
        "The consequence is that _ENDPOINT(0, p) != p for negative p, which is why",
        "minix/com.h gives every process both a _PROC_NR and an _EP macro.",
    ]);
    f.line("#define _ENDPOINT(g, p)  (((g) << ENDPOINT_GEN_SHIFT) | ((p) & ENDPOINT_PROC_MASK))");
    f.line("#define _ENDPOINT_G(e)   ((e) >> ENDPOINT_GEN_SHIFT)");
    f.line("#define _ENDPOINT_P(e)                                                   \\");
    f.line("    ((((e) & ENDPOINT_PROC_MASK) & (1 << (ENDPOINT_GEN_SHIFT - 1)))      \\");
    f.line("        ? (((e) & ENDPOINT_PROC_MASK) | ~ENDPOINT_PROC_MASK)             \\");
    f.line("        : ((e) & ENDPOINT_PROC_MASK))");

    f.section("message");
    f.block_comment(&[
        "The IPC message. Laid out to match kernel-shared's #[repr(C, align(8))]",
        "Message exactly; the assertions below are generated from the live Rust",
        "type, so a layout change on either side fails to compile rather than",
        "silently corrupting every IPC call.",
        "",
        "C11 forbids an alignment specifier on a struct type declaration, so the",
        "_Alignas rides the first member -- a struct's alignment is the maximum of",
        "its members', which yields the required 8.",
    ]);
    f.line("struct message {");
    f.line("    _Alignas(8) endpoint_t m_source;   /* set by the kernel on delivery */");
    f.line("    int                    m_type;     /* call number out, result back  */");
    f.line(&format!(
        "    unsigned char          payload[{}];",
        l.payload_len
    ));
    f.line("};");
    f.line("typedef struct message message;");

    f.blank();
    f.static_assert(
        &format!("sizeof(message) == {}", l.size),
        "message size drifted from kernel-shared Message",
    );
    f.static_assert(
        &format!("_Alignof(message) == {}", l.align),
        "message alignment drifted from kernel-shared Message",
    );
    f.static_assert(
        &format!("_MINIX_OFFSETOF(message, m_source) == {}", l.m_source_off),
        "m_source offset drifted from kernel-shared Message",
    );
    f.static_assert(
        &format!("_MINIX_OFFSETOF(message, m_type) == {}", l.m_type_off),
        "m_type offset drifted from kernel-shared Message",
    );
    f.static_assert(
        &format!("_MINIX_OFFSETOF(message, payload) == {}", l.payload_off),
        "payload offset drifted from kernel-shared Message",
    );

    f.section("IPC primitives");
    f.block_comment(&[
        "Primitive numbers for the IPC trap. The register that carries them is",
        "part of the per-architecture trap ABI -- see kernel-shared/src/ipc_const.rs",
        "and the minix-ipc crate, which are authoritative.",
    ]);
    f.define_dec("SEND", ipc_const::SEND.into());
    f.define_dec("RECEIVE", ipc_const::RECEIVE.into());
    f.define_dec("SENDREC", ipc_const::SENDREC.into());
    f.define_dec("NOTIFY", ipc_const::NOTIFY.into());
    f.define_dec("SENDNB", ipc_const::SENDNB.into());
    f.define_dec("SENDA", ipc_const::SENDA.into());
    f.define_dec("MINIX_KERNINFO", ipc_const::MINIX_KERNINFO.into());
    f.define_hex("NOTIFY_MESSAGE", ipc_const::NOTIFY_MESSAGE.into());

    f.guard_close(GUARD);
    f.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The D8-mandated layout snapshot: the generated text must agree with
    /// `Message`'s own const assertions, and the generator must derive the
    /// numbers rather than typing them in.
    #[test]
    fn pins_the_live_message_layout() {
        let text = render();
        let l = MessageLayout::measure();

        assert!(text.contains(&format!("sizeof(message) == {}", size_of::<Message>())));
        assert!(text.contains(&format!("_Alignof(message) == {}", align_of::<Message>())));
        assert!(text.contains(&format!(
            "_MINIX_OFFSETOF(message, payload) == {}",
            l.payload_off
        )));
        assert!(text.contains(&format!(
            "unsigned char          payload[{}];",
            l.payload_len
        )));

        // The ABI numbers themselves, pinned so a layout change is a deliberate
        // edit in two places (here and message.rs's const asserts).
        assert_eq!(l.size, 104);
        assert_eq!(l.align, 8);
        assert_eq!(l.m_source_off, 0);
        assert_eq!(l.m_type_off, 4);
        assert_eq!(l.payload_off, 8);
        assert_eq!(l.payload_len, 96);
    }

    /// The ABI header must be includable from freestanding C. Apple's clang
    /// redirects `<stddef.h>` to the system header for any `*-musl` triple, so
    /// an unconditional include also breaks a hermetic, sysroot-less check.
    #[test]
    fn needs_no_header_under_a_gnu_flavoured_compiler() {
        let text = render();
        let gnu_arm = text
            .split("#if defined(__GNUC__) || defined(__clang__)")
            .nth(1)
            .and_then(|s| s.split("#else").next())
            .expect("the offsetof fallback block");
        assert!(!gnu_arm.contains("#include"));
        assert!(gnu_arm.contains("__builtin_offsetof"));
        assert!(text.contains("_MINIX_OFFSETOF(message, payload)"));
    }

    #[test]
    fn alignas_rides_the_first_member() {
        // C11 rejects an alignment specifier on the struct type declaration.
        let text = render();
        assert!(text.contains("_Alignas(8) endpoint_t m_source;"));
        assert!(!text.contains("_Alignas(8) struct"));
    }

    #[test]
    fn ipc_primitives_render_from_live_constants() {
        let text = render();
        for (name, value) in [
            ("SENDREC", ipc_const::SENDREC),
            ("NOTIFY", ipc_const::NOTIFY),
            ("SENDNB", ipc_const::SENDNB),
        ] {
            assert!(text.contains(&format!("#define {name:<24} {value}")));
        }
        assert!(text.contains(&format!("0x{:X}", ipc_const::NOTIFY_MESSAGE)));
    }

    #[test]
    fn endpoint_sentinels_render_from_live_constants() {
        let text = render();
        assert!(text.contains(&format!(
            "#define ANY                      ((endpoint_t) {})",
            endpoint::ANY
        )));
        assert!(text.contains(&format!(
            "#define NONE                     ((endpoint_t) {})",
            endpoint::NONE
        )));
        assert!(text.contains(&format!(
            "#define SELF                     ((endpoint_t) {})",
            endpoint::SELF
        )));
    }

    /// The header must not restate the trap register: `ipc_const.rs`'s own
    /// comment on that point is wrong (it says `x16`; the kernel reads `x1`),
    /// and correcting it belongs to slice 5.1.
    #[test]
    fn makes_no_trap_register_claim() {
        let text = render();
        assert!(!text.contains("x16"));
        assert!(!text.contains("x1 "));
    }
}
