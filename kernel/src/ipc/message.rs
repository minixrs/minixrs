// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! User-memory IPC message copies.
//!
//! Each `mini_send` / `mini_receive` handler accepts a user-supplied VA in
//! `frame.x[2]`. The kernel reads the outgoing [`Message`] out of that buffer
//! when the caller blocks, and writes the incoming `Message` back to that
//! buffer when the kernel resumes the caller (the `MF_DELIVERMSG` flush).
//!
//! Both directions go through [`crate::mm::uaccess`], which walks the owning
//! process's page tables and copies via the frame's HHDM alias. Two
//! consequences, per decision D5:
//!
//! - A user pointer that is *in range but unmapped* yields [`EFAULT`], not an
//!   EL1 data abort. Before slice 5.1 this was a raw `read_volatile` /
//!   `write_volatile` through the live TTBR0, and a bad pointer panicked the
//!   kernel — the same-EL vector has no fixup path.
//! - The copy does not depend on which address space is active, so the
//!   deferred flush can write a receiver's buffer before its TTBR0 is
//!   installed. (`sched::schedule_next` still installs it first, but only
//!   because `eret` needs it — the flush no longer cares.)
//!
//! [`user_va_ok`] stays in front as the cheap range/alignment pre-gate: it is
//! what rejects NULL and misaligned pointers before any table walk happens.
//!
//! This module holds no `unsafe`. Messages are staged through a plain
//! `[u8; 104]` and reassembled field-by-field, so every raw-pointer operation
//! in the user-copy path lives in exactly one place, `mm::uaccess`.

use minixrs_kernel_shared::error::EFAULT;
use minixrs_kernel_shared::message::{Message, user_va_ok};

use crate::mm::uaccess;

/// Wire size of a [`Message`]: 104 bytes, `m_source` / `m_type` / `payload`
/// at offsets 0 / 4 / 8.
const MSG_BYTES: usize = core::mem::size_of::<Message>();
const M_TYPE_OFF: usize = 4;
const PAYLOAD_OFF: usize = 8;
const PAYLOAD_BYTES: usize = MSG_BYTES - PAYLOAD_OFF;

// Tie the offsets to the actual struct layout. Without this, adding a field
// ahead of `payload` would leave `PAYLOAD_OFF = 8` reading the wrong 96 bytes
// — a *silent* miscopy, since the slice length would still be right and the
// `try_into`s below would still succeed. The `try_into().expect(...)` calls are
// unreachable by construction (constant ranges within a constant-size array);
// these asserts are what actually makes a layout change a compile error.
const _: () = assert!(M_TYPE_OFF == core::mem::offset_of!(Message, m_type));
const _: () = assert!(PAYLOAD_OFF == core::mem::offset_of!(Message, payload));
const _: () = assert!(core::mem::offset_of!(Message, m_source) == 0);
const _: () = assert!(PAYLOAD_BYTES == 96);

/// Copy a [`Message`] out of user memory at `va` in the address space rooted
/// at `ttbr0_pa`.
///
/// Returns `Err(EFAULT)` if `va` fails the range/alignment gate or any page of
/// the buffer is unmapped in that address space.
pub(crate) fn copy_msg_from_user(ttbr0_pa: u64, va: u64) -> Result<Message, i32> {
    if !user_va_ok(va, MSG_BYTES) {
        return Err(EFAULT);
    }
    let mut buf = [0u8; MSG_BYTES];
    uaccess::copy_from_user_as(ttbr0_pa, va, &mut buf)?;
    Ok(Message {
        m_source: i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]),
        m_type: i32::from_ne_bytes([
            buf[M_TYPE_OFF],
            buf[M_TYPE_OFF + 1],
            buf[M_TYPE_OFF + 2],
            buf[M_TYPE_OFF + 3],
        ]),
        payload: core::array::from_fn(|i| buf[PAYLOAD_OFF + i]),
    })
}

/// Write a [`Message`] into user memory at `va` in the address space rooted at
/// `ttbr0_pa`.
///
/// Returns `Err(EFAULT)` if `va` fails the range/alignment gate, or any page of
/// the buffer is unmapped or not EL0-writable. All-or-nothing: on error the
/// user's buffer is untouched.
pub(crate) fn copy_msg_to_user(ttbr0_pa: u64, va: u64, m: &Message) -> Result<(), i32> {
    if !user_va_ok(va, MSG_BYTES) {
        return Err(EFAULT);
    }
    let mut buf = [0u8; MSG_BYTES];
    buf[0..M_TYPE_OFF].copy_from_slice(&m.m_source.to_ne_bytes());
    buf[M_TYPE_OFF..PAYLOAD_OFF].copy_from_slice(&m.m_type.to_ne_bytes());
    buf[PAYLOAD_OFF..PAYLOAD_OFF + PAYLOAD_BYTES].copy_from_slice(&m.payload);
    uaccess::copy_to_user_as(ttbr0_pa, va, &buf)
}
