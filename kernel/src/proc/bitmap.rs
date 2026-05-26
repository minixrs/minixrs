//! `Priv`-table bitmap helpers.
//!
//! `Priv` carries several `[u32; N]` bitmaps in two flavors:
//!
//! - **Priv-indexed** (`ipc_to`, `notify_pending`, `asyn_pending`) — bit
//!   `n` corresponds to `PrivId(n)`. Tested/set whenever the kernel needs
//!   to ask "is caller permitted to talk to this sysproc?" or "does this
//!   sysproc have a deferred notification from that one?".
//! - **Call-indexed** (`k_call_mask`) — bit `n` corresponds to kernel call
//!   slot `n`, i.e. `SYS_* - KERNEL_CALL`. Tested in the SYSTEM dispatch
//!   path to gate which `SYS_*` numbers the caller is allowed to invoke.
//!
//! The two flavors are typed differently (`PrivId` vs. `usize`) so the
//! compiler refuses to mix them.
//!
//! All helpers silently no-op (set) / return `false` (get) on out-of-range
//! indices. The kernel never panics on a bitmap query — matches the
//! slice-2.5 hardening contract (`set_sys_bit` was tightened in PR #6 so
//! a stale or bogus id can't take the kernel down).

use minix4_kernel_shared::PrivId;

/// Set bit `id` in a sysproc-indexed bitmap (the same encoding MINIX 3
/// uses for `s_ipc_to`, `s_notify_pending`, `s_asyn_pending`).
#[inline]
pub(crate) fn set_sys_bit(map: &mut [u32], id: PrivId) {
    set_bit(map, id.as_usize());
}

/// Test bit `id` in a sysproc-indexed bitmap.
#[inline]
pub(crate) fn get_sys_bit(map: &[u32], id: PrivId) -> bool {
    get_bit(map, id.as_usize())
}

/// Set bit `call_idx` in a call-indexed bitmap (`k_call_mask`).
#[inline]
pub(crate) fn set_call_bit(map: &mut [u32], call_idx: usize) {
    set_bit(map, call_idx);
}

/// Test bit `call_idx` in a call-indexed bitmap (`k_call_mask`).
#[inline]
pub(crate) fn get_call_bit(map: &[u32], call_idx: usize) -> bool {
    get_bit(map, call_idx)
}

#[inline]
fn set_bit(map: &mut [u32], n: usize) {
    if n / 32 >= map.len() {
        return;
    }
    map[n / 32] |= 1 << (n % 32);
}

#[inline]
fn get_bit(map: &[u32], n: usize) -> bool {
    if n / 32 >= map.len() {
        return false;
    }
    map[n / 32] & (1 << (n % 32)) != 0
}
