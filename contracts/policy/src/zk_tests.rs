//! `pay_zk` with real Groth16 proofs, through NEAR's mocked VM. Block time is set
//! freely here, so these tests cover day changes and midnight, which a sandbox
//! cannot. Each test plays the agent, honest or cheating.

use std::sync::OnceLock;

use ark_std::rand::{rngs::StdRng, SeedableRng};
use ark_std::UniformRand;
use near_sdk::test_utils::{accounts, VMContextBuilder};
use near_sdk::{testing_env, PromiseResult};
use zx402_zk::policy::{self, PolicyOpening, SpendState, ZkWallet};
use zx402_zk::{ark_bn254, ark_groth16, fr_le, near_vk, proof_bytes};

use super::*;

type Pk = ark_groth16::ProvingKey<ark_bn254::Bn254>;
type Vk = ark_groth16::VerifyingKey<ark_bn254::Bn254>;
type Fr = ark_bn254::Fr;

const DAY: u64 = 20_000;
const CAP: u128 = 1_000_000;
const PER_CALL: u128 = 500_000;
const FUNDS: u128 = 10_000_000;

fn keys() -> &'static (Pk, Vk) {
    static KEYS: OnceLock<(Pk, Vk)> = OnceLock::new();
    KEYS.get_or_init(|| policy::setup(&mut StdRng::seed_from_u64(1)).unwrap())
}

fn owner() -> AccountId {
    accounts(0)
}
fn agent() -> AccountId {
    accounts(1)
}
fn token() -> AccountId {
    "usdc.near".parse().unwrap()
}
fn contract_id() -> AccountId {
    "policy.near".parse().unwrap()
}
const VENDOR: &str = "data-api.near";
const OTHER: &str = "gpu-cloud.near";
const ATTACKER: &str = "attacker.near";

fn at(caller: AccountId, day: u64) {
    testing_env!(VMContextBuilder::new()
        .current_account_id(contract_id())
        .predecessor_account_id(caller)
        .block_timestamp(day * NANOS_PER_DAY + 12 * 3_600_000_000_000)
        .prepaid_gas(Gas::from_tgas(300))
        .build());
}

fn bytes(fr: Fr) -> Base64VecU8 {
    fr_le(&fr).to_vec().into()
}

struct Fix {
    c: Contract,
    w: ZkWallet,
    rng: StdRng,
}

/// A wallet with $10, one ZK agent, and a verifying key set.
fn fix() -> Fix {
    let mut rng = StdRng::seed_from_u64(2);
    let w = ZkWallet {
        contract: contract_id().to_string(),
        agent: agent().to_string(),
        opening: PolicyOpening {
            cap: CAP,
            per_call: PER_CALL,
            vendors: vec![VENDOR.into(), OTHER.into()],
            salt: Fr::rand(&mut rng),
        },
        state: SpendState::fresh(&mut rng),
    };
    at(owner(), DAY);
    let mut c = Contract::new(owner(), token());
    let vk = near_vk(&keys().1);
    c.set_zk_verifying_key(
        vk.alpha.into(),
        vk.neg_beta.into(),
        vk.neg_gamma.into(),
        vk.neg_delta.into(),
        vk.ic.into(),
    );
    c.set_zk_agent(
        agent(),
        bytes(w.opening.commitment().unwrap()),
        bytes(w.state.commitment()),
        None,
    );
    at(token(), DAY);
    c.ft_on_transfer(owner(), U128(FUNDS), String::new());
    Fix { c, w, rng }
}

#[test]
fn verifying_key_view_returns_what_was_set() {
    at(owner(), DAY);
    let mut c = Contract::new(owner(), token());
    assert_eq!(c.get_zk_verifying_key(), None);

    let vk = near_vk(&keys().1);
    c.set_zk_verifying_key(
        vk.alpha.clone().into(),
        vk.neg_beta.clone().into(),
        vk.neg_gamma.clone().into(),
        vk.neg_delta.clone().into(),
        vk.ic.clone().into(),
    );
    let view = c.get_zk_verifying_key().unwrap();
    assert_eq!(view.alpha.0, vk.alpha);
    assert_eq!(view.neg_beta.0, vk.neg_beta);
    assert_eq!(view.neg_gamma.0, vk.neg_gamma);
    assert_eq!(view.neg_delta.0, vk.neg_delta);
    assert_eq!(view.ic.0, vk.ic);
}

