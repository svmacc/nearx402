//! zx402 zero-knowledge code: the Option C policy circuit in [`policy`], the byte
//! encoding that NEAR's `alt_bn128_*` host functions expect, and the verifier
//! spike that measured them.
//!
//! The spike's circuit stands in for the size and shape of the policy circuit. It does
//! **not** enforce any spending rule yet. It proves knowledge of a secret `w`
//! such that hashing `w` and the public inputs through `rounds` Poseidon
//! permutations gives `output`. Every public input is absorbed, so swapping or
//! changing any of them must make verification fail.
//!
//! The setup uses a seed chosen by the caller. **Keys from a known seed offer no
//! security**; they exist to make test vectors reproducible.

pub mod policy;
pub mod store;

// Re-exported so callers name the same versions of these types.
pub use {ark_bn254, ark_groth16};

use anyhow::{anyhow, Result};
use ark_bn254::{Bn254, Fq, Fr, G1Affine, G2Affine};
use ark_crypto_primitives::sponge::constraints::CryptographicSpongeVar;
use ark_crypto_primitives::sponge::poseidon::constraints::PoseidonSpongeVar;
use ark_crypto_primitives::sponge::poseidon::{
    find_poseidon_ark_and_mds, PoseidonConfig, PoseidonSponge,
};
use ark_crypto_primitives::sponge::CryptographicSponge;
use ark_ec::AffineRepr;
use ark_ff::{BigInteger, PrimeField};
use ark_groth16::{Groth16, Proof, ProvingKey, VerifyingKey};
use ark_r1cs_std::alloc::AllocVar;
use ark_r1cs_std::eq::EqGadget;
use ark_r1cs_std::fields::fp::FpVar;
use ark_relations::r1cs::{
    ConstraintSynthesizer, ConstraintSystem, ConstraintSystemRef, SynthesisError,
};
use ark_snark::SNARK;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

/// Non-output public inputs. With `output` this gives the eight public inputs
/// the Option C statement plans to use (see `docs/threat-model.md`).
pub const NUM_ABSORBED: usize = 7;
pub const NUM_PUBLIC: usize = NUM_ABSORBED + 1;

/// Poseidon over the BN254 scalar field: rate 2, capacity 1, x^5 S-box, 8 full
/// and 57 partial rounds, constants from the Grain LFSR in arkworks.
pub fn poseidon_config() -> PoseidonConfig<Fr> {
    let (rate, full, partial, alpha) = (2usize, 8usize, 57usize, 5u64);
    let (ark, mds) = find_poseidon_ark_and_mds::<Fr>(
        Fr::MODULUS_BIT_SIZE as u64,
        rate,
        full as u64,
        partial as u64,
        0,
    );
    PoseidonConfig::new(full, partial, alpha, mds, ark, rate, 1)
}

/// Hashes `w` and the absorbed inputs, then squeezes and re-absorbs for the
/// remaining rounds. `rounds` sets the circuit size.
pub fn hash_native(cfg: &PoseidonConfig<Fr>, w: Fr, absorbed: &[Fr], rounds: usize) -> Fr {
    let mut sponge = PoseidonSponge::new(cfg);
    sponge.absorb(&w);
    for x in absorbed {
        sponge.absorb(x);
    }
    let mut h = sponge.squeeze_field_elements::<Fr>(1)[0];
    for _ in 1..rounds {
        let mut next = PoseidonSponge::new(cfg);
        next.absorb(&h);
        h = next.squeeze_field_elements::<Fr>(1)[0];
    }
    h
}

#[derive(Clone)]
pub struct SpikeCircuit {
    pub cfg: PoseidonConfig<Fr>,
    pub rounds: usize,
    pub w: Option<Fr>,
    pub absorbed: Option<[Fr; NUM_ABSORBED]>,
    pub output: Option<Fr>,
}

impl ConstraintSynthesizer<Fr> for SpikeCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        // Allocation order is the public-input order.
        let output = FpVar::new_input(cs.clone(), || {
            self.output.ok_or(SynthesisError::AssignmentMissing)
        })?;
        let mut absorbed = Vec::with_capacity(NUM_ABSORBED);
        for i in 0..NUM_ABSORBED {
            absorbed.push(FpVar::new_input(cs.clone(), || {
                self.absorbed
                    .map(|a| a[i])
                    .ok_or(SynthesisError::AssignmentMissing)
            })?);
        }
        let w = FpVar::new_witness(cs.clone(), || {
            self.w.ok_or(SynthesisError::AssignmentMissing)
        })?;

        let mut sponge = PoseidonSpongeVar::new(cs.clone(), &self.cfg);
        sponge.absorb(&w)?;
        sponge.absorb(&absorbed)?;
        let mut h = sponge.squeeze_field_elements(1)?.remove(0);
        for _ in 1..self.rounds {
            let mut next = PoseidonSpongeVar::new(cs.clone(), &self.cfg);
            next.absorb(&h)?;
            h = next.squeeze_field_elements(1)?.remove(0);
        }
        h.enforce_equal(&output)
    }
}

