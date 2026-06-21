// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! The DS name→endpoint registry (slice 4.2).
//!
//! DS is the system's directory: every server publishes its own endpoint under
//! a key (its NUL-padded name) at init, so others can look each other up by name
//! instead of hard-coding boot proc numbers. The store is a flat
//! `[Entry; CAP]` — small, fixed, no allocator — wrapped in an
//! `UnsafeCell` newtype with `unsafe impl Sync`, exactly like the VM server's
//! `region` table and the kernel's `PROC_TABLE`. The single-mutator invariant
//! is the same: DS is one EL0 thread with no interrupt handlers of its own, so
//! the table is only ever touched from DS's straight-line receive loop.
//!
//! The pure `*_in` helpers operate on a borrowed array and carry the host unit
//! tests; the thin `publish`/`retrieve`/`check` wrappers reach the static and
//! are the only `unsafe` here. This keeps the logic measured under coverage
//! while the IPC glue in `main.rs` is integration-tested via QEMU.

use core::cell::UnsafeCell;

use minixrs_kernel_shared::callnr::SYS_GETINFO_NAME_LEN;
use minixrs_kernel_shared::error::{EINVAL, ENOMEM, OK};

/// Registry key length — a NUL-padded server name. Matches the kernel's
/// `GET_WHOAMI` name field so a server can publish under the name it learns at
/// startup and clients can look it up by the same bytes.
const KEY_LEN: usize = SYS_GETINFO_NAME_LEN;

/// Registry capacity. Comfortably covers every boot server (≈11) plus slack;
/// no allocator means this is a hard cap, returned as `ENOMEM` when full.
const CAP: usize = 16;

/// One name→endpoint binding. `in_use == false` marks a free slot.
#[derive(Copy, Clone)]
struct Entry {
    key: [u8; KEY_LEN],
    endpoint: i32,
    in_use: bool,
}

impl Entry {
    const EMPTY: Self = Self {
        key: [0; KEY_LEN],
        endpoint: 0,
        in_use: false,
    };
}

/// True if `key` is all-NUL (an unset/empty name). Publishing such a key is
/// rejected so a zeroed payload can never claim a slot.
fn key_is_empty(key: &[u8; KEY_LEN]) -> bool {
    key.iter().all(|&b| b == 0)
}

/// Publish `ep` under `key`. Re-publishing an existing key updates its endpoint
/// in place (idempotent, single slot). Returns `OK`, `EINVAL` (empty key), or
/// `ENOMEM` (table full).
fn publish_in(t: &mut [Entry; CAP], key: &[u8; KEY_LEN], ep: i32) -> i32 {
    if key_is_empty(key) {
        return EINVAL;
    }
    // Update an existing binding for this key.
    for e in t.iter_mut() {
        if e.in_use && &e.key == key {
            e.endpoint = ep;
            return OK;
        }
    }
    // Otherwise claim the first free slot.
    for e in t.iter_mut() {
        if !e.in_use {
            *e = Entry {
                key: *key,
                endpoint: ep,
                in_use: true,
            };
            return OK;
        }
    }
    ENOMEM
}

/// Look up the endpoint registered under `key`, or `None` if unregistered.
fn retrieve_in(t: &[Entry; CAP], key: &[u8; KEY_LEN]) -> Option<i32> {
    t.iter()
        .find(|e| e.in_use && &e.key == key)
        .map(|e| e.endpoint)
}

/// True if `key` is registered.
fn check_in(t: &[Entry; CAP], key: &[u8; KEY_LEN]) -> bool {
    t.iter().any(|e| e.in_use && &e.key == key)
}

/// `UnsafeCell`-wrapped static registry. See the module note for the
/// single-mutator invariant that makes the `Sync` impl sound.
#[repr(transparent)]
struct Registry(UnsafeCell<[Entry; CAP]>);

// SAFETY: DS is a single-threaded EL0 process with no interrupt handlers of its
// own; the table is only ever accessed from DS's straight-line receive loop, so
// there is never concurrent access.
unsafe impl Sync for Registry {}

static TABLE: Registry = Registry(UnsafeCell::new([Entry::EMPTY; CAP]));