/// What the agent sends. Tests alter it to cheat.
struct Call {
    vendor: String,
    amount: u128,
    memo: String,
    proof: Vec<u8>,
    new_state: Fr,
    old_state: Fr,
    policy: Fr,
    day: u64,
    next: SpendState,
}

impl Fix {
    /// Proves a statement, satisfied or not, for block day `day`.
    fn prove(&mut self, vendor: &str, amount: u128, memo: &str, day: u64) -> Call {
        let (public, witness, next) = self
            .w
            .statement(vendor, amount, memo, day, &mut self.rng)
            .unwrap();
        let proof = policy::prove(&keys().0, public, witness, &mut self.rng).unwrap();
        Call {
            vendor: vendor.into(),
            amount,
            memo: memo.into(),
            proof: proof_bytes(&proof),
            new_state: public.new_state,
            old_state: public.old_state,
            policy: public.policy_commitment,
            day,
            next,
        }
    }

    fn send(&mut self, call: &Call, block_day: u64) -> PromiseOrValue<PayOutcome> {
        at(agent(), block_day);
        self.c.pay_zk(
            call.vendor.parse().unwrap(),
            U128(call.amount),
            call.memo.clone(),
            call.proof.clone().into(),
            bytes(call.new_state),
            bytes(call.old_state),
            bytes(call.policy),
            U64(call.day),
        )
    }

    /// Settles the last log entry as the token would.
    fn settle(&mut self, ok: bool, day: u64) {
        let id = self.c.get_log_length() - 1;
        self.settle_id(id, ok, day);
    }

    fn settle_id(&mut self, id: u32, ok: bool, day: u64) {
        testing_env!(
            VMContextBuilder::new()
                .current_account_id(contract_id())
                .predecessor_account_id(contract_id())
                .block_timestamp(day * NANOS_PER_DAY)
                .prepaid_gas(Gas::from_tgas(100))
                .build(),
            near_sdk::test_vm_config(),
            near_sdk::RuntimeFeesConfig::test(),
            Default::default(),
            vec![if ok {
                PromiseResult::Successful(vec![])
            } else {
                PromiseResult::Failed
            }]
        );
        self.c.on_pay_transfer(id);
    }

    /// One honest payment, start to finish.
    fn pay_ok(&mut self, vendor: &str, amount: u128, day: u64) {
        let call = self.prove(vendor, amount, "nonce", day);
        assert!(blocked_reason(self.send(&call, day)).is_none());
        self.settle(true, day);
        self.w.state = call.next;
    }

    fn view(&self) -> ZkAgentView {
        self.c.get_zk_agent(agent()).unwrap()
    }

    fn balance(&self) -> u128 {
        self.c.get_balance().0
    }

    fn state_is(&self, fr: Fr) -> bool {
        self.view().state_commitment.0 == fr_le(&fr).to_vec()
    }
}

fn blocked_reason(p: PromiseOrValue<PayOutcome>) -> Option<String> {
    match p {
        PromiseOrValue::Value(o) => {
            assert_eq!(o.status, Status::Blocked);
            o.reason
        }
        PromiseOrValue::Promise(_) => None,
    }
}

/// Sends a call that must panic, and returns the panic message.
fn panic_text(f: &mut Fix, call: &Call, block_day: u64) -> String {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f.send(call, block_day)));
    let Err(payload) = result else {
        panic!("the call should have panicked");
    };
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// The attempt was logged as an invalid proof, and moved nothing.
fn assert_rejected(f: &mut Fix, call: &Call, block_day: u64) {
    let balance = f.balance();
    let state = f.view().state_commitment;
    let blocked = f.view().blocked_today;
    assert_eq!(
        blocked_reason(f.send(call, block_day)).as_deref(),
        Some("invalid proof")
    );
    assert_eq!(f.balance(), balance, "no funds may move");
    assert_eq!(f.view().state_commitment, state, "state must not change");
    assert_eq!(f.view().blocked_today, blocked + 1);
    assert_eq!(f.view().pending_log_id, None);
}

#[test]
fn honest_payment_transfers_and_advances_state() {
    let mut f = fix();
    let call = f.prove(VENDOR, 100_000, "nonce-1", DAY);
    assert!(blocked_reason(f.send(&call, DAY)).is_none());
    assert_eq!(f.balance(), FUNDS - 100_000);
    assert!(f.state_is(call.new_state));
    assert_eq!(f.c.get_log_entry(0).unwrap().status, Status::Pending);
    assert_eq!(f.view().pending_log_id, Some(0));

    f.settle(true, DAY);
    let entry = f.c.get_log_entry(0).unwrap();
    assert_eq!(entry.status, Status::Paid);
    assert_eq!(entry.memo.as_deref(), Some("nonce-1"));
    assert_eq!(f.view().pending_log_id, None);
    assert!(f.state_is(call.new_state));
}

