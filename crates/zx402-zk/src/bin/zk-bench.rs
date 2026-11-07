//! Measures constraint count, setup, and proving time at several circuit sizes,
//! and writes reproducible test vectors. Run with `--profile prover`.
//!
//! `zk-bench bench`             prints measurements for the spike circuit
//! `zk-bench policy`            prints measurements for the Option C circuit
//! `zk-bench vectors <file>`    writes JSON test vectors from fixed seeds

use std::time::Instant;

use anyhow::{bail, Result};
use ark_bn254::Bn254;
use ark_groth16::Groth16;
use ark_snark::SNARK;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde_json::json;
use zx402_zk::*;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn bench() -> Result<()> {
    let cfg = poseidon_config();
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    println!("threads available: {threads} (arkworks built without the parallel feature)");
    println!("rounds | constraints | setup ms | prove ms median | prove ms worst | verify ms");
    for rounds in [1usize, 8, 32, 128] {
        let constraints = constraint_count(rounds)?;
        let t = Instant::now();
        let (pk, vk) = setup(rounds, 7)?;
        let setup_ms = ms(t);

        let runs = 7;
        let mut times = Vec::with_capacity(runs);
        let inst = Instance::sample(&cfg, rounds, 11);
        let mut last = None;
        for i in 0..runs {
            let t = Instant::now();
            last = Some(prove(&pk, rounds, &inst, 100 + i as u64)?);
            times.push(ms(t));
        }
        times.sort_by(f64::total_cmp);
        let t = Instant::now();
        let ok = Groth16::<Bn254>::verify(&vk, &inst.public_inputs(), &last.unwrap())?;
        if !ok {
            bail!("proof did not verify at {rounds} rounds");
        }
        println!(
            "{rounds:>6} | {constraints:>11} | {setup_ms:>8.0} | {:>15.1} | {:>14.1} | {:>9.1}",
            times[runs / 2],
            times[runs - 1],
            ms(t)
        );
    }
    Ok(())
}

/// Measures the real Option C policy circuit, separately from any network time.
fn bench_policy() -> Result<()> {
    use ark_std::rand::rngs::OsRng;
    use ark_std::UniformRand;
    use zx402_zk::policy::{self, PolicyOpening, SpendState, ZkWallet};
    let mut rng = OsRng;

    let t = Instant::now();
    let (pk, vk) = policy::setup(&mut rng)?;
    let setup_ms = ms(t);

    let wallet = ZkWallet {
        contract: "policy.near".into(),
        agent: "agent.near".into(),
        opening: PolicyOpening {
            cap: 1_000_000,
            per_call: 500_000,
            vendors: (0..256).map(|i| format!("vendor-{i}.near")).collect(),
            salt: ark_bn254::Fr::rand(&mut rng),
        },
        state: SpendState::fresh(&mut rng),
    };
    let runs = 21;
    let mut proving = Vec::with_capacity(runs);
    let mut witness = Vec::with_capacity(runs);
    let mut verifying = Vec::with_capacity(runs);
    let mut constraints = 0;
    for _ in 0..runs {
        let t = Instant::now();
        let (public, w, _) =
            wallet.statement("vendor-200.near", 50_000, "nonce", 20_000, &mut rng)?;
        witness.push(ms(t));
        let (count, ok) = policy::check(public, w.clone())?;
        if !ok {
            bail!("the honest statement does not satisfy the circuit");
        }
        constraints = count;
        let t = Instant::now();
        let proof = policy::prove(&pk, public, w, &mut rng)?;
        proving.push(ms(t));
        let t = Instant::now();
        if !Groth16::<Bn254>::verify(&vk, &public.to_inputs(), &proof)? {
            bail!("proof did not verify");
        }
        verifying.push(ms(t));
    }
    for v in [&mut proving, &mut witness, &mut verifying] {
        v.sort_by(f64::total_cmp);
    }
    println!(
        "constraints: {constraints}; allowlist full at 256 vendors; {runs} runs; single thread"
    );
    println!("setup: {setup_ms:.0} ms");
    let row = |name: &str, v: &[f64]| {
        println!(
            "{name}: median {:.1} ms, worst {:.1} ms",
            v[v.len() / 2],
            v[v.len() - 1]
        );
    };
    row("build statement (hashes, allowlist tree)", &witness);
    row("prove", &proving);
    row("native verify", &verifying);
    Ok(())
}

fn vectors(path: &str) -> Result<()> {
    let (rounds, setup_seed, inst_seed, proof_seed) = (8usize, 7u64, 11u64, 13u64);
    let cfg = poseidon_config();
    let (pk, vk) = setup(rounds, setup_seed)?;
    let inst = Instance::sample(&cfg, rounds, inst_seed);
    let proof = prove(&pk, rounds, &inst, proof_seed)?;
    let good = proof_bytes(&proof);
    let inputs = inst.public_inputs();
    let near = near_vk(&vk);

    let mut swapped = inputs.clone();
    swapped.swap(1, 2);
    let mut flipped = good.clone();
    flipped[5] ^= 0x01;

    let doc = json!({
        "warning": "Keys come from a public seed and offer NO security. For format testing only.",
        "circuit": { "rounds": rounds, "seeds": { "setup": setup_seed, "instance": inst_seed, "proof": proof_seed } },
        "encoding": "Field elements 32 bytes little-endian; G1 x||y; G2 x.c0||x.c1||y.c0||y.c1; proof A||B||C; base64",
        "verifying_key": {
            "alpha": B64.encode(&near.alpha),
            "neg_beta": B64.encode(&near.neg_beta),
            "neg_gamma": B64.encode(&near.neg_gamma),
            "neg_delta": B64.encode(&near.neg_delta),
            "ic": B64.encode(&near.ic),
        },
        "cases": [
            { "name": "valid", "proof": B64.encode(&good), "inputs": B64.encode(inputs_bytes(&inputs)), "expect": "true" },
            { "name": "public inputs swapped", "proof": B64.encode(&good), "inputs": B64.encode(inputs_bytes(&swapped)), "expect": "false" },
            { "name": "byte flipped in A", "proof": B64.encode(&flipped), "inputs": B64.encode(inputs_bytes(&inputs)), "expect": "host function error" },
        ],
    });
    std::fs::write(path, serde_json::to_string_pretty(&doc)? + "\n")?;
    println!("wrote {path}");
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("bench") => bench(),
        Some("policy") => bench_policy(),
        Some("vectors") if args.len() == 3 => vectors(&args[2]),
        _ => bail!("usage: zk-bench bench | zk-bench policy | zk-bench vectors <file>"),
    }
}
