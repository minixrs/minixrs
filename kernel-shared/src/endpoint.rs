// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Endpoints and generation-aware proc-number encoding.
//!
//! An `Endpoint` is the kernel's name for a process. It encodes both the
//! process-table slot (`ProcNr`) and a generation counter (`GenNr`) so that
//! a slot reuse after a process exit produces a distinct endpoint. This
//! catches stale-endpoint bugs in user-space code.
//!
//! Mirrors MINIX 3 `include/minix/type.h` (`typedef int endpoint_t`) and
//! `include/minix/endpoint.h` (the `_ENDPOINT(g, n)` / `_ENDPOINT_P(e)` /
//! `_ENDPOINT_G(e)` macros), with one deliberate ABI deviation: minix.rs
//! uses sign-extension of the low `ENDPOINT_GEN_SHIFT` bits to recover
//! negative task slots, rather than MINIX 3's `MAX_NR_TASKS` offset-bias
//! trick. The consequence is that `make_endpoint(0, p) != p` for negative
//! `p` — use `boot_endpoint(p)` explicitly when constructing an endpoint
//! from a kernel-task `ProcNr` constant like `SYSTEM`.

use core::fmt;

/// Kernel-assigned identifier for a process; encodes generation + proc_nr.
///
/// Stays an `i32` alias because it is wire-format: passed through registers
/// on the IPC trap and stored in `Message::m_source`. The strongly-typed
/// process and privilege indices live in [`ProcNr`] and [`PrivId`].
pub type Endpoint = i32;

/// Generation counter — bumped when a process slot is reused.
pub type GenNr = i32;

/// Index into the process table. Task slots are negative (`SYSTEM = -2`, …);
/// user-process slots are non-negative.
///
/// Wraps an `i32` rather than aliasing it so the compiler can distinguish a
/// proc-table index from an [`Endpoint`] or a raw register value. Use
/// [`ProcNr::new`] for construction and [`ProcNr::get`] when an `i32` is
/// needed (e.g. bit-ops or casts to `usize`).
#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcNr(i32);

impl ProcNr {
    pub const fn new(n: i32) -> Self {
        Self(n)
    }
    pub const fn get(self) -> i32 {
        self.0
    }
    pub const fn is_task(self) -> bool {
        self.0 < 0
    }
}

impl fmt::Display for ProcNr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Index into the privilege table. Sized to cover `NR_SYS_PROCS` slots; each
/// system (privileged) process has its own slot, and all non-system user
/// processes share one. Mirrors MINIX 3 `priv.h`'s `s_id` field.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrivId(u16);