#[test]
fn caps_and_allowlist_stay_off_chain() {
    let f = fix();
    assert!(f.c.get_agent(agent()).is_none());
    assert!(f.c.get_agents(None, None).is_empty());
    assert!(f.c.get_vendors().is_empty());
    let listed = f.c.get_zk_agents(None, None);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].agent_id, agent());
}

#[test]
fn unapproved_vendor_is_rejected() {
    let mut f = fix();
    let call = f.prove(ATTACKER, 100_000, "n", DAY);
    assert_rejected(&mut f, &call, DAY);
}

#[test]
fn proof_for_one_vendor_cannot_pay_another() {
    let mut f = fix();
    let mut call = f.prove(VENDOR, 100_000, "n", DAY);
    call.vendor = ATTACKER.into();
    assert_rejected(&mut f, &call, DAY);
    // Not even another allowlisted one.
    call.vendor = OTHER.into();
    assert_rejected(&mut f, &call, DAY);
}

#[test]
fn proof_for_one_amount_cannot_transfer_another() {
    let mut f = fix();
    let mut call = f.prove(VENDOR, 100_000, "n", DAY);
    call.amount = 100_001;
    assert_rejected(&mut f, &call, DAY);
}

#[test]
fn proof_for_one_invoice_cannot_settle_another() {
    let mut f = fix();
    let mut call = f.prove(VENDOR, 100_000, "invoice-a", DAY);
    call.memo = "invoice-b".into();
    assert_rejected(&mut f, &call, DAY);
}

#[test]
fn amount_over_the_private_per_call_limit_is_rejected() {
    let mut f = fix();
    let call = f.prove(VENDOR, PER_CALL + 1, "n", DAY);
    assert_rejected(&mut f, &call, DAY);
}

#[test]
fn cumulative_overspend_is_rejected() {
    let mut f = fix();
    f.pay_ok(VENDOR, PER_CALL, DAY);
    f.pay_ok(OTHER, PER_CALL, DAY);
    let call = f.prove(VENDOR, 1, "n", DAY);
    assert_rejected(&mut f, &call, DAY);
    assert_eq!(f.balance(), FUNDS - CAP);
}

#[test]
fn proof_for_a_fabricated_future_day_does_not_reset_the_cap() {
    let mut f = fix();
    f.pay_ok(VENDOR, PER_CALL, DAY);
    f.pay_ok(OTHER, PER_CALL, DAY);

    // The cap is used up. A cheating agent proves for tomorrow, when the budget
    // would be fresh. Claiming tomorrow is simply refused as out of date...
    let forged = f.prove(VENDOR, PER_CALL, "n", DAY + 1);
    assert!(panic_text(&mut f, &forged, DAY).contains("stale proof: the day has changed"));

    // ...and naming the real day with tomorrow's proof fails verification.
    let mut forged = f.prove(VENDOR, PER_CALL, "n", DAY + 1);
    forged.day = DAY;
    assert_rejected(&mut f, &forged, DAY);
    assert_eq!(f.balance(), FUNDS - CAP);
}

#[test]
fn proof_for_a_past_day_is_rejected() {
    let mut f = fix();
    f.pay_ok(VENDOR, PER_CALL, DAY);
    let mut forged = f.prove(VENDOR, PER_CALL, "n", DAY - 1);
    forged.day = DAY;
    assert_rejected(&mut f, &forged, DAY);
}

#[test]
fn the_budget_resets_only_when_block_time_reaches_the_next_day() {
    let mut f = fix();
    f.pay_ok(VENDOR, PER_CALL, DAY);
    f.pay_ok(OTHER, PER_CALL, DAY);
    f.pay_ok(VENDOR, PER_CALL, DAY + 1);
    assert_eq!(f.balance(), FUNDS - CAP - PER_CALL);
}

#[test]
#[should_panic(expected = "stale proof: the day has changed")]
fn proof_made_before_midnight_must_be_redone_after_it() {
    let mut f = fix();
    let call = f.prove(VENDOR, 100_000, "n", DAY);
    f.send(&call, DAY + 1);
}

