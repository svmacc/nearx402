//! The Option C policy circuit: proves that one payment obeys a private spending
//! policy, without revealing the caps or the allowlist.
//!
//! Statement, with public inputs in this exact order:
//!
//! | # | Input | Who supplies it on chain |
//! | --- | --- | --- |
//! | 0 | `context` | Contract computes from its own id and the caller |
//! | 1 | `policy_commitment` | Contract storage |
//! | 2 | `old_state` | Contract storage |
//! | 3 | `new_state` | Agent, constrained here |
//! | 4 | `day` | Contract computes from block time |
//! | 5 | `vendor` | Contract hashes the account it transfers to |
//! | 6 | `amount` | Contract uses the amount it transfers |
//! | 7 | `invoice` | Contract hashes the memo it records |
//!
//! Vendor and amount stay public in the token transfer. This circuit does not
//! hide them, and the agent proving it must know the whole policy.

use std::sync::OnceLock;

use anyhow::{anyhow, bail, Result};
use ark_bn254::{Bn254, Fr};
use ark_crypto_primitives::sponge::constraints::CryptographicSpongeVar;
use ark_crypto_primitives::sponge::poseidon::constraints::PoseidonSpongeVar;
use ark_crypto_primitives::sponge::poseidon::{PoseidonConfig, PoseidonSponge};
use ark_crypto_primitives::sponge::CryptographicSponge;
use ark_ff::{BigInteger, PrimeField};
use ark_groth16::{Groth16, Proof, ProvingKey, VerifyingKey};
use ark_r1cs_std::alloc::AllocVar;
use ark_r1cs_std::boolean::Boolean;
use ark_r1cs_std::eq::EqGadget;
use ark_r1cs_std::fields::fp::FpVar;
use ark_r1cs_std::fields::FieldVar;
use ark_r1cs_std::prelude::CondSelectGadget;
use ark_r1cs_std::R1CSVar;
use ark_relations::r1cs::{
    ConstraintSynthesizer, ConstraintSystem, ConstraintSystemRef, SynthesisError,
};
use ark_snark::SNARK;
use ark_std::rand::{CryptoRng, RngCore};
use ark_std::UniformRand;
use sha2::{Digest, Sha256};

use crate::poseidon_config;

/// Allowlist tree depth: up to 256 vendors.
pub const MERKLE_DEPTH: usize = 8;
pub const NUM_PUBLIC: usize = 8;

/// Bits for token amounts, the same width as the contract's `u128`.
const AMOUNT_BITS: usize = 128;
/// `spent + amount` of two 128-bit values needs one more bit.
const SUM_BITS: usize = 129;
const DAY_BITS: usize = 64;

// Poseidon domain tags, absorbed first so hashes of different kinds never collide.
const DOM_POLICY: u64 = 1;
const DOM_STATE: u64 = 2;
const DOM_LEAF: u64 = 3;
const DOM_NODE: u64 = 4;

// SHA-256 domain tags for values the contract derives. Keep in step with
// `contracts/policy/src/zk.rs`.
const CONTEXT_DOMAIN: &[u8] = b"zx402:context:v1";
const VENDOR_DOMAIN: &[u8] = b"zx402:vendor:v1";
const INVOICE_DOMAIN: &[u8] = b"zx402:invoice:v1";

fn cfg() -> &'static PoseidonConfig<Fr> {
    static CFG: OnceLock<PoseidonConfig<Fr>> = OnceLock::new();
    CFG.get_or_init(poseidon_config)
}

// ---- native hashes ----

fn poseidon(domain: u64, fields: &[Fr]) -> Fr {
    let mut sponge = PoseidonSponge::new(cfg());
    sponge.absorb(&Fr::from(domain));
    for x in fields {
        sponge.absorb(x);
    }
    sponge.squeeze_field_elements::<Fr>(1)[0]
}

pub fn policy_commitment(cap: Fr, per_call: Fr, root: Fr, salt: Fr) -> Fr {
    poseidon(DOM_POLICY, &[cap, per_call, root, salt])
}

pub fn state_commitment(day: Fr, spent: Fr, salt: Fr) -> Fr {
    poseidon(DOM_STATE, &[day, spent, salt])
}

fn leaf_hash(vendor: Fr) -> Fr {
    poseidon(DOM_LEAF, &[vendor])
}

fn node_hash(left: Fr, right: Fr) -> Fr {
    poseidon(DOM_NODE, &[left, right])
}

