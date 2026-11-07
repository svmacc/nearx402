//! Runs real proofs through NEAR's `alt_bn128_*` host functions on a local
//! sandbox: valid proofs must pass and every tampered variant must not.
//! Needs `scripts/build-contracts.sh` to have run.
//!
//! Prints the measured gas. Run with `--nocapture` to see it.

use std::sync::Arc;

use ark_bn254::{Bn254, Fq2, Fr, G2Affine};
use ark_ec::CurveGroup;
use ark_ff::{BigInteger, Field, PrimeField};
use ark_groth16::Proof;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use near_api::signer::generate_secret_key;
use near_api::types::transaction::result::TransactionResultError;
use near_api::types::{AccountId, NearGas, NearToken};
use near_api::{Account, Contract, NetworkConfig, Signer};
use near_sandbox::config::{DEFAULT_GENESIS_ACCOUNT, DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY};
use near_sandbox::Sandbox;
use serde_json::json;
use zx402_zk::*;

/// Size of the circuit under test. About the size the policy circuit may reach
/// (see `docs/zk-spike.md`). Gas does not depend on this, only on public inputs.
const ROUNDS: usize = 8;

struct Spike {
    net: NetworkConfig,
    id: AccountId,
    signer: Arc<Signer>,
}

/// What a `verify` transaction did.
#[derive(Debug, PartialEq)]
enum Outcome {
    Returned(bool),
    Failed,
}

impl Spike {
    /// Returns the outcome and the gas the whole transaction burned.
    async fn verify(&self, proof: &[u8], inputs: &[u8]) -> (Outcome, u128) {
        let r = Contract(self.id.clone())
            .call_function(
                "verify",
                json!({ "proof": B64.encode(proof), "inputs": B64.encode(inputs) }),
            )
            .transaction()
            .gas(NearGas::from_tgas(300))
            .with_signer(self.id.clone(), self.signer.clone())
            .send_to(&self.net)
            .await
            .expect("transaction reached the sandbox");
        match r.into_result() {
            Ok(ok) => (
                Outcome::Returned(ok.json::<bool>().unwrap()),
                ok.total_gas_burnt.as_gas() as u128,
            ),
            Err(TransactionResultError::Failure(f)) => {
                println!("REASON {f}");
                (Outcome::Failed, f.total_gas_burnt.as_gas() as u128)
            }
            Err(e) => panic!("transaction did not finish: {e}"),
        }
    }

    async fn verified(&self) -> u64 {
        let v: near_api::Data<u64> = Contract(self.id.clone())
            .call_function("verified", json!({}))
            .read_only()
            .fetch_from(&self.net)
            .await
            .unwrap();
        v.data
    }
}

async fn deploy(
    net: &NetworkConfig,
    root: &AccountId,
    root_signer: &Arc<Signer>,
    name: &str,
    wasm: Vec<u8>,
    vk: &NearVk,
) -> Spike {
    let id: AccountId = format!("{name}.{root}").parse().unwrap();
    let sk = generate_secret_key().unwrap();
    Account::create_account(id.clone())
        .fund_myself(root.clone(), NearToken::from_near(20))
        .with_public_key(sk.public_key())
        .with_signer(root_signer.clone())
        .send_to(net)
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let signer = Signer::from_secret_key(sk).unwrap();
    Contract::deploy(id.clone())
        .use_code(wasm)
        .with_init_call(
            "new",
            json!({
                "alpha": B64.encode(&vk.alpha),
                "neg_beta": B64.encode(&vk.neg_beta),
                "neg_gamma": B64.encode(&vk.neg_gamma),
                "neg_delta": B64.encode(&vk.neg_delta),
                "ic": B64.encode(&vk.ic),
            }),
        )
        .unwrap()
        .with_signer(signer.clone())
        .send_to(net)
        .await
        .unwrap()
        .into_result()
        .unwrap();
    Spike {
        net: net.clone(),
        id,
        signer,
    }
}