#[test]
#[should_panic(expected = "stale proof: the state has changed")]
fn same_proof_cannot_be_replayed() {
    let mut f = fix();
    let call = f.prove(VENDOR, 100_000, "n", DAY);
    let replay = Call {
        vendor: call.vendor.clone(),
        memo: call.memo.clone(),
        proof: call.proof.clone(),
        ..call
    };
    f.send(&call, DAY);
    f.settle(true, DAY);
    f.send(&replay, DAY);
}

#[test]
fn rolled_back_state_fails_verification() {
    // An agent naming the current state as expected, with a proof from before
    // the last payment, cannot pass.
    let mut f = fix();
    let mut old = f.prove(VENDOR, 100_000, "n", DAY);
    f.pay_ok(VENDOR, 100_000, DAY);
    old.old_state = f.w.state.commitment();
    assert_rejected(&mut f, &old, DAY);
}

#[test]
fn two_proofs_from_one_state_cannot_both_spend_it() {
    let mut f = fix();
    // Half the cap is left, so either payment alone fits and both together do not.
    f.pay_ok(VENDOR, PER_CALL, DAY);
    let first = f.prove(VENDOR, PER_CALL, "n1", DAY);
    let mut second = f.prove(OTHER, PER_CALL, "n2", DAY);

    assert!(blocked_reason(f.send(&first, DAY)).is_none());
    f.settle(true, DAY);
    f.w.state = first.next;
    assert_eq!(f.balance(), FUNDS - CAP);

    // Honest about the state it was built on: out of date.
    let text = panic_text(&mut f, &second, DAY);
    assert!(
        text.contains("stale proof: the state has changed"),
        "{text}"
    );
    // Lying about it: the contract supplies the real state, so the proof fails.
    second.old_state = f.w.state.commitment();
    assert_rejected(&mut f, &second, DAY);
    assert_eq!(f.balance(), FUNDS - CAP, "the cap holds");
}

#[test]
fn two_agents_cannot_spend_the_same_balance() {
    let mut f = fix();
    f.c.balance = 600_000;
    let other_agent = accounts(2);
    let other = ZkWallet {
        contract: contract_id().to_string(),
        agent: other_agent.to_string(),
        opening: PolicyOpening {
            cap: CAP,
            per_call: PER_CALL,
            vendors: vec![VENDOR.into()],
            salt: Fr::rand(&mut f.rng),
        },
        state: SpendState::fresh(&mut f.rng),
    };
    at(owner(), DAY);
    f.c.set_zk_agent(
        other_agent.clone(),
        bytes(other.opening.commitment().unwrap()),
        bytes(other.state.commitment()),
        None,
    );

    // Each is within its own policy. The first is still in flight.
    let first = f.prove(VENDOR, 400_000, "n1", DAY);
    assert!(blocked_reason(f.send(&first, DAY)).is_none());
    assert_eq!(f.balance(), 200_000);

    let (public, witness, _) = other
        .statement(VENDOR, 400_000, "n2", DAY, &mut f.rng)
        .unwrap();
    let proof = policy::prove(&keys().0, public, witness, &mut f.rng).unwrap();
    at(other_agent.clone(), DAY);
    let result = f.c.pay_zk(
        VENDOR.parse().unwrap(),
        U128(400_000),
        "n2".into(),
        proof_bytes(&proof).into(),
        bytes(public.new_state),
        bytes(public.old_state),
        bytes(public.policy_commitment),
        U64(DAY),
    );
    assert_eq!(
        blocked_reason(result).as_deref(),
        Some("insufficient wallet balance")
    );
    assert_eq!(f.balance(), 200_000);
    f.settle_id(0, true, DAY);
    assert_eq!(f.balance(), 200_000);
}

#[test]
#[should_panic(expected = "insufficient balance")]
fn finance_cannot_withdraw_what_a_payment_has_reserved() {
    let mut f = fix();
    let call = f.prove(VENDOR, 100_000, "n", DAY);
    f.send(&call, DAY);
    at(owner(), DAY);
    let _ = f.c.withdraw(U128(FUNDS));
}