/// SHA-256 of a domain tag and length-prefixed parts, truncated to 248 bits so
/// the result is always a canonical field element.
fn field_hash(domain: &[u8], parts: &[&[u8]]) -> Fr {
    let mut h = Sha256::new();
    h.update(domain);
    for part in parts {
        h.update((part.len() as u32).to_le_bytes());
        h.update(part);
    }
    let mut digest: [u8; 32] = h.finalize().into();
    digest[31] = 0;
    Fr::from_le_bytes_mod_order(&digest)
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

// ---- allowlist tree ----

/// Sibling hashes from the leaf upward; `is_right[i]` says the node on the path
/// at level `i` is the right child.
#[derive(Clone, Debug)]
pub struct MerklePath {
    pub siblings: [Fr; MERKLE_DEPTH],
    pub is_right: [bool; MERKLE_DEPTH],
}

/// A fixed-depth tree of vendor leaves. Unused slots hold zero, which no
/// `leaf_hash` output can equal in practice.
pub struct Allowlist {
    /// `levels[0]` is the leaves; the last level holds only the root.
    levels: Vec<Vec<Fr>>,
    vendors: Vec<Fr>,
}

impl Allowlist {
    pub fn new(vendors: &[Fr]) -> Result<Self> {
        let capacity = 1usize << MERKLE_DEPTH;
        if vendors.len() > capacity {
            bail!("allowlist holds at most {capacity} vendors");
        }
        let mut level: Vec<Fr> = vendors.iter().map(|v| leaf_hash(*v)).collect();
        level.resize(capacity, Fr::from(0u64));
        let mut levels = vec![level];
        for _ in 0..MERKLE_DEPTH {
            let next = levels
                .last()
                .expect("levels is never empty")
                .as_chunks::<2>()
                .0
                .iter()
                .map(|[left, right]| node_hash(*left, *right))
                .collect();
            levels.push(next);
        }
        Ok(Self {
            levels,
            vendors: vendors.to_vec(),
        })
    }

    pub fn root(&self) -> Fr {
        self.levels[MERKLE_DEPTH][0]
    }

    /// The path of an allowlisted vendor, or `None` if it is not on the list.
    pub fn path(&self, vendor: Fr) -> Option<MerklePath> {
        let mut index = self.vendors.iter().position(|v| *v == vendor)?;
        let mut siblings = [Fr::from(0u64); MERKLE_DEPTH];
        let mut is_right = [false; MERKLE_DEPTH];
        for level in 0..MERKLE_DEPTH {
            siblings[level] = self.levels[level][index ^ 1];
            is_right[level] = index & 1 == 1;
            index >>= 1;
        }
        Some(MerklePath { siblings, is_right })
    }
}

// ---- statement ----

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PolicyPublic {
    pub context: Fr,
    pub policy_commitment: Fr,
    pub old_state: Fr,
    pub new_state: Fr,
    pub day: Fr,
    pub vendor: Fr,
    pub amount: Fr,
    pub invoice: Fr,
}

impl PolicyPublic {
    /// In the order the circuit allocates them and the contract supplies them.
    pub fn to_inputs(&self) -> [Fr; NUM_PUBLIC] {
        [
            self.context,
            self.policy_commitment,
            self.old_state,
            self.new_state,
            self.day,
            self.vendor,
            self.amount,
            self.invoice,
        ]
    }
}

/// The private side of the statement. Field-typed, so a test can play a
/// malicious prover and supply values no honest wallet would produce.
#[derive(Clone, Debug)]
pub struct PolicyWitness {
    pub cap: Fr,
    pub per_call: Fr,
    pub root: Fr,
    pub policy_salt: Fr,
    pub old_day: Fr,
    pub spent: Fr,
    pub old_salt: Fr,
    pub new_salt: Fr,
    pub path: MerklePath,
}

#[derive(Clone)]
pub struct PolicyCircuit {
    pub public: Option<PolicyPublic>,
    pub witness: Option<PolicyWitness>,
}

// ---- gadgets ----

fn poseidon_var(
    cs: &ConstraintSystemRef<Fr>,
    domain: u64,
    fields: &[&FpVar<Fr>],
) -> Result<FpVar<Fr>, SynthesisError> {
    let mut sponge = PoseidonSpongeVar::new(cs.clone(), cfg());
    sponge.absorb(&FpVar::constant(Fr::from(domain)))?;
    for x in fields {
        sponge.absorb(*x)?;
    }
    Ok(sponge.squeeze_field_elements(1)?.remove(0))
}

/// Enforces `0 <= x < 2^bits`. Sound for `bits` well below the field size: the
/// bits are constrained boolean and their sum cannot wrap the modulus.
fn range_check(
    cs: &ConstraintSystemRef<Fr>,
    x: &FpVar<Fr>,
    bits: usize,
) -> Result<(), SynthesisError> {
    let mut vars = Vec::with_capacity(bits);
    for i in 0..bits {
        vars.push(Boolean::new_witness(cs.clone(), || {
            Ok(x.value()?.into_bigint().get_bit(i))
        })?);
    }
    Boolean::le_bits_to_fp(&vars)?.enforce_equal(x)
}

/// Enforces `a <= b` for values already known to fit `bits`. If `a > b`, then
/// `b - a` wraps to a value near the modulus, which fails the range check.
fn enforce_le(
    cs: &ConstraintSystemRef<Fr>,
    a: &FpVar<Fr>,
    b: &FpVar<Fr>,
    bits: usize,
) -> Result<(), SynthesisError> {
    range_check(cs, &(b - a), bits)
}

impl ConstraintSynthesizer<Fr> for PolicyCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let public = self.public;
        let input = |get: fn(&PolicyPublic) -> Fr| {
            FpVar::new_input(cs.clone(), || {
                public
                    .as_ref()
                    .map(get)
                    .ok_or(SynthesisError::AssignmentMissing)
            })
        };
        // Allocation order is the public-input order. See PolicyPublic::to_inputs.
        let context = input(|p| p.context)?;
        let policy = input(|p| p.policy_commitment)?;
        let old_state = input(|p| p.old_state)?;
        let new_state = input(|p| p.new_state)?;
        let day = input(|p| p.day)?;
        let vendor = input(|p| p.vendor)?;
        let amount = input(|p| p.amount)?;
        let invoice = input(|p| p.invoice)?;

        let w = self.witness;
        let witness = |get: fn(&PolicyWitness) -> Fr| {
            FpVar::new_witness(cs.clone(), || {
                w.as_ref().map(get).ok_or(SynthesisError::AssignmentMissing)
            })
        };
        let cap = witness(|w| w.cap)?;
        let per_call = witness(|w| w.per_call)?;
        let root = witness(|w| w.root)?;
        let policy_salt = witness(|w| w.policy_salt)?;
        let old_day = witness(|w| w.old_day)?;
        let spent = witness(|w| w.spent)?;
        let old_salt = witness(|w| w.old_salt)?;
        let new_salt = witness(|w| w.new_salt)?;

        // Every value used in arithmetic fits its width, so nothing can wrap
        // around the field modulus.
        range_check(&cs, &amount, AMOUNT_BITS)?;
        range_check(&cs, &cap, AMOUNT_BITS)?;
        range_check(&cs, &per_call, AMOUNT_BITS)?;
        range_check(&cs, &spent, AMOUNT_BITS)?;
        range_check(&cs, &day, DAY_BITS)?;
        range_check(&cs, &old_day, DAY_BITS)?;

        // The agent uses the policy finance committed to.
        poseidon_var(&cs, DOM_POLICY, &[&cap, &per_call, &root, &policy_salt])?
            .enforce_equal(&policy)?;

        // It starts from the state the contract holds.
        poseidon_var(&cs, DOM_STATE, &[&old_day, &spent, &old_salt])?.enforce_equal(&old_state)?;

        // The vendor actually being paid is on the allowlist.
        let mut node = poseidon_var(&cs, DOM_LEAF, &[&vendor])?;
        for level in 0..MERKLE_DEPTH {
            let sibling = FpVar::new_witness(cs.clone(), || {
                w.as_ref()
                    .map(|w| w.path.siblings[level])
                    .ok_or(SynthesisError::AssignmentMissing)
            })?;
            let is_right = Boolean::new_witness(cs.clone(), || {
                w.as_ref()
                    .map(|w| w.path.is_right[level])
                    .ok_or(SynthesisError::AssignmentMissing)
            })?;
            let left = FpVar::conditionally_select(&is_right, &sibling, &node)?;
            let right = FpVar::conditionally_select(&is_right, &node, &sibling)?;
            node = poseidon_var(&cs, DOM_NODE, &[&left, &right])?;
        }
        node.enforce_equal(&root)?;

        // Per-call limit.
        enforce_le(&cs, &amount, &per_call, AMOUNT_BITS)?;

        // Time only moves forward; the budget resets only when the day the
        // contract supplied is later than the committed one.
        enforce_le(&cs, &old_day, &day, DAY_BITS)?;
        let same_day = old_day.is_eq(&day)?;
        let new_spent = FpVar::conditionally_select(&same_day, &(&spent + &amount), &amount)?;

        // Daily cap.
        enforce_le(&cs, &new_spent, &cap, SUM_BITS)?;

        // The new state is exactly the result of this transition.
        poseidon_var(&cs, DOM_STATE, &[&day, &new_spent, &new_salt])?.enforce_equal(&new_state)?;

        // Tie the inputs used nowhere else into a constraint, so they cannot
        // be changed without invalidating the proof.
        let _ = context.square()?;
        let _ = invoice.square()?;
        Ok(())
    }
}

