//! Fixed-size IPC message.
//!
//! The 104-byte layout matches MINIX 3's `sizeof(message) == 104` assertion
//! for `__x86_64__` (see `include/minix/ipc.h`). Both x86_64 and aarch64 ports
//! of MINIX 4 adopt the same size so that the message is ABI-portable.
//!
//! Typed access — MINIX 3 had a union over many `mess_*` payload variants
//! (`m1i1`, `m2l1`, etc.). MINIX 4 will eventually expose strongly-typed
//! accessor structs per call (e.g., `as_vfs_read()`), but those live in the
//! servers/userland crates so the kernel doesn't pull in every protocol
//! definition. For now, `payload` is a raw byte array that callers
//! interpret per-call.

use crate::endpoint::Endpoint;

/// 104-byte fixed-size IPC message. Layout matches MINIX 3 x86_64.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Message {
    /// Endpoint of the sender (set by the kernel on delivery).
    pub m_source: Endpoint,
    /// Call number on send; result code on reply.
    pub m_type: i32,
    /// Raw payload — interpreted per-call by typed accessors.
    pub payload: [u8; 96],
}

// Compile-time guarantee that the layout matches MINIX 3's x86_64 message.
const _: () = assert!(core::mem::size_of::<Message>() == 104);
const _: () = assert!(core::mem::align_of::<Message>() == 4);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_is_104_bytes() {
        assert_eq!(core::mem::size_of::<Message>(), 104);
    }

    #[test]
    fn message_align_is_4() {
        assert_eq!(core::mem::align_of::<Message>(), 4);
    }

    #[test]
    fn message_payload_offset_is_8() {
        let m = Message { m_source: 0, m_type: 0, payload: [0; 96] };
        let base = &m as *const _ as usize;
        let payload = m.payload.as_ptr() as usize;
        assert_eq!(payload - base, 8);
    }
}