#[test]
fn identity_points_do_not_make_a_proof_pass() {
    // Replacing any of A, B, and C by the identity turns its pairing into 1, the
    // shortcut a verifier with a sign or order mistake would accept.
    let mut f = fix();
    // Enough room for every case: this is about the verifier, not the breaker.
    at(owner(), DAY);
    f.c.set_zk_agent(
        agent(),
        bytes(f.w.opening.commitment().unwrap()),
        bytes(f.w.state.commitment()),
        Some(100),
    );
    for vendor in [VENDOR, ATTACKER] {
        let mut call = f.prove(vendor, 100_000, "n", DAY);
        let real = call.proof.clone();
        for mask in 1..8u8 {
            call.proof = real.clone();
            if mask & 1 != 0 {
                call.proof[..64].fill(0);
            }
            if mask & 2 != 0 {
                call.proof[64..192].fill(0);
            }
            if mask & 4 != 0 {
                call.proof[192..].fill(0);
            }
            assert_rejected(&mut f, &call, DAY);
        }
    }
}

#[test]
fn both_sides_derive_the_same_public_inputs() {
    at(owner(), DAY);
    let long = "m".repeat(128);
    let strings = ["", "a", "data-api.near", "née-multibyte", long.as_str()];
    for x in strings {
        for y in strings {
            assert_eq!(
                crate::zk::context_field(x, y).to_vec(),
                fr_le(&policy::context_field(x, y)).to_vec(),
                "context {x} {y}"
            );
        }
        assert_eq!(
            crate::zk::vendor_field(x).to_vec(),
            fr_le(&policy::vendor_field(x)).to_vec(),
            "vendor {x}"
        );
        assert_eq!(
            crate::zk::invoice_field(x).to_vec(),
            fr_le(&policy::invoice_field(x)).to_vec(),
            "invoice {x}"
        );
        // Truncated to 248 bits, so it is always canonical.
        assert_eq!(crate::zk::vendor_field(x)[31], 0);
    }
    for x in [0, 1, u64::MAX] {
        assert_eq!(
            crate::zk::fr_from_u64(x).to_vec(),
            fr_le(&Fr::from(x)).to_vec()
        );
    }
    for x in [0, 1, u128::MAX] {
        assert_eq!(
            crate::zk::fr_from_u128(x).to_vec(),
            fr_le(&Fr::from(x)).to_vec()
        );
    }

    // Length prefixes keep part boundaries apart, and tags keep kinds apart.
    assert_ne!(
        crate::zk::context_field("ab", "c"),
        crate::zk::context_field("a", "bc")
    );
    assert_ne!(
        crate::zk::vendor_field("same"),
        crate::zk::invoice_field("same")
    );
}

#[test]
#[should_panic(expected = "a payment is already in flight")]
fn overlapping_payments_are_refused() {
    let mut f = fix();
    let first = f.prove(VENDOR, 100_000, "n1", DAY);
    let second = f.prove(VENDOR, 100_000, "n2", DAY);
    f.send(&first, DAY);
    f.send(&second, DAY);
}

#[test]
fn failed_transfer_restores_budget_state_and_funds() {
    let mut f = fix();
    let call = f.prove(VENDOR, 100_000, "n", DAY);
    f.send(&call, DAY);
    assert_eq!(f.balance(), FUNDS - 100_000);
    f.settle(false, DAY);

    let entry = f.c.get_log_entry(0).unwrap();
    assert_eq!(entry.status, Status::Failed);
    assert_eq!(f.balance(), FUNDS);
    assert!(f.state_is(call.old_state), "state must roll back");
    assert_eq!(f.view().pending_log_id, None);

    // The wallet kept its old state, so it can pay again.
    f.pay_ok(VENDOR, 100_000, DAY);
}

#[test]
fn rotated_agent_is_not_rolled_back_by_a_late_failure() {
    let mut f = fix();
    let call = f.prove(VENDOR, 100_000, "n", DAY);
    f.send(&call, DAY);

    // Finance rotates while the transfer is in flight.
    let replacement = SpendState::fresh(&mut f.rng);
    at(owner(), DAY);
    f.c.set_zk_agent(
        agent(),
        bytes(f.w.opening.commitment().unwrap()),
        bytes(replacement.commitment()),
        None,
    );
    f.settle(false, DAY);
    assert!(f.state_is(replacement.commitment()));
    assert_eq!(f.balance(), FUNDS, "the failed transfer is still refunded");
}

#[test]
fn late_failure_does_not_disturb_a_newer_payment() {
    let mut f = fix();
    let first = f.prove(VENDOR, 100_000, "n1", DAY);
    f.send(&first, DAY);

    // Finance rotates, and the agent starts a new payment from the new state.
    f.w.state = SpendState::fresh(&mut f.rng);
    at(owner(), DAY);
    f.c.set_zk_agent(
        agent(),
        bytes(f.w.opening.commitment().unwrap()),
        bytes(f.w.state.commitment()),
        None,
    );
    let second = f.prove(VENDOR, 100_000, "n2", DAY);
    f.send(&second, DAY);

    // The first transfer now fails. Only its funds come back.
    f.settle_id(0, false, DAY);
    assert!(f.state_is(second.new_state));
    assert_eq!(f.view().pending_log_id, Some(1));
    assert_eq!(f.balance(), FUNDS - 100_000);
}

