// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! CRC-32 as GPT uses it: the IEEE 802.3 polynomial, reflected, with an
//! all-ones initial value and final XOR.
//!
//! Table-driven, with the table computed at compile time. This is deliberately
//! *not* the code in `driver-rt::gpt` (which goes a bit at a time): the writer
//! and the parser share nothing, so that agreeing on an image means two separate
//! derivations agree.

/// The reflected form of the generator polynomial `0x04C11DB7`.
const REFLECTED_POLYNOMIAL: u32 = 0xEDB8_8320;

/// `TABLE[b]` is the CRC register after shifting the single byte `b` through an
/// all-zero register.
const TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut byte = 0;
    while byte < 256 {
        let mut reg = byte as u32;
        let mut bit = 0;
        while bit < 8 {
            let carry = reg & 1;
            reg >>= 1;
            if carry != 0 {
                reg ^= REFLECTED_POLYNOMIAL;
            }
            bit += 1;
        }
        table[byte] = reg;
        byte += 1;
    }
    table
};

/// The CRC-32 of `bytes`. `crc32(b"123456789")` is `0xCBF43926`.
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(!0u32, |reg, &b| {
        TABLE[usize::from((reg as u8) ^ b)] ^ (reg >> 8)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_published_check_value() {
        // The "check" value every CRC catalogue lists for CRC-32/ISO-HDLC.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn the_empty_input_and_a_single_zero_byte() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(&[0]), 0xD202_EF8D);
    }

    #[test]
    fn a_changed_byte_changes_the_sum() {
        let mut bytes = [0u8; 92];
        let before = crc32(&bytes);
        bytes[40] = 1;
        assert_ne!(crc32(&bytes), before);
    }
}