/// A proving statement with its witness: `[output, absorbed...]` are public.
pub struct Instance {
    pub w: Fr,
    pub absorbed: [Fr; NUM_ABSORBED],
    pub output: Fr,
}

impl Instance {
    /// A sample instance derived from a seed.
    pub fn sample(cfg: &PoseidonConfig<Fr>, rounds: usize, seed: u64) -> Self {
        use ark_std::UniformRand;
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let w = Fr::rand(&mut rng);
        let absorbed = std::array::from_fn(|_| Fr::rand(&mut rng));
        let output = hash_native(cfg, w, &absorbed, rounds);
        Self {
            w,
            absorbed,
            output,
        }
    }

    pub fn public_inputs(&self) -> Vec<Fr> {
        let mut v = Vec::with_capacity(NUM_PUBLIC);
        v.push(self.output);
        v.extend_from_slice(&self.absorbed);
        v
    }
}

/// Runs the circuit-specific setup from a known seed. **Not secure.**
pub fn setup(rounds: usize, seed: u64) -> Result<(ProvingKey<Bn254>, VerifyingKey<Bn254>)> {
    let circuit = SpikeCircuit {
        cfg: poseidon_config(),
        rounds,
        w: None,
        absorbed: None,
        output: None,
    };
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    Groth16::<Bn254>::circuit_specific_setup(circuit, &mut rng).map_err(|e| anyhow!("setup: {e}"))
}

pub fn prove(
    pk: &ProvingKey<Bn254>,
    rounds: usize,
    inst: &Instance,
    seed: u64,
) -> Result<Proof<Bn254>> {
    let circuit = SpikeCircuit {
        cfg: poseidon_config(),
        rounds,
        w: Some(inst.w),
        absorbed: Some(inst.absorbed),
        output: Some(inst.output),
    };
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    Groth16::<Bn254>::prove(pk, circuit, &mut rng).map_err(|e| anyhow!("prove: {e}"))
}

/// Number of R1CS constraints for a given size.
pub fn constraint_count(rounds: usize) -> Result<usize> {
    let cfg = poseidon_config();
    let inst = Instance::sample(&cfg, rounds, 1);
    let cs = ConstraintSystem::<Fr>::new_ref();
    SpikeCircuit {
        cfg,
        rounds,
        w: Some(inst.w),
        absorbed: Some(inst.absorbed),
        output: Some(inst.output),
    }
    .generate_constraints(cs.clone())
    .map_err(|e| anyhow!("synthesize: {e}"))?;
    if !cs.is_satisfied().map_err(|e| anyhow!("{e}"))? {
        return Err(anyhow!("sample instance does not satisfy the circuit"));
    }
    Ok(cs.num_constraints())
}

// ---- NEAR byte encoding ----

pub fn fq_le(x: &Fq) -> [u8; 32] {
    x.into_bigint()
        .to_bytes_le()
        .try_into()
        .expect("Fq is 32 bytes")
}

pub fn fr_le(x: &Fr) -> [u8; 32] {
    x.into_bigint()
        .to_bytes_le()
        .try_into()
        .expect("Fr is 32 bytes")
}

pub fn g1_bytes(p: &G1Affine) -> Vec<u8> {
    let (x, y) = p.xy().expect("point at infinity has no NEAR encoding here");
    [fq_le(&x), fq_le(&y)].concat()
}

pub fn g2_bytes(p: &G2Affine) -> Vec<u8> {
    let (x, y) = p.xy().expect("point at infinity has no NEAR encoding here");
    [fq_le(&x.c0), fq_le(&x.c1), fq_le(&y.c0), fq_le(&y.c1)].concat()
}

/// A G2 point on the curve but outside the prime-order subgroup, for testing
/// that verification refuses it.
pub fn g2_outside_subgroup() -> G2Affine {
    use ark_bn254::Fq2;
    use ark_ff::Field;
    let mut x = Fq2::ONE;
    loop {
        if let Some(p) = G2Affine::get_point_from_x_unchecked(x, false) {
            if p.is_on_curve() && !p.is_in_correct_subgroup_assuming_on_curve() {
                return p;
            }
        }
        x += Fq2::ONE;
    }
}

/// `A || B || C`, 256 bytes.
pub fn proof_bytes(p: &Proof<Bn254>) -> Vec<u8> {
    [g1_bytes(&p.a), g2_bytes(&p.b), g1_bytes(&p.c)].concat()
}

pub fn inputs_bytes(inputs: &[Fr]) -> Vec<u8> {
    inputs.iter().flat_map(fr_le).collect()
}