impl PrivId {
    pub const fn new(n: u16) -> Self {
        Self(n)
    }
    pub const fn get(self) -> u16 {
        self.0
    }
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Display for PrivId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// MINIX 3 spelling of [`PrivId`]; kept as a type alias for code that reads
/// closer to the reference (e.g. translating `sys_id_t s_id`). The two names
/// are interchangeable.
pub type SysId = PrivId;

/// Width of the proc-number field within an endpoint, in bits. Generation
/// occupies the bits above this.
pub const ENDPOINT_GEN_SHIFT: i32 = 15;

/// Mask covering the proc-number field of an endpoint.
pub const ENDPOINT_PROC_MASK: i32 = (1 << ENDPOINT_GEN_SHIFT) - 1;

/// Highest positive proc-number representable at generation zero (one less
/// than the sign-bit boundary of the proc field). Sentinel endpoints live
/// just below this so they decode to gen 0 with proc-numbers well outside
/// any plausible `NR_PROCS`. Mirrors MINIX 3's `_ENDPOINT_SLOT_TOP`.
pub const ENDPOINT_SLOT_TOP: ProcNr = ProcNr::new((1 << (ENDPOINT_GEN_SHIFT - 1)) - 1);

/// Special endpoint matching any sender (used as the `src` argument to
/// `RECEIVE`). Always at generation zero.
pub const ANY: Endpoint = make_endpoint(0, ENDPOINT_SLOT_TOP);

/// Special endpoint meaning "no process". Always at generation zero.
pub const NONE: Endpoint = make_endpoint(0, ProcNr::new(ENDPOINT_SLOT_TOP.get() - 1));

/// Special endpoint referring to the calling process itself. Always at
/// generation zero.
pub const SELF: Endpoint = make_endpoint(0, ProcNr::new(ENDPOINT_SLOT_TOP.get() - 2));

/// Encode `(generation, proc_nr)` into an endpoint.
pub const fn make_endpoint(g: GenNr, p: ProcNr) -> Endpoint {
    (g << ENDPOINT_GEN_SHIFT) | (p.get() & ENDPOINT_PROC_MASK)
}

/// Extract the process-table slot number from an endpoint, sign-extending
/// the lower `ENDPOINT_GEN_SHIFT` bits so that task slots come back negative.
pub const fn endpoint_proc(e: Endpoint) -> ProcNr {
    let masked = e & ENDPOINT_PROC_MASK;
    let sign_bit = 1 << (ENDPOINT_GEN_SHIFT - 1);
    let signed = if masked & sign_bit != 0 {
        masked | !ENDPOINT_PROC_MASK
    } else {
        masked
    };
    ProcNr::new(signed)
}

/// Extract the generation counter from an endpoint.
pub const fn endpoint_gen(e: Endpoint) -> GenNr {
    e >> ENDPOINT_GEN_SHIFT
}

/// Highest generation an endpoint can carry: the generation field occupies
/// bits `ENDPOINT_GEN_SHIFT..31`, and the top (sign) bit must stay clear so
/// endpoints remain positive `i32`s on the wire. Mirrors MINIX 3
/// `_ENDPOINT_MAX_GENERATION`.
pub const ENDPOINT_MAX_GENERATION: GenNr = (1 << (31 - ENDPOINT_GEN_SHIFT)) - 1;

/// Advance an endpoint's generation for slot reuse (`SYS_EXIT` frees a slot,
/// `SYS_FORK` consumes the bumped endpoint). Wraps to **1**, never back to 0 —
/// generation 0 is reserved for boot endpoints (MINIX 3 `do_fork.c` parity),
/// so a recycled slot can never alias a `boot_endpoint(nr)` again. The proc
/// field is preserved.
pub const fn bump_generation(e: Endpoint) -> Endpoint {
    let g = endpoint_gen(e);
    let next = if g >= ENDPOINT_MAX_GENERATION {
        1
    } else {
        g + 1
    };
    make_endpoint(next, endpoint_proc(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::NR_PROCS;

    #[test]
    fn special_endpoints_distinct() {
        assert_ne!(ANY, SELF);
        assert_ne!(ANY, NONE);
        assert_ne!(SELF, NONE);
    }

    #[test]
    fn special_endpoints_have_generation_zero() {
        // MINIX 3 invariant: ANY, NONE, SELF must decode to gen 0 so they
        // can never collide with a real (gen, proc) endpoint.
        for s in [ANY, NONE, SELF] {
            assert_eq!(endpoint_gen(s), 0, "sentinel {s:#x} should be at gen 0");
        }
    }

    #[test]
    fn special_endpoints_outside_proc_range() {
        // Sentinels must decode to proc-numbers well above any plausible
        // NR_PROCS so a real gen-0 endpoint never aliases a sentinel.
        for s in [ANY, NONE, SELF] {
            let p = endpoint_proc(s);
            assert!(
                p.get() >= NR_PROCS as i32,
                "sentinel {s:#x} decoded to proc {p}, inside NR_PROCS={NR_PROCS}"
            );
        }
    }

    #[test]
    fn encode_decode_positive_proc() {
        let e = make_endpoint(3, ProcNr::new(7));
        assert_eq!(endpoint_gen(e), 3);
        assert_eq!(endpoint_proc(e), ProcNr::new(7));
    }

    #[test]
    fn encode_decode_negative_proc() {
        // Task endpoints have negative proc numbers (SYSTEM=-2, IDLE=-4, …).
        let e = make_endpoint(1, ProcNr::new(-2));
        assert_eq!(endpoint_gen(e), 1);
        assert_eq!(endpoint_proc(e), ProcNr::new(-2));
    }

    #[test]
    fn encode_decode_generation_zero() {
        let e = make_endpoint(0, ProcNr::new(11));
        assert_eq!(endpoint_gen(e), 0);
        assert_eq!(endpoint_proc(e), ProcNr::new(11));
    }

    #[test]
    fn proc_field_isolated_from_gen_field() {
        // Bumping generation must not change the decoded proc_nr.
        for g in 0..8 {
            let e = make_endpoint(g, ProcNr::new(5));
            assert_eq!(endpoint_proc(e), ProcNr::new(5), "gen={g}");
        }
    }

    #[test]
    fn bump_generation_increments_and_preserves_proc() {
        let e = crate::com::boot_endpoint(ProcNr::new(16));
        let b = bump_generation(e);
        assert_eq!(endpoint_gen(b), 1);
        assert_eq!(endpoint_proc(b), ProcNr::new(16));
        let b2 = bump_generation(b);
        assert_eq!(endpoint_gen(b2), 2);
        assert_eq!(endpoint_proc(b2), ProcNr::new(16));
    }

    #[test]
    fn bump_generation_wraps_to_one_not_zero() {
        // Gen 0 is reserved for boot endpoints; the wrap must skip it so a
        // heavily-recycled slot never re-aliases boot_endpoint(nr).
        let e = make_endpoint(ENDPOINT_MAX_GENERATION, ProcNr::new(16));
        let b = bump_generation(e);
        assert_eq!(endpoint_gen(b), 1);
        assert_eq!(endpoint_proc(b), ProcNr::new(16));
    }

    #[test]
    fn bump_generation_preserves_negative_proc() {
        // Task slots never recycle in practice, but the proc field must
        // survive the round trip regardless of sign.
        let e = make_endpoint(4, ProcNr::new(-2));
        let b = bump_generation(e);
        assert_eq!(endpoint_gen(b), 5);
        assert_eq!(endpoint_proc(b), ProcNr::new(-2));
    }

    #[test]
    fn endpoints_stay_positive_through_max_generation() {
        // The sign bit is the wire-format invariant ENDPOINT_MAX_GENERATION
        // protects: every (gen, user-proc) endpoint must be a positive i32.
        for g in [
            0,
            1,
            2,
            ENDPOINT_MAX_GENERATION - 1,
            ENDPOINT_MAX_GENERATION,
        ] {
            let e = make_endpoint(g, ProcNr::new(NR_PROCS as i32 - 1));
            assert!(e >= 0, "gen {g} produced negative endpoint {e:#x}");
        }
    }

    #[test]
    fn procnr_get_round_trip() {
        for n in [-5_i32, -1, 0, 1, 42, 1023] {
            assert_eq!(ProcNr::new(n).get(), n);
        }
    }

    #[test]
    fn procnr_is_task() {
        assert!(ProcNr::new(-2).is_task());
        assert!(ProcNr::new(-5).is_task());
        assert!(!ProcNr::new(0).is_task());
        assert!(!ProcNr::new(11).is_task());
    }

    #[test]
    fn priv_id_round_trips() {
        for n in [0_u16, 1, 15, 63] {
            let p = PrivId::new(n);
            assert_eq!(p.get(), n);
            assert_eq!(p.as_usize(), n as usize);
        }
    }

    #[test]
    fn priv_id_and_sys_id_alias() {
        // SysId is the MINIX-3 spelling of PrivId; they must be identical.
        let a: PrivId = PrivId::new(7);
        let b: SysId = a;
        assert_eq!(a, b);
    }
}