/// Publish `ep` under `key` in the global registry. Returns `OK` / `EINVAL` /
/// `ENOMEM` (see [`publish_in`]).
pub fn publish(key: &[u8; KEY_LEN], ep: i32) -> i32 {
    // SAFETY: single-mutator invariant (module note); no other reference into
    // the table is live during DS's straight-line loop.
    let t = unsafe { &mut *TABLE.0.get() };
    publish_in(t, key, ep)
}

/// Look up the endpoint registered under `key` in the global registry.
pub fn retrieve(key: &[u8; KEY_LEN]) -> Option<i32> {
    // SAFETY: single-mutator invariant (module note); shared read, no live `&mut`.
    let t = unsafe { &*TABLE.0.get() };
    retrieve_in(t, key)
}

/// True if `key` is registered in the global registry.
pub fn check(key: &[u8; KEY_LEN]) -> bool {
    // SAFETY: single-mutator invariant (module note); shared read, no live `&mut`.
    let t = unsafe { &*TABLE.0.get() };
    check_in(t, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> [u8; KEY_LEN] {
        let mut k = [0u8; KEY_LEN];
        let b = name.as_bytes();
        k[..b.len()].copy_from_slice(b);
        k
    }

    #[test]
    fn publish_then_retrieve_and_check() {
        let mut t = [Entry::EMPTY; CAP];
        assert_eq!(publish_in(&mut t, &key("vm"), 7), OK);
        assert_eq!(retrieve_in(&t, &key("vm")), Some(7));
        assert!(check_in(&t, &key("vm")));
    }

    #[test]
    fn retrieve_and_check_absent_key() {
        let t = [Entry::EMPTY; CAP];
        assert_eq!(retrieve_in(&t, &key("vfs")), None);
        assert!(!check_in(&t, &key("vfs")));
    }

    #[test]
    fn empty_key_is_einval() {
        let mut t = [Entry::EMPTY; CAP];
        assert_eq!(publish_in(&mut t, &[0u8; KEY_LEN], 1), EINVAL);
        // Nothing was stored.
        assert!(!check_in(&t, &[0u8; KEY_LEN]));
    }

    #[test]
    fn republish_updates_endpoint_in_one_slot() {
        let mut t = [Entry::EMPTY; CAP];
        assert_eq!(publish_in(&mut t, &key("ds"), 5), OK);
        assert_eq!(publish_in(&mut t, &key("ds"), 99), OK);
        assert_eq!(retrieve_in(&t, &key("ds")), Some(99));
        // Re-publish must not consume a second slot.
        let used = t.iter().filter(|e| e.in_use).count();
        assert_eq!(used, 1);
    }

    #[test]
    fn distinct_keys_coexist() {
        let mut t = [Entry::EMPTY; CAP];
        publish_in(&mut t, &key("vm"), 7);
        publish_in(&mut t, &key("vfs"), 1);
        publish_in(&mut t, &key("ds"), 5);
        assert_eq!(retrieve_in(&t, &key("vm")), Some(7));
        assert_eq!(retrieve_in(&t, &key("vfs")), Some(1));
        assert_eq!(retrieve_in(&t, &key("ds")), Some(5));
    }

    #[test]
    fn full_table_is_enomem() {
        let mut t = [Entry::EMPTY; CAP];
        // Fill every slot with distinct keys.
        for i in 0..CAP {
            let mut k = [0u8; KEY_LEN];
            k[0] = b'a';
            k[1] = i as u8 + 1; // keep non-empty + distinct
            assert_eq!(publish_in(&mut t, &k, i as i32), OK);
        }
        // A new distinct key has nowhere to go.
        assert_eq!(publish_in(&mut t, &key("late"), 42), ENOMEM);
        // But re-publishing an existing key still works (updates in place).
        let mut existing = [0u8; KEY_LEN];
        existing[0] = b'a';
        existing[1] = 1;
        assert_eq!(publish_in(&mut t, &existing, 1234), OK);
        assert_eq!(retrieve_in(&t, &existing), Some(1234));
    }

    #[test]
    fn prefix_keys_are_distinct() {
        // "vm" and "vmm" differ only past the second byte; NUL-padding must keep
        // them distinct (no false prefix match).
        let mut t = [Entry::EMPTY; CAP];
        publish_in(&mut t, &key("vm"), 7);
        publish_in(&mut t, &key("vmm"), 8);
        assert_eq!(retrieve_in(&t, &key("vm")), Some(7));
        assert_eq!(retrieve_in(&t, &key("vmm")), Some(8));
    }
}