/// The verifying key in the form the spike contract's `new` takes: G2 points
/// are negated, because the contract does no curve arithmetic of its own.
pub struct NearVk {
    pub alpha: Vec<u8>,
    pub neg_beta: Vec<u8>,
    pub neg_gamma: Vec<u8>,
    pub neg_delta: Vec<u8>,
    pub ic: Vec<u8>,
}

pub fn near_vk(vk: &VerifyingKey<Bn254>) -> NearVk {
    let neg = |p: &G2Affine| -> G2Affine { -*p };
    NearVk {
        alpha: g1_bytes(&vk.alpha_g1),
        neg_beta: g2_bytes(&neg(&vk.beta_g2)),
        neg_gamma: g2_bytes(&neg(&vk.gamma_g2)),
        neg_delta: g2_bytes(&neg(&vk.delta_g2)),
        ic: vk.gamma_abc_g1.iter().flat_map(g1_bytes).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape the Poseidon paper gives for 128-bit security at t = 3 and x^5
    /// over a 254-bit field. This checks that the arguments reached the generator
    /// as intended, and that the matrix is MDS. It does not run the reference
    /// script's invariant-subspace checks, which arkworks does not run either.
    #[test]
    fn poseidon_parameters_have_the_expected_shape_and_an_mds_matrix() {
        let c = poseidon_config();
        assert_eq!(
            (c.full_rounds, c.partial_rounds, c.alpha, c.rate, c.capacity),
            (8, 57, 5, 2, 1)
        );
        assert_eq!(c.ark.len(), 65);
        assert!(c.ark.iter().all(|round| round.len() == 3));
        let mut constants: Vec<Fr> = c.ark.iter().flatten().copied().collect();
        constants.sort();
        constants.dedup();
        assert_eq!(constants.len(), 65 * 3, "round constants must be distinct");

        // A matrix is MDS when every square submatrix is nonsingular.
        let m = &c.mds;
        assert_eq!(m.len(), 3);
        assert!(m.iter().all(|row| row.len() == 3));
        let zero = Fr::from(0u64);
        let det2 = |r1: usize, r2: usize, c1: usize, c2: usize| {
            m[r1][c1] * m[r2][c2] - m[r1][c2] * m[r2][c1]
        };
        assert!(m.iter().flatten().all(|x| *x != zero));
        for (r1, r2) in [(0, 1), (0, 2), (1, 2)] {
            for (c1, c2) in [(0, 1), (0, 2), (1, 2)] {
                assert_ne!(det2(r1, r2, c1, c2), zero);
            }
        }
        let det3 =
            m[0][0] * det2(1, 2, 1, 2) - m[0][1] * det2(1, 2, 0, 2) + m[0][2] * det2(1, 2, 0, 1);
        assert_ne!(det3, zero);
    }

    const ROUNDS: usize = 4;

    fn native_verify(vk: &VerifyingKey<Bn254>, inputs: &[Fr], proof: &Proof<Bn254>) -> bool {
        Groth16::<Bn254>::verify(vk, inputs, proof).unwrap()
    }

    #[test]
    fn valid_proof_verifies_and_tampered_inputs_do_not() {
        let cfg = poseidon_config();
        let (pk, vk) = setup(ROUNDS, 7).unwrap();
        let inst = Instance::sample(&cfg, ROUNDS, 11);
        let proof = prove(&pk, ROUNDS, &inst, 13).unwrap();
        let inputs = inst.public_inputs();
        assert!(native_verify(&vk, &inputs, &proof));

        let mut swapped = inputs.clone();
        swapped.swap(1, 2);
        assert!(!native_verify(&vk, &swapped, &proof));

        let mut changed = inputs.clone();
        changed[3] += Fr::from(1u64);
        assert!(!native_verify(&vk, &changed, &proof));
    }

    #[test]
    fn a_wrong_witness_does_not_satisfy_the_circuit() {
        let cfg = poseidon_config();
        let inst = Instance::sample(&cfg, ROUNDS, 11);
        let cs = ConstraintSystem::<Fr>::new_ref();
        SpikeCircuit {
            cfg,
            rounds: ROUNDS,
            w: Some(inst.w + Fr::from(1u64)),
            absorbed: Some(inst.absorbed),
            output: Some(inst.output),
        }
        .generate_constraints(cs.clone())
        .unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }

    #[test]
    fn encodings_have_the_sizes_the_contract_expects() {
        let (pk, vk) = setup(ROUNDS, 7).unwrap();
        let cfg = poseidon_config();
        let inst = Instance::sample(&cfg, ROUNDS, 11);
        let proof = prove(&pk, ROUNDS, &inst, 13).unwrap();
        assert_eq!(proof_bytes(&proof).len(), 256);
        assert_eq!(inputs_bytes(&inst.public_inputs()).len(), NUM_PUBLIC * 32);
        let v = near_vk(&vk);
        assert_eq!(v.alpha.len(), 64);
        assert_eq!(v.neg_beta.len(), 128);
        assert_eq!(v.ic.len(), (NUM_PUBLIC + 1) * 64);
    }
}
