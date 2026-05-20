//! Endpoints and generation-aware proc-number encoding.
//!
//! An `Endpoint` is the kernel's name for a process. It encodes both the
//! process-table slot (`ProcNr`) and a generation counter (`GenNr`) so that
//! a slot reuse after a process exit produces a distinct endpoint. This
//! catches stale-endpoint bugs in user-space code.
//!
//! Mirrors MINIX 3 `include/minix/type.h` (`typedef int endpoint_t`) and
//! `include/minix/endpoint.h` (the `_ENDPOINT(g, n)` / `_ENDPOINT_P(e)` /
//! `_ENDPOINT_G(e)` macros).

/// Kernel-assigned identifier for a process; encodes generation + proc_nr.
pub type Endpoint = i32;

/// Index into the process table. Task slots are negative (`SYSTEM=-2`, …);
/// user-process slots are non-negative.
pub type ProcNr = i32;

/// Generation counter — bumped when a process slot is reused.
pub type GenNr = i32;

/// Special endpoint matching any sender (used as the `src` argument to
/// `RECEIVE`).
pub const ANY: Endpoint = 0x7ace;

/// Special endpoint referring to the calling process itself.
pub const SELF: Endpoint = 0x8ace;

/// Special endpoint meaning "no process".
pub const NONE: Endpoint = 0x6ace;

const ENDPOINT_GEN_SHIFT: i32 = 15;
const ENDPOINT_PROC_MASK: i32 = (1 << ENDPOINT_GEN_SHIFT) - 1;

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

    #[test]
    fn special_endpoints_distinct() {
        assert_ne!(ANY, SELF);
        assert_ne!(ANY, NONE);
        assert_ne!(SELF, NONE);
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
