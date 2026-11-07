//! Groth16 verification and public-input derivation for `pay_zk`. Curve
//! operations use only NEAR's `alt_bn128_*` host functions.
//!
//! Encoding: field elements are 32 bytes little-endian; G1 is `x || y`; G2 is
//! `x.c0 || x.c1 || y.c0 || y.c1`; a proof is `A || B || C`. The off-chain side
//! is `crates/zx402-zk/src/policy.rs`, and the two must stay in step.

use near_sdk::{env, near, require};

pub const G1: usize = 64;
pub const G2: usize = 128;
pub const FR: usize = 32;
pub const PROOF_LEN: usize = G1 + G2 + G1;
/// Public inputs of the policy circuit.
pub const NUM_INPUTS: usize = 8;

const PAIR: usize = G1 + G2;

const CONTEXT_DOMAIN: &[u8] = b"zx402:context:v1";
const VENDOR_DOMAIN: &[u8] = b"zx402:vendor:v1";
const INVOICE_DOMAIN: &[u8] = b"zx402:invoice:v1";

/// BN254 scalar field modulus, little-endian.
const FR_MODULUS: [u8; FR] = [
    0x01, 0x00, 0x00, 0xf0, 0x93, 0xf5, 0xe1, 0x43, 0x91, 0x70, 0xb9, 0x79, 0x48, 0xe8, 0x33, 0x28,
    0x5d, 0x58, 0x81, 0x81, 0xb6, 0x45, 0x50, 0xb8, 0x29, 0xa0, 0x31, 0xe1, 0x72, 0x4e, 0x64, 0x30,
];

pub type Fr = [u8; FR];

/// True if the bytes are a little-endian integer below the scalar modulus.
/// Without this, `x` and `x + r` would be two encodings of one value.
pub fn is_canonical(fr: &Fr) -> bool {
    for i in (0..FR).rev() {
        if fr[i] != FR_MODULUS[i] {
            return fr[i] < FR_MODULUS[i];
        }
    }
    false
}

/// A 32-byte field element from a contract argument; panics on anything else.
pub fn field_arg(bytes: &[u8], what: &str) -> Fr {
    let fr: Fr = bytes
        .try_into()
        .unwrap_or_else(|_| env::panic_str(&format!("{what} must be 32 bytes")));
    require!(is_canonical(&fr), format!("{what} is not canonical"));
    fr
}

pub fn fr_from_u64(x: u64) -> Fr {
    let mut out = [0u8; FR];
    out[..8].copy_from_slice(&x.to_le_bytes());
    out
}

pub fn fr_from_u128(x: u128) -> Fr {
    let mut out = [0u8; FR];
    out[..16].copy_from_slice(&x.to_le_bytes());
    out
}

/// SHA-256 of a domain tag and length-prefixed parts, truncated to 248 bits so
/// the result is always canonical.
fn field_hash(domain: &[u8], parts: &[&[u8]]) -> Fr {
    let mut data = domain.to_vec();
    for part in parts {
        data.extend_from_slice(&(part.len() as u32).to_le_bytes());
        data.extend_from_slice(part);
    }
    let mut digest: Fr = env::sha256_array(&data);
    digest[31] = 0;
    digest
}

pub fn context_field(contract: &str, agent: &str) -> Fr {
    field_hash(CONTEXT_DOMAIN, &[contract.as_bytes(), agent.as_bytes()])
}

pub fn vendor_field(vendor: &str) -> Fr {
    field_hash(VENDOR_DOMAIN, &[vendor.as_bytes()])
}

pub fn invoice_field(memo: &str) -> Fr {
    field_hash(INVOICE_DOMAIN, &[memo.as_bytes()])
}

#[near(serializers = [borsh])]
pub struct VerifyingKey {
    pub alpha: Vec<u8>,
    pub neg_beta: Vec<u8>,
    pub neg_gamma: Vec<u8>,
    pub neg_delta: Vec<u8>,
    /// `ic[0..=NUM_INPUTS]`, 64 bytes each.
    pub ic: Vec<u8>,
}

impl VerifyingKey {
    pub fn assert_well_formed(&self) {
        require!(self.alpha.len() == G1, "alpha must be 64 bytes");
        require!(self.neg_beta.len() == G2, "neg_beta must be 128 bytes");
        require!(self.neg_gamma.len() == G2, "neg_gamma must be 128 bytes");
        require!(self.neg_delta.len() == G2, "neg_delta must be 128 bytes");
        require!(
            self.ic.len() == (NUM_INPUTS + 1) * G1,
            "ic must hold one point per public input, plus one"
        );
    }

    /// Checks `e(A, B) * e(alpha, -beta) * e(vk_x, -gamma) * e(C, -delta) = 1`.
    /// A malformed point makes the host function abort the whole call.
    pub fn verify(&self, proof: &[u8], inputs: &[Fr; NUM_INPUTS]) -> bool {
        require!(proof.len() == PROOF_LEN, "proof must be 256 bytes");

        // vk_x = ic[0] + sum(input[i] * ic[i + 1])
        let mut msm = Vec::with_capacity(NUM_INPUTS * (G1 + FR));
        for (point, fr) in self.ic[G1..].as_chunks::<G1>().0.iter().zip(inputs) {
            msm.extend_from_slice(point);
            msm.extend_from_slice(fr);
        }
        let sum_inputs = env::alt_bn128_g1_multiexp(&msm);
        let mut sum = Vec::with_capacity(2 * (1 + G1));
        sum.push(0u8);
        sum.extend_from_slice(&self.ic[..G1]);
        sum.push(0u8);
        sum.extend_from_slice(&sum_inputs);
        let vk_x = env::alt_bn128_g1_sum(&sum);

        let (a, rest) = proof.split_at(G1);
        let (b, c) = rest.split_at(G2);
        let mut pairs = Vec::with_capacity(4 * PAIR);
        for part in [
            a,
            b,
            &self.alpha,
            &self.neg_beta,
            &vk_x,
            &self.neg_gamma,
            c,
            &self.neg_delta,
        ] {
            pairs.extend_from_slice(part);
        }
        env::alt_bn128_pairing_check(&pairs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_check_rejects_the_modulus_and_above() {
        assert!(is_canonical(&[0u8; FR]));
        let mut below = FR_MODULUS;
        below[0] = 0;
        assert!(is_canonical(&below));
        assert!(!is_canonical(&FR_MODULUS));
        assert!(!is_canonical(&[0xff; FR]));
    }

    #[test]
    fn small_values_encode_little_endian() {
        assert_eq!(fr_from_u64(1)[0], 1);
        assert_eq!(fr_from_u128(u128::MAX)[15], 0xff);
        assert_eq!(fr_from_u128(u128::MAX)[16], 0);
    }
}