#[test]
#[should_panic(expected = "stale proof: the policy has changed")]
fn policy_rotation_invalidates_outstanding_proofs() {
    let mut f = fix();
    let call = f.prove(VENDOR, 100_000, "n", DAY);
    let stricter = PolicyOpening {
        cap: CAP / 2,
        per_call: PER_CALL / 2,
        vendors: vec![VENDOR.into()],
        salt: Fr::rand(&mut f.rng),
    };
    at(owner(), DAY);
    f.c.set_zk_agent(
        agent(),
        bytes(stricter.commitment().unwrap()),
        bytes(f.w.state.commitment()),
        None,
    );
    f.send(&call, DAY);
}

#[test]
fn old_policy_fails_even_when_named_as_current() {
    let mut f = fix();
    let mut call = f.prove(VENDOR, PER_CALL, "n", DAY);
    let stricter = PolicyOpening {
        cap: CAP / 2,
        per_call: PER_CALL / 2,
        vendors: vec![VENDOR.into()],
        salt: Fr::rand(&mut f.rng),
    };
    let commitment = stricter.commitment().unwrap();
    at(owner(), DAY);
    f.c.set_zk_agent(
        agent(),
        bytes(commitment),
        bytes(f.w.state.commitment()),
        None,
    );
    call.policy = commitment;
    assert_rejected(&mut f, &call, DAY);
}

#[test]
fn proof_for_another_agent_is_rejected() {
    // The contract computes context from the caller, so a proof made for
    // agent A does not verify when agent B sends it.
    let mut f = fix();
    f.w.agent = "someone-else.near".into();
    let call = f.prove(VENDOR, 100_000, "n", DAY);
    assert_rejected(&mut f, &call, DAY);
}

#[test]
fn proof_for_another_contract_is_rejected() {
    let mut f = fix();
    f.w.contract = "other-policy.near".into();
    let call = f.prove(VENDOR, 100_000, "n", DAY);
    assert_rejected(&mut f, &call, DAY);
}

#[test]
fn proof_cannot_move_to_a_contract_at_another_address() {
    // The proof is for policy.near. The same code and state at another address
    // computes a different context from its own id, so verification fails.
    let mut f = fix();
    let call = f.prove(VENDOR, 100_000, "n", DAY);
    testing_env!(VMContextBuilder::new()
        .current_account_id("other-policy.near".parse().unwrap())
        .predecessor_account_id(agent())
        .block_timestamp(DAY * NANOS_PER_DAY)
        .prepaid_gas(Gas::from_tgas(300))
        .build());
    let outcome = f.c.pay_zk(
        call.vendor.parse().unwrap(),
        U128(call.amount),
        call.memo.clone(),
        call.proof.clone().into(),
        bytes(call.new_state),
        bytes(call.old_state),
        bytes(call.policy),
        U64(call.day),
    );
    assert_eq!(blocked_reason(outcome).as_deref(), Some("invalid proof"));
    assert_eq!(f.balance(), FUNDS);
}

#[test]
fn failed_transfer_is_not_counted_by_the_breaker() {
    // A vendor's token problem is a payment failure, not an attack.
    let mut f = fix();
    for _ in 0..6 {
        let call = f.prove(VENDOR, 100_000, "n", DAY);
        f.send(&call, DAY);
        f.settle(false, DAY);
    }
    assert_eq!(f.view().blocked_today, 0);
    assert!(!f.view().frozen);
    assert_eq!(f.balance(), FUNDS);
}

#[test]
fn insufficient_funds_count_toward_the_breaker() {
    // Same as `pay`: a wallet left empty freezes its agent after 5 attempts.
    let mut f = fix();
    at(owner(), DAY);
    f.c.withdraw(U128(FUNDS));
    for i in 1..=5 {
        let call = f.prove(VENDOR, 100_000, "n", DAY);
        assert_eq!(
            blocked_reason(f.send(&call, DAY)).as_deref(),
            Some("insufficient wallet balance")
        );
        assert_eq!(f.view().blocked_today, i);
    }
    assert!(f.view().frozen);
}