/// A point on the G2 curve outside the prime-order subgroup.
fn g2_outside_subgroup() -> G2Affine {
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

/// Encoding of the scalar modulus plus `x`: the same value mod r, non-canonical.
fn fr_plus_modulus(x: &Fr) -> [u8; 32] {
    let mut big = x.into_bigint();
    big.add_with_carry(&Fr::MODULUS);
    big.to_bytes_le().try_into().unwrap()
}

#[tokio::test]
async fn groth16_through_near_host_functions() {
    let wasm = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../res/zk_spike.wasm"
    ))
    .expect("run scripts/build-contracts.sh first");

    let sandbox = Sandbox::start_sandbox().await.expect("start sandbox");
    let net = NetworkConfig::from_rpc_url("sandbox", sandbox.rpc_addr.parse().unwrap());
    let root: AccountId = DEFAULT_GENESIS_ACCOUNT.as_str().parse().unwrap();
    let root_signer =
        Signer::from_secret_key(DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY.parse().unwrap()).unwrap();

    let cfg = poseidon_config();
    let (pk, vk) = setup(ROUNDS, 7).unwrap();
    let spike = deploy(
        &net,
        &root,
        &root_signer,
        "spike",
        wasm.clone(),
        &near_vk(&vk),
    )
    .await;

    let inst = Instance::sample(&cfg, ROUNDS, 11);
    let proof: Proof<Bn254> = prove(&pk, ROUNDS, &inst, 13).unwrap();
    let good = proof_bytes(&proof);
    let inputs = inst.public_inputs();
    let good_inputs = inputs_bytes(&inputs);

    // The published test vectors are exactly what this test sends on chain.
    let vectors: serde_json::Value =
        serde_json::from_str(include_str!("../vectors/spike-groth16.json")).unwrap();
    assert_eq!(vectors["cases"][0]["proof"], B64.encode(&good));
    assert_eq!(vectors["cases"][0]["inputs"], B64.encode(&good_inputs));
    assert_eq!(vectors["verifying_key"]["ic"], B64.encode(near_vk(&vk).ic));

    // 1. A valid proof passes, and the gas is what we came to measure.
    let (out, gas) = spike.verify(&good, &good_inputs).await;
    assert_eq!(out, Outcome::Returned(true), "valid proof must verify");
    assert_eq!(spike.verified().await, 1);
    println!(
        "GAS verify, {NUM_PUBLIC} public inputs: {:.2} Tgas ({gas} gas)",
        gas as f64 / 1e12
    );

    // A second proof of a different statement also passes: nothing is hard-coded.
    let inst2 = Instance::sample(&cfg, ROUNDS, 12);
    let proof2 = prove(&pk, ROUNDS, &inst2, 14).unwrap();
    let (out, _) = spike
        .verify(&proof_bytes(&proof2), &inputs_bytes(&inst2.public_inputs()))
        .await;
    assert_eq!(out, Outcome::Returned(true));
    assert_eq!(spike.verified().await, 2);

    // Failures below must never count as verified.
    let expect_rejected = |what: &'static str, r: (Outcome, u128), want: Outcome| {
        println!("CASE {what}: {:?}, {:.2} Tgas", r.0, r.1 as f64 / 1e12);
        assert_eq!(r.0, want, "{what}");
    };

    // 2. Public inputs swapped, changed, or from another statement.
    let mut swapped = inputs.clone();
    swapped.swap(1, 2);
    assert_eq!(
        vectors["cases"][1]["inputs"],
        B64.encode(inputs_bytes(&swapped))
    );
    expect_rejected(
        "public inputs swapped",
        spike.verify(&good, &inputs_bytes(&swapped)).await,
        Outcome::Returned(false),
    );
    let mut changed = inputs.clone();
    changed[4] += Fr::from(1u64);
    expect_rejected(
        "public input changed by one",
        spike.verify(&good, &inputs_bytes(&changed)).await,
        Outcome::Returned(false),
    );
    expect_rejected(
        "proof paired with another statement's inputs",
        spike
            .verify(&good, &inputs_bytes(&inst2.public_inputs()))
            .await,
        Outcome::Returned(false),
    );

    // 3. Proof points that are valid curve points but the wrong ones.
    let doubled = Proof::<Bn254> {
        a: (proof.a + proof.a).into_affine(),
        b: proof.b,
        c: proof.c,
    };
    expect_rejected(
        "A replaced by 2A",
        spike.verify(&proof_bytes(&doubled), &good_inputs).await,
        Outcome::Returned(false),
    );
    let swapped_points = [&proof_bytes(&proof2)[..64], &good[64..192], &good[192..]].concat();
    expect_rejected(
        "A taken from another proof",
        spike.verify(&swapped_points, &good_inputs).await,
        Outcome::Returned(false),
    );

    // 4. A proof from a different setup does not satisfy this key.
    let (other_pk, _) = setup(ROUNDS, 8).unwrap();
    let foreign = prove(&other_pk, ROUNDS, &inst, 13).unwrap();
    expect_rejected(
        "proof from a different setup",
        spike.verify(&proof_bytes(&foreign), &good_inputs).await,
        Outcome::Returned(false),
    );

    // 5. Malformed encodings: the host function rejects, and the call fails.
    let mut flipped = good.clone();
    flipped[5] ^= 0x01;
    assert_eq!(vectors["cases"][2]["proof"], B64.encode(&flipped));
    expect_rejected(
        "byte flipped in A (off curve)",
        spike.verify(&flipped, &good_inputs).await,
        Outcome::Failed,
    );
    let mut flipped_b = good.clone();
    flipped_b[100] ^= 0x01;
    expect_rejected(
        "byte flipped in B (off curve)",
        spike.verify(&flipped_b, &good_inputs).await,
        Outcome::Failed,
    );
    let outside = [
        &good[..64],
        &g2_bytes(&g2_outside_subgroup())[..],
        &good[192..],
    ]
    .concat();
    expect_rejected(
        "B on the curve but outside the subgroup",
        spike.verify(&outside, &good_inputs).await,
        Outcome::Failed,
    );

    // 6. Input shape and canonical form, checked before any pairing.
    expect_rejected(
        "proof too short",
        spike.verify(&good[..255], &good_inputs).await,
        Outcome::Failed,
    );
    expect_rejected(
        "one public input missing",
        spike
            .verify(&good, &good_inputs[..good_inputs.len() - 32])
            .await,
        Outcome::Failed,
    );
    let mut aliased = good_inputs.clone();
    aliased[..32].copy_from_slice(&fr_plus_modulus(&inputs[0]));
    expect_rejected(
        "public input + modulus (same value mod r)",
        spike.verify(&good, &aliased).await,
        Outcome::Failed,
    );

    // 7. All-zero proof. Zero bytes decode as the point at infinity.
    expect_rejected(
        "all-zero proof",
        spike.verify(&[0u8; 256], &good_inputs).await,
        Outcome::Returned(false),
    );

    assert_eq!(spike.verified().await, 2, "no rejected case may count");

    // 8. Groth16 proofs are malleable: without the witness, anyone can turn a
    // valid proof into a different valid proof of the same statement. This
    // test documents the property, so that replay protection is never built on
    // proof bytes being unique. Option C binds replay to the state commitment.
    let r = Fr::from(5u64);
    let mauled = Proof::<Bn254> {
        a: (proof.a * r).into_affine(),
        b: (proof.b * r.inverse().unwrap()).into_affine(),
        c: proof.c,
    };
    assert_ne!(proof_bytes(&mauled), good);
    let (out, _) = spike.verify(&proof_bytes(&mauled), &good_inputs).await;
    assert_eq!(
        out,
        Outcome::Returned(true),
        "malleated proof still verifies"
    );
    assert_eq!(spike.verified().await, 3);
}