// ---- setup and proving ----

/// Runs the circuit-specific setup. Whoever knows `rng`'s output can forge
/// proofs, so it must not run where the agent can see it (see SD-007).
pub fn setup<R: RngCore + CryptoRng>(
    rng: &mut R,
) -> Result<(ProvingKey<Bn254>, VerifyingKey<Bn254>)> {
    let circuit = PolicyCircuit {
        public: None,
        witness: None,
    };
    Groth16::<Bn254>::circuit_specific_setup(circuit, rng).map_err(|e| anyhow!("setup: {e}"))
}

/// Builds a proof for any statement, satisfied or not. A false statement gives
/// a proof that the contract rejects, which is how a test plays a cheating agent.
pub fn prove<R: RngCore + CryptoRng>(
    pk: &ProvingKey<Bn254>,
    public: PolicyPublic,
    witness: PolicyWitness,
    rng: &mut R,
) -> Result<Proof<Bn254>> {
    let circuit = PolicyCircuit {
        public: Some(public),
        witness: Some(witness),
    };
    Groth16::<Bn254>::prove(pk, circuit, rng).map_err(|e| anyhow!("prove: {e}"))
}

/// Number of constraints and whether a statement satisfies them.
pub fn check(public: PolicyPublic, witness: PolicyWitness) -> Result<(usize, bool)> {
    let cs = ConstraintSystem::<Fr>::new_ref();
    PolicyCircuit {
        public: Some(public),
        witness: Some(witness),
    }
    .generate_constraints(cs.clone())
    .map_err(|e| anyhow!("synthesize: {e}"))?;
    let ok = cs.is_satisfied().map_err(|e| anyhow!("{e}"))?;
    Ok((cs.num_constraints(), ok))
}