#[test]
fn invalid_proofs_trip_the_breaker_and_then_write_nothing() {
    let mut f = fix();
    for i in 1..=5 {
        let call = f.prove(ATTACKER, 100_000, "n", DAY);
        assert_rejected(&mut f, &call, DAY);
        assert_eq!(f.view().frozen, i == 5);
    }
    let len = f.c.get_log_length();
    let call = f.prove(VENDOR, 100_000, "n", DAY);
    assert!(panic_text(&mut f, &call, DAY).contains("too many blocked attempts"));
    assert_eq!(f.c.get_log_length(), len);
}

#[test]
fn frozen_agent_cannot_pay_even_with_a_valid_proof() {
    let mut f = fix();
    at(owner(), DAY);
    f.c.set_zk_frozen(agent(), true);
    let call = f.prove(VENDOR, 100_000, "n", DAY);
    assert_eq!(
        blocked_reason(f.send(&call, DAY)).as_deref(),
        Some("agent is frozen")
    );
    assert_eq!(f.balance(), FUNDS);

    at(owner(), DAY);
    f.c.set_zk_frozen(agent(), false);
    assert_eq!(f.view().blocked_today, 0);
    assert!(blocked_reason(f.send(&call, DAY)).is_none());
}

#[test]
fn insufficient_funds_block_a_valid_proof() {
    let mut f = fix();
    at(owner(), DAY);
    // Drain almost everything through a withdrawal.
    f.c.withdraw(U128(FUNDS - 1));
    let call = f.prove(VENDOR, 100_000, "n", DAY);
    assert_eq!(
        blocked_reason(f.send(&call, DAY)).as_deref(),
        Some("insufficient wallet balance")
    );
    assert!(f.state_is(call.old_state));
}

#[test]
#[should_panic(expected = "new_state is not canonical")]
fn non_canonical_new_state_is_refused() {
    let mut f = fix();
    let call = f.prove(VENDOR, 100_000, "n", DAY);
    at(agent(), DAY);
    f.c.pay_zk(
        VENDOR.parse().unwrap(),
        U128(100_000),
        "n".into(),
        call.proof.into(),
        vec![0xffu8; 32].into(),
        bytes(call.old_state),
        bytes(call.policy),
        U64(DAY),
    );
}

#[test]
#[should_panic(expected = "caller is not a registered ZK agent")]
fn outsider_cannot_call_pay_zk() {
    let mut f = fix();
    let call = f.prove(VENDOR, 100_000, "n", DAY);
    at(accounts(3), DAY);
    f.c.pay_zk(
        VENDOR.parse().unwrap(),
        U128(100_000),
        "n".into(),
        call.proof.into(),
        bytes(call.new_state),
        bytes(call.old_state),
        bytes(call.policy),
        U64(DAY),
    );
}

#[test]
#[should_panic(expected = "amount must be positive")]
fn zero_amount_is_refused() {
    let mut f = fix();
    let call = f.prove(VENDOR, 0, "n", DAY);
    f.send(&call, DAY);
}

#[test]
#[should_panic(expected = "owner only")]
fn agent_cannot_reset_its_own_state() {
    let mut f = fix();
    at(agent(), DAY);
    let state = bytes(SpendState::fresh(&mut f.rng).commitment());
    let policy = bytes(f.w.opening.commitment().unwrap());
    f.c.set_zk_agent(agent(), policy, state, None);
}

#[test]
#[should_panic(expected = "owner only")]
fn agent_cannot_replace_the_verifying_key() {
    let mut f = fix();
    at(agent(), DAY);
    let vk = near_vk(&keys().1);
    f.c.set_zk_verifying_key(
        vk.alpha.into(),
        vk.neg_beta.into(),
        vk.neg_gamma.into(),
        vk.neg_delta.into(),
        vk.ic.into(),
    );
}

#[test]
#[should_panic(expected = "memo must be 1 to 128 bytes")]
fn empty_memo_is_refused() {
    // The memo is the invoice, so a payment must always name one.
    let mut f = fix();
    let call = f.prove(VENDOR, 100_000, "", DAY);
    f.send(&call, DAY);
}

#[test]
#[should_panic(expected = "memo must be 1 to 128 bytes")]
fn oversized_memo_is_refused() {
    let mut f = fix();
    let call = f.prove(VENDOR, 100_000, &"x".repeat(129), DAY);
    f.send(&call, DAY);
}

