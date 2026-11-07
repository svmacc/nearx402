//! Spike: verifies a Groth16 proof over BN254 using only NEAR's `alt_bn128_*`
//! host functions, to measure gas and validate the byte format. Not for
//! production and not part of the policy contract.
//!
//! The check is `e(A, B) * e(alpha, -beta) * e(vk_x, -gamma) * e(C, -delta) = 1`,
//! where `vk_x = ic[0] + sum(input[i] * ic[i + 1])`. The verifying key stores
//! the negated G2 points, so the contract does no curve arithmetic of its own.
//!
//! Encoding, as the host functions expect it: every field element is 32 bytes
//! little-endian. G1 is `x || y` (64 bytes). G2 is `x.c0 || x.c1 || y.c0 || y.c1`
//! (128 bytes). A proof is `A || B || C` (256 bytes).

use near_sdk::json_types::Base64VecU8;
use near_sdk::{env, near, require, PanicOnDefault};

const G1: usize = 64;
const G2: usize = 128;
const FR: usize = 32;
const PROOF: usize = G1 + G2 + G1;
const PAIR: usize = G1 + G2;

/// BN254 scalar field modulus, little-endian.
const FR_MODULUS: [u8; FR] = [
    0x01, 0x00, 0x00, 0xf0, 0x93, 0xf5, 0xe1, 0x43, 0x91, 0x70, 0xb9, 0x79, 0x48, 0xe8, 0x33, 0x28,
    0x5d, 0x58, 0x81, 0x81, 0xb6, 0x45, 0x50, 0xb8, 0x29, 0xa0, 0x31, 0xe1, 0x72, 0x4e, 0x64, 0x30,
];

/// True if a little-endian 32-byte integer is below the scalar modulus.
fn is_canonical(fr: &[u8; FR]) -> bool {
    for i in (0..FR).rev() {
        if fr[i] != FR_MODULUS[i] {
            return fr[i] < FR_MODULUS[i];
        }
    }
    false
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    alpha: Vec<u8>,
    neg_beta: Vec<u8>,
    neg_gamma: Vec<u8>,
    neg_delta: Vec<u8>,
    /// `ic[0..=n]`, 64 bytes each.
    ic: Vec<u8>,
    verified: u64,
}

#[near]
impl Contract {
    /// Stores the verifying key. Spike only: anyone can initialize a fresh deployment.
    #[init]
    pub fn new(
        alpha: Base64VecU8,
        neg_beta: Base64VecU8,
        neg_gamma: Base64VecU8,
        neg_delta: Base64VecU8,
        ic: Base64VecU8,
    ) -> Self {
        require!(alpha.0.len() == G1, "alpha must be 64 bytes");
        require!(neg_beta.0.len() == G2, "beta must be 128 bytes");
        require!(neg_gamma.0.len() == G2, "gamma must be 128 bytes");
        require!(neg_delta.0.len() == G2, "delta must be 128 bytes");
        require!(
            ic.0.len() >= 2 * G1 && ic.0.len().is_multiple_of(G1),
            "ic must hold at least two G1 points"
        );
        Self {
            alpha: alpha.0,
            neg_beta: neg_beta.0,
            neg_gamma: neg_gamma.0,
            neg_delta: neg_delta.0,
            ic: ic.0,
            verified: 0,
        }
    }

    /// Verifies one proof and records success in storage, so the measured gas
    /// includes a state read and a state write. Returns the pairing result.
    pub fn verify(&mut self, proof: Base64VecU8, inputs: Base64VecU8) -> bool {
        let ok = self.check(&proof.0, &inputs.0);
        if ok {
            self.verified += 1;
        }
        ok
    }

    /// Number of proofs accepted so far.
    pub fn verified(&self) -> u64 {
        self.verified
    }
}

impl Contract {
    fn check(&self, proof: &[u8], inputs: &[u8]) -> bool {
        require!(proof.len() == PROOF, "proof must be 256 bytes");
        let n = self.ic.len() / G1 - 1;
        require!(inputs.len() == n * FR, "wrong number of public inputs");
        let scalars = inputs.as_chunks::<FR>().0;
        for fr in scalars {
            require!(is_canonical(fr), "public input is not canonical");
        }

        // vk_x = ic[0] + sum(input[i] * ic[i + 1])
        let mut msm = Vec::with_capacity(n * (G1 + FR));
        for (point, fr) in self.ic[G1..].as_chunks::<G1>().0.iter().zip(scalars) {
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
}