// ---- agent wallet ----

/// Private policy as finance configured it. The agent must hold this to prove,
/// so a compromised agent learns the policy even though it cannot break it.
pub struct PolicyOpening {
    pub cap: u128,
    pub per_call: u128,
    pub vendors: Vec<String>,
    pub salt: Fr,
}

impl PolicyOpening {
    pub fn commitment(&self) -> Result<Fr> {
        let allowlist = self.allowlist()?;
        Ok(policy_commitment(
            Fr::from(self.cap),
            Fr::from(self.per_call),
            allowlist.root(),
            self.salt,
        ))
    }

    fn allowlist(&self) -> Result<Allowlist> {
        let leaves: Vec<Fr> = self.vendors.iter().map(|v| vendor_field(v)).collect();
        Allowlist::new(&leaves)
    }
}

/// The spending state behind the contract's current `state_commitment`.
#[derive(Clone, Copy, Debug)]
pub struct SpendState {
    pub day: u64,
    pub spent: u128,
    pub salt: Fr,
}

impl SpendState {
    pub fn fresh<R: RngCore + CryptoRng>(rng: &mut R) -> Self {
        Self {
            day: 0,
            spent: 0,
            salt: Fr::rand(rng),
        }
    }

    pub fn commitment(&self) -> Fr {
        state_commitment(Fr::from(self.day), Fr::from(self.spent), self.salt)
    }
}

/// A payment ready to send, and the state to adopt once it settles.
pub struct PreparedPayment {
    pub proof: Proof<Bn254>,
    pub public: PolicyPublic,
    pub next: SpendState,
}