#[test]
#[should_panic(expected = "agent is registered for pay_zk")]
fn a_zk_agent_cannot_also_get_a_public_budget() {
    let mut f = fix();
    at(owner(), DAY);
    f.c.set_agent(agent(), U128(CAP), U128(PER_CALL), None);
}

impl Fix {
    /// Finance says the tokens of entry `id` moved.
    fn reconcile_moved(&mut self, id: u32, day: u64) {
        at(owner(), day);
        self.c.reconcile_payment(id, true);
    }

    /// Finance says the tokens of entry `id` did not move, and the token agrees.
    fn reconcile_unmoved(&mut self, id: u32, day: u64) {
        at(owner(), day);
        let seq = self.c.settle_seq;
        self.c.reconcile_payment(id, false);
        let held = U128(self.c.balance + self.c.reserved);
        testing_env!(
            VMContextBuilder::new()
                .current_account_id(contract_id())
                .predecessor_account_id(contract_id())
                .block_timestamp(day * NANOS_PER_DAY)
                .prepaid_gas(Gas::from_tgas(100))
                .build(),
            near_sdk::test_vm_config(),
            near_sdk::RuntimeFeesConfig::test(),
            Default::default(),
            vec![PromiseResult::Successful(
                near_sdk::serde_json::to_vec(&held).unwrap()
            )]
        );
        self.c.on_reconcile_unmoved(id, seq);
    }
}

#[test]
fn reconcile_of_moved_tokens_frees_the_agent_and_keeps_its_state() {
    let mut f = fix();
    let call = f.prove(VENDOR, 100_000, "n", DAY);
    f.send(&call, DAY);
    // The callback never runs, so the agent cannot pay.
    assert_eq!(f.view().pending_log_id, Some(0));

    f.reconcile_moved(0, DAY + 1);

    assert_eq!(f.view().pending_log_id, None);
    assert!(f.state_is(call.new_state));
    assert_eq!(f.balance(), FUNDS - 100_000);
    assert_eq!(f.c.reserved, 0);
    f.w.state = call.next;
    f.pay_ok(VENDOR, 100_000, DAY + 1);
}

#[test]
fn reconcile_of_unmoved_tokens_returns_funds_without_rolling_the_state_back() {
    let mut f = fix();
    let call = f.prove(VENDOR, 100_000, "n", DAY);
    f.send(&call, DAY);

    f.reconcile_unmoved(0, DAY + 1);

    assert_eq!(f.view().pending_log_id, None);
    assert!(
        f.state_is(call.new_state),
        "a claim must not refund the budget"
    );
    assert_eq!(f.balance(), FUNDS);
    assert_eq!(f.c.reserved, 0);
    f.w.state = call.next;
    f.pay_ok(VENDOR, 100_000, DAY + 1);
}

#[test]
fn reconcile_leaves_a_rotated_agent_alone() {
    for moved in [true, false] {
        let mut f = fix();
        let call = f.prove(VENDOR, 100_000, "n", DAY);
        f.send(&call, DAY);
        let replacement = SpendState::fresh(&mut f.rng);
        at(owner(), DAY);
        f.c.set_zk_agent(
            agent(),
            bytes(f.w.opening.commitment().unwrap()),
            bytes(replacement.commitment()),
            None,
        );
        if moved {
            f.reconcile_moved(0, DAY + 1);
        } else {
            f.reconcile_unmoved(0, DAY + 1);
        }
        assert!(f.state_is(replacement.commitment()));
        assert_eq!(f.c.reserved, 0);
    }
}

#[test]
fn reconcile_does_not_disturb_a_newer_payment() {
    for moved in [true, false] {
        let mut f = fix();
        let first = f.prove(VENDOR, 100_000, "n1", DAY);
        f.send(&first, DAY);

        f.w.state = SpendState::fresh(&mut f.rng);
        at(owner(), DAY);
        f.c.set_zk_agent(
            agent(),
            bytes(f.w.opening.commitment().unwrap()),
            bytes(f.w.state.commitment()),
            None,
        );
        let second = f.prove(VENDOR, 100_000, "n2", DAY);
        f.send(&second, DAY);

        if moved {
            f.reconcile_moved(0, DAY + 1);
        } else {
            f.reconcile_unmoved(0, DAY + 1);
        }
        assert!(f.state_is(second.new_state));
        assert_eq!(f.view().pending_log_id, Some(1), "the newer marker stays");
        assert_eq!(f.c.reserved, 100_000);
    }
}
