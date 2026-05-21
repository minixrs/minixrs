//! Endpoints and generation-aware proc-number encoding.
//!
//! An `Endpoint` is the kernel's name for a process. It encodes both the
//! process-table slot (`ProcNr`) and a generation counter (`GenNr`) so that
//! a slot reuse after a process exit produces a distinct endpoint. This
//! catches stale-endpoint bugs in user-space code.
//!
//! Mirrors MINIX 3 `include/minix/type.h` (`typedef int endpoint_t`) and
//! `include/minix/endpoint.h` (the `_ENDPOINT(g, n)` / `_ENDPOINT_P(e)` /
//! `_ENDPOINT_G(e)` macros), with one deliberate ABI deviation: MINIX 4
//! uses sign-extension of the low `ENDPOINT_GEN_SHIFT` bits to recover
//! negative task slots, rather than MINIX 3's `MAX_NR_TASKS` offset-bias
//! trick. The consequence is that `make_endpoint(0, p) != p` for negative
//! `p` — use `boot_endpoint(p)` explicitly when constructing an endpoint
//! from a kernel-task `ProcNr` constant like `SYSTEM`.

/// Kernel-assigned identifier for a process; encodes generation + proc_nr.
pub type Endpoint = i32;

/// Index into the process table. Task slots are negative (`SYSTEM=-2`, …);
/// user-process slots are non-negative.
pub type ProcNr = i32;

/// Generation counter — bumped when a process slot is reused.
pub type GenNr = i32;

/// Width of the proc-number field within an endpoint, in bits. Generation
/// occupies the bits above this.
pub const ENDPOINT_GEN_SHIFT: i32 = 15;

/// Mask covering the proc-number field of an endpoint.
pub const ENDPOINT_PROC_MASK: i32 = (1 << ENDPOINT_GEN_SHIFT) - 1;

/// Highest positive proc-number representable at generation zero (one less
/// than the sign-bit boundary of the proc field). Sentinel endpoints live
/// just below this so they decode to gen 0 with proc-numbers well outside
/// any plausible `NR_PROCS`. Mirrors MINIX 3's `_ENDPOINT_SLOT_TOP`.
pub const ENDPOINT_SLOT_TOP: ProcNr = (1 << (ENDPOINT_GEN_SHIFT - 1)) - 1;

/// Special endpoint matching any sender (used as the `src` argument to
/// `RECEIVE`). Always at generation zero.
pub const ANY: Endpoint = make_endpoint(0, ENDPOINT_SLOT_TOP);

/// Special endpoint meaning "no process". Always at generation zero.
pub const NONE: Endpoint = make_endpoint(0, ENDPOINT_SLOT_TOP - 1);

/// Special endpoint referring to the calling process itself. Always at
/// generation zero.
pub const SELF: Endpoint = make_endpoint(0, ENDPOINT_SLOT_TOP - 2);

/// Encode `(generation, proc_nr)` into an endpoint.
pub const fn make_endpoint(g: GenNr, p: ProcNr) -> Endpoint {
    (g << ENDPOINT_GEN_SHIFT) | (p & ENDPOINT_PROC_MASK)
}

/// Extract the process-table slot number from an endpoint, sign-extending
/// the lower `ENDPOINT_GEN_SHIFT` bits so that task slots come back negative.
pub const fn endpoint_proc(e: Endpoint) -> ProcNr {
    let masked = e & ENDPOINT_PROC_MASK;
    let sign_bit = 1 << (ENDPOINT_GEN_SHIFT - 1);
    if masked & sign_bit != 0 {
        masked | !ENDPOINT_PROC_MASK
    } else {
        masked
    }
}

/// Extract the generation counter from an endpoint.
pub const fn endpoint_gen(e: Endpoint) -> GenNr {
    e >> ENDPOINT_GEN_SHIFT
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
                p >= NR_PROCS as ProcNr,
                "sentinel {s:#x} decoded to proc {p}, inside NR_PROCS={NR_PROCS}"
            );
        }
    }

    #[test]
    fn encode_decode_positive_proc() {
        let e = make_endpoint(3, 7);
        assert_eq!(endpoint_gen(e), 3);
        assert_eq!(endpoint_proc(e), 7);
    }

    #[test]
    fn encode_decode_negative_proc() {
        // Task endpoints have negative proc numbers (SYSTEM=-2, IDLE=-4, …).
        let e = make_endpoint(1, -2);
        assert_eq!(endpoint_gen(e), 1);
        assert_eq!(endpoint_proc(e), -2);
    }

    #[test]
    fn encode_decode_generation_zero() {
        let e = make_endpoint(0, 11);
        assert_eq!(endpoint_gen(e), 0);
        assert_eq!(endpoint_proc(e), 11);
    }

    #[test]
    fn proc_field_isolated_from_gen_field() {
        // Bumping generation must not change the decoded proc_nr.
        for g in 0..8 {
            let e = make_endpoint(g, 5);
            assert_eq!(endpoint_proc(e), 5, "gen={g}");
        }
    }
}