/// Why an honest wallet refuses to prove. On chain these leave no trace: see
/// SD-004 in `docs/security-decisions.md`.
#[derive(Debug, PartialEq)]
pub enum Refusal {
    NotAllowlisted,
    OverPerCall,
    OverDailyCap,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotAllowlisted => "vendor is not on the private allowlist",
            Self::OverPerCall => "amount exceeds the per-call limit",
            Self::OverDailyCap => "payment would exceed the daily cap",
        })
    }
}

/// What the agent runs: holds the private policy and the current state, and
/// refuses to prove a payment outside policy.
pub struct ZkWallet {
    pub contract: String,
    pub agent: String,
    pub opening: PolicyOpening,
    pub state: SpendState,
}

impl ZkWallet {
    /// The public statement and witness for a payment. No policy checks: tests
    /// use this directly to attempt statements an honest wallet would refuse.
    pub fn statement<R: RngCore + CryptoRng>(
        &self,
        vendor: &str,
        amount: u128,
        memo: &str,
        day: u64,
        rng: &mut R,
    ) -> Result<(PolicyPublic, PolicyWitness, SpendState)> {
        let vendor_f = vendor_field(vendor);
        let allowlist = self.opening.allowlist()?;
        let path = allowlist.path(vendor_f).unwrap_or(MerklePath {
            siblings: [Fr::from(0u64); MERKLE_DEPTH],
            is_right: [false; MERKLE_DEPTH],
        });
        let spent = if day == self.state.day {
            self.state.spent.saturating_add(amount)
        } else {
            amount
        };
        let next = SpendState {
            day,
            spent,
            salt: Fr::rand(rng),
        };
        let public = PolicyPublic {
            context: context_field(&self.contract, &self.agent),
            policy_commitment: self.opening.commitment()?,
            old_state: self.state.commitment(),
            new_state: next.commitment(),
            day: Fr::from(day),
            vendor: vendor_f,
            amount: Fr::from(amount),
            invoice: invoice_field(memo),
        };
        let witness = PolicyWitness {
            cap: Fr::from(self.opening.cap),
            per_call: Fr::from(self.opening.per_call),
            root: allowlist.root(),
            policy_salt: self.opening.salt,
            old_day: Fr::from(self.state.day),
            spent: Fr::from(self.state.spent),
            old_salt: self.state.salt,
            new_salt: next.salt,
            path,
        };
        Ok((public, witness, next))
    }

    /// Checks policy locally, then proves. A refusal costs nothing and writes
    /// nothing on chain.
    pub fn prepare<R: RngCore + CryptoRng>(
        &self,
        pk: &ProvingKey<Bn254>,
        vendor: &str,
        amount: u128,
        memo: &str,
        day: u64,
        rng: &mut R,
    ) -> Result<std::result::Result<PreparedPayment, Refusal>> {
        if !self.opening.vendors.iter().any(|v| v == vendor) {
            return Ok(Err(Refusal::NotAllowlisted));
        }
        if amount > self.opening.per_call {
            return Ok(Err(Refusal::OverPerCall));
        }
        let (public, witness, next) = self.statement(vendor, amount, memo, day, rng)?;
        if next.spent > self.opening.cap {
            return Ok(Err(Refusal::OverDailyCap));
        }
        let proof = prove(pk, public, witness, rng)?;
        Ok(Ok(PreparedPayment {
            proof,
            public,
            next,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_std::rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    const VENDOR: &str = "data-api.near";
    const OTHER: &str = "gpu-cloud.near";
    const ATTACKER: &str = "attacker.near";
    const DAY: u64 = 20_000;

    fn rng() -> ChaCha20Rng {
        ChaCha20Rng::seed_from_u64(42)
    }

    fn wallet() -> ZkWallet {
        let mut r = rng();
        ZkWallet {
            contract: "policy.near".into(),
            agent: "agent.near".into(),
            opening: PolicyOpening {
                cap: 1_000,
                per_call: 500,
                vendors: vec![VENDOR.into(), OTHER.into()],
                salt: Fr::rand(&mut r),
            },
            state: SpendState {
                day: DAY,
                spent: 300,
                salt: Fr::rand(&mut r),
            },
        }
    }

    /// A valid payment: 100 to the first vendor, same day, 300 already spent.
    fn valid() -> (PolicyPublic, PolicyWitness) {
        let (public, witness, _) = wallet()
            .statement(VENDOR, 100, "nonce", DAY, &mut rng())
            .unwrap();
        (public, witness)
    }

    fn satisfied(public: PolicyPublic, witness: PolicyWitness) -> bool {
        check(public, witness).unwrap().1
    }

    fn neg(x: u64) -> Fr {
        -Fr::from(x)
    }

    /// Recomputes the commitments a malicious prover controls, so that a test
    /// exercises the rule it names and not merely a commitment mismatch.
    fn reseal(public: &mut PolicyPublic, w: &PolicyWitness) {
        public.policy_commitment = policy_commitment(w.cap, w.per_call, w.root, w.policy_salt);
        public.old_state = state_commitment(w.old_day, w.spent, w.old_salt);
    }

    #[test]
    fn valid_statement_satisfies_the_circuit() {
        let (public, witness) = valid();
        let (constraints, ok) = check(public, witness).unwrap();
        assert!(ok);
        println!("policy circuit constraints: {constraints}");
    }

    #[test]
    fn every_allowlist_slot_has_a_valid_path() {
        let vendors: Vec<Fr> = (0..256u64).map(Fr::from).collect();
        let list = Allowlist::new(&vendors).unwrap();
        for (i, v) in vendors.iter().enumerate().step_by(37) {
            let path = list.path(*v).unwrap();
            let mut node = leaf_hash(*v);
            for level in 0..MERKLE_DEPTH {
                let s = path.siblings[level];
                node = if path.is_right[level] {
                    node_hash(s, node)
                } else {
                    node_hash(node, s)
                };
            }
            assert_eq!(node, list.root(), "slot {i}");
        }
        assert!(list.path(Fr::from(999u64)).is_none());
        assert!(Allowlist::new(&vec![Fr::from(0u64); 257]).is_err());
    }

    #[test]
    fn spending_exactly_up_to_each_limit_is_allowed() {
        let w = wallet();
        // 500 is the per-call limit; 300 + 500 is within the cap of 1000.
        let (p, s, _) = w.statement(VENDOR, 500, "n", DAY, &mut rng()).unwrap();
        assert!(satisfied(p, s));

        // Exactly at the cap: 700 spent + 300 = 1000.
        let mut w = wallet();
        w.state.spent = 700;
        let (p, s, _) = w.statement(VENDOR, 300, "n", DAY, &mut rng()).unwrap();
        assert!(satisfied(p, s));
    }

    #[test]
    fn a_later_day_resets_the_budget() {
        let mut w = wallet();
        w.state.spent = 1_000;
        // Cap fully used today, so a new payment today fails...
        let (p, s, _) = w.statement(VENDOR, 100, "n", DAY, &mut rng()).unwrap();
        assert!(!satisfied(p, s));
        // ...and passes once the contract supplies the next day.
        let (p, s, next) = w.statement(VENDOR, 100, "n", DAY + 1, &mut rng()).unwrap();
        assert!(satisfied(p, s));
        assert_eq!(next.spent, 100);
    }

    #[test]
    fn amount_above_per_call_is_rejected() {
        let (p, s, _) = wallet()
            .statement(VENDOR, 501, "n", DAY, &mut rng())
            .unwrap();
        assert!(!satisfied(p, s));
    }

    #[test]
    fn exceeding_the_daily_cap_is_rejected() {
        let mut w = wallet();
        w.state.spent = 800;
        let (p, s, _) = w.statement(VENDOR, 201, "n", DAY, &mut rng()).unwrap();
        assert!(!satisfied(p, s));
    }

    #[test]
    fn vendor_outside_the_allowlist_is_rejected() {
        let (p, s, _) = wallet()
            .statement(ATTACKER, 100, "n", DAY, &mut rng())
            .unwrap();
        assert!(!satisfied(p, s));
    }

    #[test]
    fn allowlisted_path_cannot_be_reused_for_another_vendor() {
        // Valid path for VENDOR, but the public vendor is the attacker: the
        // recipient and the allowlist leaf must be the same value.
        let (mut p, s) = valid();
        p.vendor = vendor_field(ATTACKER);
        assert!(!satisfied(p, s));
    }

    #[test]
    fn empty_allowlist_slot_is_not_a_valid_leaf() {
        // Slot 2 is the first empty slot, and holds zero. Public vendor zero
        // still hashes to a real leaf, so it does not match the empty slot.
        let list = wallet().opening.allowlist().unwrap();
        let mut siblings = [Fr::from(0u64); MERKLE_DEPTH];
        let mut is_right = [false; MERKLE_DEPTH];
        let mut index = 2;
        for level in 0..MERKLE_DEPTH {
            siblings[level] = list.levels[level][index ^ 1];
            is_right[level] = index & 1 == 1;
            index >>= 1;
        }
        // Control: this really is the empty slot's path. From a raw zero it
        // reaches the root, so only the leaf hash stops the proof below.
        let mut node = Fr::from(0u64);
        for level in 0..MERKLE_DEPTH {
            node = if is_right[level] {
                node_hash(siblings[level], node)
            } else {
                node_hash(node, siblings[level])
            };
        }
        assert_eq!(node, list.root());

        let (mut p, mut s) = valid();
        p.vendor = Fr::from(0u64);
        s.path = MerklePath { siblings, is_right };
        assert!(!satisfied(p, s));
    }

    #[test]
    fn hashes_of_different_kinds_never_share_a_value() {
        let x = Fr::from(7u64);
        let zero = Fr::from(0u64);
        let outputs = [
            leaf_hash(x),
            node_hash(x, zero),
            node_hash(zero, x),
            state_commitment(x, zero, zero),
            policy_commitment(x, zero, zero, zero),
        ];
        for (i, a) in outputs.iter().enumerate() {
            for b in &outputs[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn a_different_policy_is_rejected() {
        // The agent proves against limits it made up.
        let (p, mut s) = valid();
        s.cap = Fr::from(1_000_000u64);
        assert!(!satisfied(p, s));
        let (p, mut s) = valid();
        s.per_call = Fr::from(1_000_000u64);
        assert!(!satisfied(p, s));
        let (p, mut s) = valid();
        s.policy_salt += Fr::from(1u64);
        assert!(!satisfied(p, s));
    }

    #[test]
    fn a_different_old_state_is_rejected() {
        // Each lie below is a way to spend the same daily budget twice. The new
        // state is sealed for the lie, so only the old state commitment stops it.
        // The control shows the rest of the statement is consistent.
        let seal = |p: &mut PolicyPublic, s: &PolicyWitness| {
            p.new_state = state_commitment(Fr::from(DAY), Fr::from(100u64), s.new_salt);
        };

        // Claims nothing has been spent today.
        let (mut p, mut s) = valid();
        s.spent = Fr::from(0u64);
        seal(&mut p, &s);
        assert!(!satisfied(p, s.clone()));
        p.old_state = state_commitment(s.old_day, s.spent, s.old_salt);
        assert!(satisfied(p, s), "control");

        // Claims the committed state was from an earlier day, to reset the budget.
        let (mut p, mut s) = valid();
        s.old_day = Fr::from(DAY - 1);
        seal(&mut p, &s);
        assert!(!satisfied(p, s.clone()));
        p.old_state = state_commitment(s.old_day, s.spent, s.old_salt);
        assert!(satisfied(p, s), "control");

        let (p, mut s) = valid();
        s.old_salt += Fr::from(1u64);
        assert!(!satisfied(p, s));
    }

    #[test]
    fn new_state_must_be_the_exact_transition() {
        let (public, witness) = valid();
        // Under-reports the spend.
        let mut p = public;
        p.new_state = state_commitment(Fr::from(DAY), Fr::from(300u64), witness.new_salt);
        assert!(!satisfied(p, witness.clone()));
        // Writes a far-future day, to dodge the same-day accumulation later.
        let mut p = public;
        p.new_state = state_commitment(Fr::from(DAY + 9), Fr::from(400u64), witness.new_salt);
        assert!(!satisfied(p, witness.clone()));
        // Resets the spend to just this amount on the same day.
        let mut p = public;
        p.new_state = state_commitment(Fr::from(DAY), Fr::from(100u64), witness.new_salt);
        assert!(!satisfied(p, witness));
    }

    #[test]
    fn a_day_earlier_than_the_committed_one_is_rejected() {
        let (mut p, s) = valid();
        p.day = Fr::from(DAY - 1);
        p.new_state = state_commitment(p.day, Fr::from(100u64), s.new_salt);
        assert!(!satisfied(p, s));
    }

    #[test]
    fn negative_amount_cannot_wrap_the_field() {
        // amount = -100 would make spent + amount smaller, and pass as under
        // the limits if it were not range-checked.
        let (mut p, s) = valid();
        p.amount = neg(100);
        p.new_state = state_commitment(Fr::from(DAY), Fr::from(200u64), s.new_salt);
        assert!(!satisfied(p, s));
    }

    #[test]
    fn oversized_witness_values_cannot_wrap_the_comparisons() {
        // A cap beyond 128 bits, correctly committed, must not widen the limits.
        let (mut p, mut s) = valid();
        s.cap = Fr::from(u128::MAX) + Fr::from(1u64);
        reseal(&mut p, &s);
        assert!(!satisfied(p, s));

        let (mut p, mut s) = valid();
        s.per_call = neg(1);
        reseal(&mut p, &s);
        assert!(!satisfied(p, s));

        // Just past 128 bits, where the comparison alone would still pass.
        let (mut p, mut s) = valid();
        s.per_call = Fr::from(u128::MAX) + Fr::from(1u64);
        reseal(&mut p, &s);
        assert!(!satisfied(p, s));

        // A "negative" spent that wraps back to a small sum.
        let (mut p, mut s) = valid();
        s.spent = neg(1_000);
        reseal(&mut p, &s);
        let wrapped = s.spent + Fr::from(100u64);
        p.new_state = state_commitment(Fr::from(DAY), wrapped, s.new_salt);
        assert!(!satisfied(p, s));
    }

    #[test]
    fn oversized_days_are_rejected() {
        // A "negative" old day compares below any day, so it passes as a new
        // day. New state is sealed for that case, so only the range check stops it.
        let (mut p, mut s) = valid();
        s.old_day = neg(1);
        reseal(&mut p, &s);
        p.new_state = state_commitment(p.day, Fr::from(100u64), s.new_salt);
        assert!(!satisfied(p, s));

        let (mut p, s) = valid();
        p.day = neg(1);
        p.new_state = state_commitment(p.day, Fr::from(100u64), s.new_salt);
        assert!(!satisfied(p, s));

        // Past 64 bits, yet close enough to the old day for the comparison to pass.
        let (mut p, s) = valid();
        p.day = Fr::from(DAY) + Fr::from(u64::MAX);
        p.new_state = state_commitment(p.day, Fr::from(100u64), s.new_salt);
        assert!(!satisfied(p, s));
    }

    #[test]
    fn same_day_payments_accumulate() {
        // Exactly what is valid today must include what was already spent.
        let (p, s, next) = wallet()
            .statement(VENDOR, 100, "n", DAY, &mut rng())
            .unwrap();
        assert!(satisfied(p, s));
        assert_eq!(next.spent, 400);
    }

    #[test]
    fn honest_wallet_refuses_before_proving() {
        let (pk, _) = setup(&mut rng()).unwrap();
        let w = wallet();
        let refusal = |vendor, amount| {
            w.prepare(&pk, vendor, amount, "n", DAY, &mut rng())
                .unwrap()
                .err()
        };
        assert_eq!(refusal(ATTACKER, 100), Some(Refusal::NotAllowlisted));
        assert_eq!(refusal(VENDOR, 501), Some(Refusal::OverPerCall));
        let mut full = wallet();
        full.state.spent = 950;
        assert_eq!(
            full.prepare(&pk, VENDOR, 100, "n", DAY, &mut rng())
                .unwrap()
                .err(),
            Some(Refusal::OverDailyCap)
        );
        assert!(refusal(VENDOR, 100).is_none());
    }

    #[test]
    fn real_proof_binds_every_public_input() {
        let (pk, vk) = setup(&mut rng()).unwrap();
        let (public, witness) = valid();
        let proof = prove(&pk, public, witness, &mut rng()).unwrap();
        let inputs = public.to_inputs();
        assert!(Groth16::<Bn254>::verify(&vk, &inputs, &proof).unwrap());
        // Including context and invoice, which appear in no other constraint.
        for i in 0..NUM_PUBLIC {
            let mut changed = inputs;
            changed[i] += Fr::from(1u64);
            assert!(
                !Groth16::<Bn254>::verify(&vk, &changed, &proof).unwrap(),
                "input {i} is not bound"
            );
        }
    }

    #[test]
    fn a_false_statement_cannot_be_proved() {
        // What a cheating agent would do: run the prover anyway.
        let (pk, vk) = setup(&mut rng()).unwrap();
        let (p, s, _) = wallet()
            .statement(ATTACKER, 100, "n", DAY, &mut rng())
            .unwrap();
        let proof = prove(&pk, p, s, &mut rng()).unwrap();
        assert!(!Groth16::<Bn254>::verify(&vk, &p.to_inputs(), &proof).unwrap());
    }
}
