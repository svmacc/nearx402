//! Spend policy wallet for AI agents paying x402 APIs, settled on NEAR.
//!
//! The contract holds a NEP-141 token (USDC). The owner registers agents (daily
//! cap, per-call max) and approves vendor accounts. An agent calls `pay`; if the
//! payment is within policy the contract itself transfers the tokens to the
//! vendor, so the agent never holds funds. Every call is logged, including
//! blocked attempts: off-policy calls return a `Blocked` outcome instead of
//! panicking, because a panic would roll back the log entry.

use near_sdk::json_types::{Base64VecU8, U128, U64};
use near_sdk::serde_json::json;
use near_sdk::store::{IterableMap, IterableSet, LazyOption, Vector};
use near_sdk::{
    env, ext_contract, near, require, AccountId, BorshStorageKey, Gas, NearToken, PanicOnDefault,
    Promise, PromiseError, PromiseOrValue, Timestamp,
};

mod zk;
use zk::VerifyingKey;

const NANOS_PER_DAY: u64 = 86_400 * 1_000_000_000;
const MAX_MEMO_LEN: usize = 128;
const MAX_PAGE: u32 = 100;
const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);
const GAS_FOR_FT_TRANSFER: Gas = Gas::from_tgas(15);
const GAS_FOR_CALLBACK: Gas = Gas::from_tgas(15);
const GAS_FOR_BALANCE_OF: Gas = Gas::from_tgas(10);
const DEFAULT_MAX_BLOCKED_PER_DAY: u32 = 5;
/// How old a `Pending` entry must be before the owner can reconcile it, so that
/// a slow transfer cannot still execute after the decision. NEAR gives no hard
/// bound on receipt delay, so this is a chosen margin.
const RECONCILE_AFTER: u64 = NANOS_PER_DAY;
/// Gas an agent must attach to `pay` so the transfer and its callback can run.
pub const GAS_FOR_PAY: Gas = Gas::from_tgas(50);
/// As `GAS_FOR_PAY`, plus proof verification (about 35 Tgas).
pub const GAS_FOR_PAY_ZK: Gas = Gas::from_tgas(90);

#[ext_contract(ext_ft)]
#[allow(dead_code)]
trait FungibleTokenCore {
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>);
    fn ft_balance_of(&self, account_id: AccountId) -> U128;
}

#[ext_contract(ext_self)]
#[allow(dead_code)]
trait SelfCallbacks {
    fn on_pay_transfer(&mut self, id: u32) -> PayOutcome;
    fn on_withdraw(&mut self, amount: U128);
    fn on_sync_balance(&mut self, seq: u64, max_amount: U128) -> U128;
    fn on_reconcile_unmoved(&mut self, id: u32, seq: u64) -> PayOutcome;
}

#[near(serializers = [borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    Agents,
    Vendors,
    Log,
    ZkAgents,
    ZkVerifyingKey,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct AgentPolicy {
    /// Maximum total spend per UTC day, in token base units (USDC: 6 decimals).
    pub daily_cap: U128,
    /// Maximum amount of a single payment, in token base units.
    pub per_call_max: U128,
    pub frozen: bool,
    /// Circuit breaker: the agent is frozen by its N-th blocked attempt of a
    /// UTC day. Further attempts that day are rejected without a log entry,
    /// so a compromised agent cannot flood the audit log.
    pub max_blocked_per_day: u32,
}

#[near(serializers = [borsh])]
struct AgentState {
    policy: AgentPolicy,
    /// UTC day number that `spent_today` and `blocked_today` refer to.
    day: u64,
    spent_today: u128,
    blocked_today: u32,
}

#[near(serializers = [json])]
#[derive(Debug)]
pub struct AgentView {
    pub agent_id: AccountId,
    pub policy: AgentPolicy,
    pub spent_today: U128,
    pub blocked_today: u32,
}

/// A transfer in flight for a ZK agent. Its state commitment has already moved
/// on, and a failed transfer must put it back. While this is set the state
/// cannot change except through `set_zk_agent`, which also clears this.
#[near(serializers = [borsh])]
struct ZkPending {
    log_id: u32,
    old_state: zk::Fr,
}

/// An agent whose limits and allowlist are private: the contract holds only
/// commitments, and the agent proves each payment against them.
#[near(serializers = [borsh])]
struct ZkAgent {
    policy_commitment: zk::Fr,
    state_commitment: zk::Fr,
    frozen: bool,
    max_blocked_per_day: u32,
    /// UTC day number that `blocked_today` refers to.
    blocked_day: u64,
    blocked_today: u32,
    pending: Option<ZkPending>,
}

#[near(serializers = [json])]
#[derive(Debug)]
pub struct ZkAgentView {
    pub agent_id: AccountId,
    pub policy_commitment: Base64VecU8,
    pub state_commitment: Base64VecU8,
    pub frozen: bool,
    pub max_blocked_per_day: u32,
    pub blocked_today: u32,
    /// Log entry of the transfer in flight, if any.
    pub pending_log_id: Option<u32>,
}

/// The verifying key in the same form `set_zk_verifying_key` takes it, so anyone
/// can check which setup the contract trusts.
#[near(serializers = [json])]
#[derive(Debug, PartialEq)]
pub struct ZkVerifyingKeyView {
    pub alpha: Base64VecU8,
    pub neg_beta: Base64VecU8,
    pub neg_gamma: Base64VecU8,
    pub neg_delta: Base64VecU8,
    pub ic: Base64VecU8,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
    /// Within policy; the token transfer is in flight.
    Pending,
    /// The vendor received the tokens. If `reason` says the owner reconciled the
    /// entry, this is the owner's claim, not a receipt result.
    Paid,
    /// The token transfer failed; budget and balance were restored. If `reason`
    /// says the owner reconciled the entry, this is the owner's claim, and only
    /// the balance was restored.
    Failed,
    /// Rejected by policy; nothing was transferred.
    Blocked,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct LogEntry {
    pub id: u32,
    pub timestamp: Timestamp,
    pub agent_id: AccountId,
    pub vendor: AccountId,
    pub amount: U128,
    pub status: Status,
    /// Why the payment was blocked or failed, or that the owner reconciled it;
    /// `None` otherwise.
    pub reason: Option<String>,
    /// Free text from the agent. x402 clients put the payment nonce here.
    pub memo: Option<String>,
    /// Block height at which the transfer settled.
    pub settled_at: Option<u64>,
}

#[near(serializers = [json])]
#[derive(Debug)]
pub struct PayOutcome {
    pub log_id: u32,
    pub status: Status,
    pub reason: Option<String>,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    owner_id: AccountId,
    /// Proposed new owner; becomes owner only when it calls `accept_owner`.
    pending_owner: Option<AccountId>,
    /// NEP-141 token this wallet holds and pays in.
    token_id: AccountId,
    /// Tokens held and not reserved by an in-flight payment.
    balance: u128,
    /// Tokens in in-flight transfers (payments and withdrawals). They are
    /// still in the wallet's token account until the transfer executes.
    reserved: u128,
    /// Bumped whenever an in-flight transfer resolves, so `sync_balance` can
    /// tell that the token balance it read may already be out of date.
    settle_seq: u64,
    agents: IterableMap<AccountId, AgentState>,
    vendors: IterableSet<AccountId>,
    log: Vector<LogEntry>,
    zk_agents: IterableMap<AccountId, ZkAgent>,
    zk_verifying_key: LazyOption<VerifyingKey>,
}

fn today() -> u64 {
    env::block_timestamp() / NANOS_PER_DAY
}

fn emit(event: &str, data: near_sdk::serde_json::Value) {
    let payload = json!({
        "standard": "nearx402_policy",
        "version": "2.0.0",
        "event": event,
        "data": [data],
    });
    env::log_str(&format!("EVENT_JSON:{payload}"));
}

fn emit_entry(event: &str, entry: &LogEntry) {
    emit(event, json!(entry));
}

/// Whether the single promise this callback depends on succeeded. Only an
/// explicit failure counts as failure: a successful call that returned a value
/// (`TooLong` here, since nothing is read) still moved the tokens.
fn promise_succeeded() -> bool {
    env::promise_results_count() == 1
        && !matches!(env::promise_result_checked(0, 0), Err(PromiseError::Failed))
}

#[near]
impl Contract {
    #[init]
    pub fn new(owner_id: AccountId, token_id: AccountId) -> Self {
        Self {
            owner_id,
            pending_owner: None,
            token_id,
            balance: 0,
            reserved: 0,
            settle_seq: 0,
            agents: IterableMap::new(StorageKey::Agents),
            vendors: IterableSet::new(StorageKey::Vendors),
            log: Vector::new(StorageKey::Log),
            zk_agents: IterableMap::new(StorageKey::ZkAgents),
            zk_verifying_key: LazyOption::new(StorageKey::ZkVerifyingKey, None),
        }
    }

    // ---------- funding ----------

    /// NEP-141 receiver: anyone can fund the wallet by `ft_transfer_call` from the configured token.
    pub fn ft_on_transfer(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128> {
        let _ = msg;
        require!(
            env::predecessor_account_id() == self.token_id,
            "unsupported token"
        );
        self.balance += amount.0;
        emit(
            "deposit",
            json!({ "sender_id": sender_id, "amount": amount, "balance": U128(self.balance) }),
        );
        PromiseOrValue::Value(U128(0))
    }

    // ---------- owner ----------

    /// Step 1 of an ownership transfer. The contract is locked (no access
    /// keys), so this is the only way to hand finance control to another
    /// account. `None` cancels a pending proposal.
    pub fn propose_owner(&mut self, new_owner_id: Option<AccountId>) {
        self.assert_owner();
        self.pending_owner = new_owner_id;
        emit(
            "owner_proposed",
            json!({ "owner_id": self.owner_id, "pending_owner": self.pending_owner }),
        );
    }

    /// Step 2: the proposed account accepts. Two steps, so a typo in the new
    /// account id cannot lock finance out of the wallet.
    pub fn accept_owner(&mut self) {
        let caller = env::predecessor_account_id();
        require!(
            self.pending_owner.as_ref() == Some(&caller),
            "caller is not the proposed owner"
        );
        let old = std::mem::replace(&mut self.owner_id, caller);
        self.pending_owner = None;
        emit(
            "owner_changed",
            json!({ "old_owner_id": old, "new_owner_id": self.owner_id }),
        );
    }

    /// Registers an agent or updates its limits. Keeps the frozen flag and
    /// today's counters. `max_blocked_per_day` defaults to 5.
    pub fn set_agent(
        &mut self,
        agent_id: AccountId,
        daily_cap: U128,
        per_call_max: U128,
        max_blocked_per_day: Option<u32>,
    ) {
        self.assert_owner();
        require!(
            !self.zk_agents.contains_key(&agent_id),
            "agent is registered for pay_zk"
        );
        require!(per_call_max.0 > 0, "per_call_max must be positive");
        require!(
            per_call_max.0 <= daily_cap.0,
            "per_call_max cannot exceed daily_cap"
        );
        let max_blocked = max_blocked_per_day.unwrap_or(DEFAULT_MAX_BLOCKED_PER_DAY);
        require!(max_blocked > 0, "max_blocked_per_day must be positive");
        if let Some(state) = self.agents.get_mut(&agent_id) {
            state.policy.daily_cap = daily_cap;
            state.policy.per_call_max = per_call_max;
            state.policy.max_blocked_per_day = max_blocked;
            // Lowering the limit below today's count must trip the breaker
            // now: `pay` only freezes when the count reaches the limit.
            if state.day == today() && state.blocked_today >= max_blocked && !state.policy.frozen {
                state.policy.frozen = true;
                emit(
                    "agent_auto_frozen",
                    json!({ "agent_id": agent_id, "log_id": null }),
                );
            }
        } else {
            let policy = AgentPolicy {
                daily_cap,
                per_call_max,
                frozen: false,
                max_blocked_per_day: max_blocked,
            };
            self.agents.insert(
                agent_id,
                AgentState {
                    policy,
                    day: today(),
                    spent_today: 0,
                    blocked_today: 0,
                },
            );
        }
    }

    pub fn remove_agent(&mut self, agent_id: AccountId) {
        self.assert_owner();
        require!(self.agents.remove(&agent_id).is_some(), "unknown agent");
    }

    /// Freezes or unfreezes an agent. Unfreezing also resets today's count of
    /// blocked attempts, so the circuit breaker starts over.
    pub fn set_frozen(&mut self, agent_id: AccountId, frozen: bool) {
        self.assert_owner();
        let state = self
            .agents
            .get_mut(&agent_id)
            .unwrap_or_else(|| env::panic_str("unknown agent"));
        state.policy.frozen = frozen;
        if !frozen {
            state.blocked_today = 0;
        }
    }

    pub fn approve_vendor(&mut self, vendor: AccountId) {
        self.assert_owner();
        self.vendors.insert(vendor);
    }

    pub fn revoke_vendor(&mut self, vendor: AccountId) {
        self.assert_owner();
        require!(self.vendors.remove(&vendor), "unknown vendor");
    }

    /// Sends unreserved tokens back to the owner.
    pub fn withdraw(&mut self, amount: U128) -> Promise {
        self.assert_owner();
        require!(
            amount.0 > 0 && amount.0 <= self.balance,
            "insufficient balance"
        );
        self.balance -= amount.0;
        self.reserved += amount.0;
        ext_ft::ext(self.token_id.clone())
            .with_attached_deposit(ONE_YOCTO)
            .with_static_gas(GAS_FOR_FT_TRANSFER)
            .ft_transfer(
                self.owner_id.clone(),
                amount,
                Some("nearx402:withdraw".into()),
            )
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(GAS_FOR_CALLBACK)
                    .on_withdraw(amount),
            )
    }

    /// Credits tokens that reached the wallet without `ft_transfer_call`
    /// (a plain `ft_transfer`), which `ft_on_transfer` never saw. Without this
    /// they would be stuck: the contract is locked.
    ///
    /// Reads the wallet's token balance, then credits the surplus over
    /// `balance + reserved`, but at most `max_amount`. The cap is what makes a
    /// deposit that is in flight harmless: the token may already count it
    /// while `ft_on_transfer` has not run yet, so the surplus can include it.
    /// Pass the amount you actually sent. If any transfer resolves meanwhile,
    /// nothing is credited and the call can be repeated.
    pub fn sync_balance(&mut self, max_amount: U128) -> Promise {
        self.assert_owner();
        require!(max_amount.0 > 0, "max_amount must be positive");
        ext_ft::ext(self.token_id.clone())
            .with_static_gas(GAS_FOR_BALANCE_OF)
            .ft_balance_of(env::current_account_id())
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(GAS_FOR_CALLBACK)
                    .on_sync_balance(self.settle_seq, max_amount),
            )
    }

    /// Closes a payment whose callback never ran, which would otherwise stay
    /// `Pending` with its amount reserved. Once the contract is locked, nothing
    /// else can do this.
    ///
    /// A contract cannot read an old receipt's outcome, and its token balance
    /// cannot settle the question either, so the owner reads the token's
    /// `ft_transfer` event with memo `nearx402:{id}` and states the result.
    /// `balance + reserved` never rises here, so a wrong claim cannot create funds:
    ///
    /// - `tokens_moved = true` drops the reservation. If that was wrong, the tokens
    ///   become a surplus that `sync_balance` can credit.
    /// - `tokens_moved = false` returns the amount to `balance`, after the token
    ///   confirms it holds everything the contract counts.
    ///
    /// Budgets do not go back up, unlike a failure the token reported, because
    /// this outcome is a claim. Withdrawals have no log entry and are not covered.
    pub fn reconcile_payment(&mut self, id: u32, tokens_moved: bool) -> PromiseOrValue<PayOutcome> {
        self.assert_owner();
        let entry = self
            .log
            .get(id)
            .unwrap_or_else(|| env::panic_str("unknown log entry"));
        require!(entry.status == Status::Pending, "payment is not pending");
        require!(
            env::block_timestamp().saturating_sub(entry.timestamp) >= RECONCILE_AFTER,
            "payment is too recent to reconcile"
        );
        if tokens_moved {
            return PromiseOrValue::Value(self.close_reconciled(id, true));
        }
        ext_ft::ext(self.token_id.clone())
            .with_static_gas(GAS_FOR_BALANCE_OF)
            .ft_balance_of(env::current_account_id())
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(GAS_FOR_CALLBACK)
                    .on_reconcile_unmoved(id, self.settle_seq),
            )
            .into()
    }

    /// Stores the verifying key of the policy circuit. Replacing it changes
    /// which proofs the contract accepts, so it is owner-only, and any setup
    /// behind it must not have run where an agent could see its randomness.
    pub fn set_zk_verifying_key(
        &mut self,
        alpha: Base64VecU8,
        neg_beta: Base64VecU8,
        neg_gamma: Base64VecU8,
        neg_delta: Base64VecU8,
        ic: Base64VecU8,
    ) {
        self.assert_owner();
        let vk = VerifyingKey {
            alpha: alpha.0,
            neg_beta: neg_beta.0,
            neg_gamma: neg_gamma.0,
            neg_delta: neg_delta.0,
            ic: ic.0,
        };
        vk.assert_well_formed();
        self.zk_verifying_key.set(Some(vk));
        emit("zk_verifying_key_set", json!({}));
    }

    /// Registers a private-policy agent, or rotates its commitments. This is
    /// also the recovery path: it replaces the state and clears any stuck
    /// in-flight marker, so proofs not yet submitted stop being valid.
    pub fn set_zk_agent(
        &mut self,
        agent_id: AccountId,
        policy_commitment: Base64VecU8,
        state_commitment: Base64VecU8,
        max_blocked_per_day: Option<u32>,
    ) {
        self.assert_owner();
        require!(
            !self.agents.contains_key(&agent_id),
            "agent is registered for pay"
        );
        let policy_commitment = zk::field_arg(&policy_commitment.0, "policy_commitment");
        let state_commitment = zk::field_arg(&state_commitment.0, "state_commitment");
        let max_blocked = max_blocked_per_day.unwrap_or(DEFAULT_MAX_BLOCKED_PER_DAY);
        require!(max_blocked > 0, "max_blocked_per_day must be positive");
        let day = today();
        if let Some(a) = self.zk_agents.get_mut(&agent_id) {
            a.policy_commitment = policy_commitment;
            a.state_commitment = state_commitment;
            a.max_blocked_per_day = max_blocked;
            a.pending = None;
            if a.blocked_day == day && a.blocked_today >= max_blocked && !a.frozen {
                a.frozen = true;
                emit(
                    "agent_auto_frozen",
                    json!({ "agent_id": agent_id, "log_id": null }),
                );
            }
        } else {
            self.zk_agents.insert(
                agent_id,
                ZkAgent {
                    policy_commitment,
                    state_commitment,
                    frozen: false,
                    max_blocked_per_day: max_blocked,
                    blocked_day: day,
                    blocked_today: 0,
                    pending: None,
                },
            );
        }
    }

    /// Freezes or unfreezes a ZK agent. Unfreezing resets the circuit breaker.
    pub fn set_zk_frozen(&mut self, agent_id: AccountId, frozen: bool) {
        self.assert_owner();
        let a = self
            .zk_agents
            .get_mut(&agent_id)
            .unwrap_or_else(|| env::panic_str("unknown agent"));
        a.frozen = frozen;
        if !frozen {
            a.blocked_today = 0;
        }
    }

    // ---------- agent ----------

    /// Pays a vendor under a private policy: the agent proves the payment is
    /// within limits, and the chain sees only commitments to those limits.
    ///
    /// The contract derives or checks every public input. `day` comes from
    /// block time, `vendor` and `amount` are the values it transfers, and the
    /// commitments come from storage. The `expected_*` arguments only tell an
    /// out-of-date proof (it panics, the agent should prove again) from an
    /// invalid one (it is logged as `Blocked` and counts toward the breaker).
    #[allow(clippy::too_many_arguments)]
    pub fn pay_zk(
        &mut self,
        vendor: AccountId,
        amount: U128,
        memo: String,
        proof: Base64VecU8,
        new_state: Base64VecU8,
        expected_old_state: Base64VecU8,
        expected_policy: Base64VecU8,
        expected_day: U64,
    ) -> PromiseOrValue<PayOutcome> {
        require!(amount.0 > 0, "amount must be positive");
        require!(
            !memo.is_empty() && memo.len() <= MAX_MEMO_LEN,
            "memo must be 1 to 128 bytes"
        );
        require!(
            env::prepaid_gas() >= GAS_FOR_PAY_ZK,
            "attach at least 90 Tgas to pay_zk"
        );
        let new_state = zk::field_arg(&new_state.0, "new_state");

        let agent_id = env::predecessor_account_id();
        let day = today();
        let (frozen, policy, old_state) = {
            let a = self
                .zk_agents
                .get_mut(&agent_id)
                .unwrap_or_else(|| env::panic_str("caller is not a registered ZK agent"));
            if a.blocked_day != day {
                a.blocked_day = day;
                a.blocked_today = 0;
            }
            // A retry must never race a state transition (SD-005).
            require!(a.pending.is_none(), "a payment is already in flight");
            (a.frozen, a.policy_commitment, a.state_commitment)
        };
        if frozen {
            return self.block_zk(agent_id, vendor, amount, memo, "agent is frozen");
        }

        require!(expected_day.0 == day, "stale proof: the day has changed");
        require!(
            expected_old_state.0 == old_state,
            "stale proof: the state has changed"
        );
        require!(
            expected_policy.0 == policy,
            "stale proof: the policy has changed"
        );

        if amount.0 > self.balance {
            return self.block_zk(
                agent_id,
                vendor,
                amount,
                memo,
                "insufficient wallet balance",
            );
        }

        // Order must match PolicyPublic::to_inputs in crates/zx402-zk.
        let inputs = [
            zk::context_field(env::current_account_id().as_str(), agent_id.as_str()),
            policy,
            old_state,
            new_state,
            zk::fr_from_u64(day),
            zk::vendor_field(vendor.as_str()),
            zk::fr_from_u128(amount.0),
            zk::invoice_field(&memo),
        ];
        let valid = self
            .zk_verifying_key
            .get()
            .as_ref()
            .unwrap_or_else(|| env::panic_str("no verifying key set"))
            .verify(&proof.0, &inputs);
        if !valid {
            return self.block_zk(agent_id, vendor, amount, memo, "invalid proof");
        }

        let id = self.log.len();
        emit(
            "zk_proof_verified",
            json!({ "agent_id": agent_id, "log_id": id }),
        );
        let a = self
            .zk_agents
            .get_mut(&agent_id)
            .unwrap_or_else(|| env::panic_str("caller is not a registered ZK agent"));
        a.state_commitment = new_state;
        a.pending = Some(ZkPending {
            log_id: id,
            old_state,
        });
        self.balance -= amount.0;
        self.reserved += amount.0;
        let entry = LogEntry {
            id,
            timestamp: env::block_timestamp(),
            agent_id,
            vendor: vendor.clone(),
            amount,
            status: Status::Pending,
            reason: None,
            memo: Some(memo),
            settled_at: None,
        };
        emit_entry("payment_approved", &entry);
        self.log.push(entry);

        ext_ft::ext(self.token_id.clone())
            .with_attached_deposit(ONE_YOCTO)
            .with_static_gas(GAS_FOR_FT_TRANSFER)
            .ft_transfer(vendor, amount, Some(format!("nearx402:{id}")))
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(GAS_FOR_CALLBACK)
                    .on_pay_transfer(id),
            )
            .into()
    }

    /// Called by a registered agent to pay a vendor. Within policy, the contract
    /// transfers the tokens and the call resolves to `Paid` or `Failed`.
    /// Off-policy calls never panic: they are logged as `Blocked` with a reason.
    pub fn pay(
        &mut self,
        vendor: AccountId,
        amount: U128,
        memo: Option<String>,
    ) -> PromiseOrValue<PayOutcome> {
        require!(amount.0 > 0, "amount must be positive");
        if let Some(m) = &memo {
            require!(m.len() <= MAX_MEMO_LEN, "memo too long");
        }
        require!(
            env::prepaid_gas() >= GAS_FOR_PAY,
            "attach at least 50 Tgas to pay"
        );

        let agent_id = env::predecessor_account_id();
        let vendor_ok = self.vendors.contains(&vendor);
        let balance = self.balance;
        let day = today();
        let state = self
            .agents
            .get_mut(&agent_id)
            .unwrap_or_else(|| env::panic_str("caller is not a registered agent"));
        if state.day != day {
            state.day = day;
            state.spent_today = 0;
            state.blocked_today = 0;
        }

        let reason = if state.policy.frozen {
            Some("agent is frozen")
        } else if !vendor_ok {
            Some("recipient not in allowlist")
        } else if amount.0 > state.policy.per_call_max.0 {
            Some("amount exceeds per-call max")
        } else if state.spent_today.saturating_add(amount.0) > state.policy.daily_cap.0 {
            Some("daily cap exceeded")
        } else if amount.0 > balance {
            Some("insufficient wallet balance")
        } else {
            None
        };

        let mut auto_frozen = false;
        let status = match reason {
            None => {
                state.spent_today += amount.0;
                self.balance -= amount.0;
                self.reserved += amount.0;
                Status::Pending
            }
            Some(_) => {
                // Panicking writes nothing, so once the breaker has tripped,
                // further attempts cost the agent gas but add no log entries.
                require!(
                    state.blocked_today < state.policy.max_blocked_per_day,
                    "agent frozen: too many blocked attempts today"
                );
                state.blocked_today += 1;
                if state.blocked_today == state.policy.max_blocked_per_day && !state.policy.frozen {
                    state.policy.frozen = true;
                    auto_frozen = true;
                }
                Status::Blocked
            }
        };

        let id = self.log.len();
        let entry = LogEntry {
            id,
            timestamp: env::block_timestamp(),
            agent_id,
            vendor: vendor.clone(),
            amount,
            status,
            reason: reason.map(str::to_string),
            memo,
            settled_at: None,
        };
        emit_entry(
            if status == Status::Blocked {
                "payment_blocked"
            } else {
                "payment_approved"
            },
            &entry,
        );
        if auto_frozen {
            emit(
                "agent_auto_frozen",
                json!({ "agent_id": entry.agent_id, "log_id": id }),
            );
        }
        let outcome = PayOutcome {
            log_id: id,
            status,
            reason: entry.reason.clone(),
        };
        self.log.push(entry);

        if status == Status::Blocked {
            return PromiseOrValue::Value(outcome);
        }
        ext_ft::ext(self.token_id.clone())
            .with_attached_deposit(ONE_YOCTO)
            .with_static_gas(GAS_FOR_FT_TRANSFER)
            .ft_transfer(vendor, amount, Some(format!("nearx402:{id}")))
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(GAS_FOR_CALLBACK)
                    .on_pay_transfer(id),
            )
            .into()
    }

    // ---------- callbacks ----------

    #[private]
    pub fn on_pay_transfer(&mut self, id: u32) -> PayOutcome {
        let ok = promise_succeeded();
        let entry = self
            .log
            .get_mut(id)
            .unwrap_or_else(|| env::panic_str("unknown log entry"));
        require!(entry.status == Status::Pending, "payment is not pending");
        if ok {
            entry.status = Status::Paid;
            entry.settled_at = Some(env::block_height());
        } else {
            entry.status = Status::Failed;
            entry.reason =
                Some("token transfer failed (is the vendor registered with the token?)".into());
        }
        let entry = entry.clone();
        self.reserved -= entry.amount.0;
        self.settle_seq += 1;
        if !ok {
            self.balance += entry.amount.0;
            if let Some(state) = self.agents.get_mut(&entry.agent_id) {
                if state.day == entry.timestamp / NANOS_PER_DAY {
                    state.spent_today = state.spent_today.saturating_sub(entry.amount.0);
                }
            }
        }
        if let Some(a) = self.zk_agents.get_mut(&entry.agent_id) {
            // Skipped if finance has rotated the agent since: the marker is
            // gone, or it belongs to a newer payment.
            if a.pending.as_ref().is_some_and(|p| p.log_id == id) {
                if let Some(p) = a.pending.take() {
                    if !ok {
                        a.state_commitment = p.old_state;
                    }
                }
            }
        }
        emit_entry(if ok { "payment_paid" } else { "payment_failed" }, &entry);
        PayOutcome {
            log_id: id,
            status: entry.status,
            reason: entry.reason,
        }
    }

    #[private]
    pub fn on_withdraw(&mut self, amount: U128) {
        self.reserved -= amount.0;
        self.settle_seq += 1;
        if !promise_succeeded() {
            self.balance += amount.0;
        }
    }

    #[private]
    pub fn on_sync_balance(&mut self, seq: u64, max_amount: U128) -> U128 {
        let held = match env::promise_result_checked(0, 64) {
            Ok(bytes) => near_sdk::serde_json::from_slice::<U128>(&bytes).ok(),
            Err(_) => None,
        };
        let credited = match held {
            None => {
                emit(
                    "sync_failed",
                    json!({ "reason": "could not read token balance" }),
                );
                0
            }
            Some(_) if seq != self.settle_seq => {
                emit(
                    "sync_failed",
                    json!({ "reason": "a transfer resolved meanwhile, try again" }),
                );
                0
            }
            Some(held) => {
                let counted = self.balance + self.reserved;
                held.0.saturating_sub(counted).min(max_amount.0)
            }
        };
        if credited > 0 {
            self.balance += credited;
            emit(
                "deposit",
                json!({ "sender_id": null, "amount": U128(credited), "balance": U128(self.balance), "via": "sync_balance" }),
            );
        }
        U128(credited)
    }

    /// Finishes a `tokens_moved = false` claim. A refusal changes nothing, so the
    /// owner can call again.
    #[private]
    pub fn on_reconcile_unmoved(&mut self, id: u32, seq: u64) -> PayOutcome {
        let entry = self
            .log
            .get(id)
            .unwrap_or_else(|| env::panic_str("unknown log entry"));
        require!(entry.status == Status::Pending, "payment is not pending");
        require!(
            seq == self.settle_seq,
            "a transfer resolved meanwhile, try again"
        );
        let held = env::promise_result_checked(0, 64)
            .ok()
            .and_then(|bytes| near_sdk::serde_json::from_slice::<U128>(&bytes).ok())
            .unwrap_or_else(|| env::panic_str("could not read token balance"));
        require!(
            held.0 >= self.balance + self.reserved,
            "the token balance does not cover the wallet, so the tokens appear to have moved"
        );
        self.close_reconciled(id, false)
    }

    // ---------- views ----------

    pub fn get_owner(&self) -> AccountId {
        self.owner_id.clone()
    }

    pub fn get_pending_owner(&self) -> Option<AccountId> {
        self.pending_owner.clone()
    }

    pub fn get_token_id(&self) -> AccountId {
        self.token_id.clone()
    }

    pub fn get_balance(&self) -> U128 {
        U128(self.balance)
    }

    pub fn get_agent(&self, agent_id: AccountId) -> Option<AgentView> {
        self.agents
            .get(&agent_id)
            .map(|state| self.view_of(agent_id.clone(), state))
    }

    pub fn get_agents(&self, from_index: Option<u32>, limit: Option<u32>) -> Vec<AgentView> {
        let (from, limit) = page(from_index, limit);
        self.agents
            .iter()
            .skip(from as usize)
            .take(limit as usize)
            .map(|(id, state)| self.view_of(id.clone(), state))
            .collect()
    }

    /// The UTC day number the contract will use, which a `pay_zk` proof must
    /// commit to.
    pub fn get_day(&self) -> U64 {
        U64(today())
    }

    pub fn get_zk_verifying_key(&self) -> Option<ZkVerifyingKeyView> {
        self.zk_verifying_key
            .get()
            .as_ref()
            .map(|vk| ZkVerifyingKeyView {
                alpha: vk.alpha.clone().into(),
                neg_beta: vk.neg_beta.clone().into(),
                neg_gamma: vk.neg_gamma.clone().into(),
                neg_delta: vk.neg_delta.clone().into(),
                ic: vk.ic.clone().into(),
            })
    }

    pub fn get_zk_agent(&self, agent_id: AccountId) -> Option<ZkAgentView> {
        let a = self.zk_agents.get(&agent_id)?;
        Some(zk_view_of(agent_id, a))
    }

    pub fn get_zk_agents(&self, from_index: Option<u32>, limit: Option<u32>) -> Vec<ZkAgentView> {
        let (from, limit) = page(from_index, limit);
        self.zk_agents
            .iter()
            .skip(from as usize)
            .take(limit as usize)
            .map(|(id, a)| zk_view_of(id.clone(), a))
            .collect()
    }

    pub fn get_vendors(&self) -> Vec<AccountId> {
        self.vendors.iter().cloned().collect()
    }

    pub fn is_vendor_approved(&self, vendor: AccountId) -> bool {
        self.vendors.contains(&vendor)
    }

    pub fn get_log_entry(&self, id: u32) -> Option<LogEntry> {
        self.log.get(id).cloned()
    }

    pub fn get_log(&self, from_index: Option<u32>, limit: Option<u32>) -> Vec<LogEntry> {
        let (from, limit) = page(from_index, limit);
        (from..self.log.len().min(from.saturating_add(limit)))
            .filter_map(|i| self.log.get(i).cloned())
            .collect()
    }

    pub fn get_log_length(&self) -> u32 {
        self.log.len()
    }

    // ---------- internal ----------

    fn assert_owner(&self) {
        require!(env::predecessor_account_id() == self.owner_id, "owner only");
    }

    /// Logs a rejected `pay_zk` as `Blocked` and counts it toward the breaker.
    /// Once the breaker has tripped, attempts panic and write nothing.
    fn block_zk(
        &mut self,
        agent_id: AccountId,
        vendor: AccountId,
        amount: U128,
        memo: String,
        reason: &str,
    ) -> PromiseOrValue<PayOutcome> {
        let a = self
            .zk_agents
            .get_mut(&agent_id)
            .unwrap_or_else(|| env::panic_str("caller is not a registered ZK agent"));
        require!(
            a.blocked_today < a.max_blocked_per_day,
            "agent frozen: too many blocked attempts today"
        );
        a.blocked_today += 1;
        let tripped = a.blocked_today == a.max_blocked_per_day && !a.frozen;
        if tripped {
            a.frozen = true;
        }
        let id = self.log.len();
        let entry = LogEntry {
            id,
            timestamp: env::block_timestamp(),
            agent_id,
            vendor,
            amount,
            status: Status::Blocked,
            reason: Some(reason.to_string()),
            memo: Some(memo),
            settled_at: None,
        };
        emit_entry("payment_blocked", &entry);
        if tripped {
            emit(
                "agent_auto_frozen",
                json!({ "agent_id": entry.agent_id, "log_id": id }),
            );
        }
        let outcome = PayOutcome {
            log_id: id,
            status: Status::Blocked,
            reason: entry.reason.clone(),
        };
        self.log.push(entry);
        PromiseOrValue::Value(outcome)
    }

    /// Closes a `Pending` entry on the owner's claim. See `reconcile_payment`.
    fn close_reconciled(&mut self, id: u32, tokens_moved: bool) -> PayOutcome {
        let entry = self
            .log
            .get_mut(id)
            .unwrap_or_else(|| env::panic_str("unknown log entry"));
        entry.status = if tokens_moved {
            Status::Paid
        } else {
            Status::Failed
        };
        entry.reason = Some(
            if tokens_moved {
                "reconciled by the owner: tokens moved"
            } else {
                "reconciled by the owner: tokens did not move"
            }
            .into(),
        );
        let entry = entry.clone();
        self.reserved -= entry.amount.0;
        self.settle_seq += 1;
        if !tokens_moved {
            self.balance += entry.amount.0;
        }
        if let Some(a) = self.zk_agents.get_mut(&entry.agent_id) {
            // As in `on_pay_transfer`: a rotated agent's marker is gone, or
            // belongs to a newer payment. The state stays, so spend stays counted.
            if a.pending.as_ref().is_some_and(|p| p.log_id == id) {
                a.pending = None;
            }
        }
        emit_entry("payment_reconciled", &entry);
        PayOutcome {
            log_id: id,
            status: entry.status,
            reason: entry.reason,
        }
    }

    fn view_of(&self, agent_id: AccountId, state: &AgentState) -> AgentView {
        let (spent, blocked) = if state.day == today() {
            (state.spent_today, state.blocked_today)
        } else {
            (0, 0)
        };
        AgentView {
            agent_id,
            policy: state.policy.clone(),
            spent_today: U128(spent),
            blocked_today: blocked,
        }
    }
}

fn zk_view_of(agent_id: AccountId, a: &ZkAgent) -> ZkAgentView {
    ZkAgentView {
        policy_commitment: a.policy_commitment.to_vec().into(),
        state_commitment: a.state_commitment.to_vec().into(),
        frozen: a.frozen,
        max_blocked_per_day: a.max_blocked_per_day,
        blocked_today: if a.blocked_day == today() {
            a.blocked_today
        } else {
            0
        },
        pending_log_id: a.pending.as_ref().map(|p| p.log_id),
        agent_id,
    }
}

fn page(from_index: Option<u32>, limit: Option<u32>) -> (u32, u32) {
    (
        from_index.unwrap_or(0),
        limit.unwrap_or(MAX_PAGE).min(MAX_PAGE),
    )
}

#[cfg(test)]
#[allow(unused_must_use)]
mod zk_tests;

#[cfg(test)]
// Unit tests call `pay`/`withdraw` directly; the returned promises are not executed here.
#[allow(unused_must_use)]
mod tests {
    use super::*;
    use near_sdk::test_utils::{accounts, VMContextBuilder};
    use near_sdk::{testing_env, PromiseResult};

    fn owner() -> AccountId {
        accounts(0)
    }
    fn agent() -> AccountId {
        accounts(1)
    }
    fn vendor() -> AccountId {
        "data-api.near".parse().unwrap()
    }
    fn token() -> AccountId {
        "usdc.near".parse().unwrap()
    }
    fn contract_id() -> AccountId {
        "policy.near".parse().unwrap()
    }

    fn usdc_cents(cents: u128) -> U128 {
        U128(cents * 10_000)
    }

    fn ctx(caller: AccountId, timestamp: u64) -> VMContextBuilder {
        let mut b = VMContextBuilder::new();
        b.current_account_id(contract_id())
            .predecessor_account_id(caller)
            .block_timestamp(timestamp)
            .prepaid_gas(Gas::from_tgas(100));
        b
    }

    fn call_as(caller: AccountId, timestamp: u64) {
        testing_env!(ctx(caller, timestamp).build());
    }

    /// Runs `on_pay_transfer` as the contract itself, with the given transfer result.
    fn settle(c: &mut Contract, id: u32, ok: bool) -> PayOutcome {
        let result = if ok {
            PromiseResult::Successful(vec![])
        } else {
            PromiseResult::Failed
        };
        settle_with(c, id, result)
    }

    fn settle_with(c: &mut Contract, id: u32, result: PromiseResult) -> PayOutcome {
        as_callback(result);
        c.on_pay_transfer(id)
    }

    /// Next call runs as the contract itself, with one promise result.
    fn as_callback(result: PromiseResult) {
        testing_env!(
            ctx(contract_id(), 0).build(),
            near_sdk::test_vm_config(),
            near_sdk::RuntimeFeesConfig::test(),
            Default::default(),
            vec![result]
        );
    }

    /// Runs `on_sync_balance` as if the token reported `held`.
    fn sync(c: &mut Contract, seq: u64, max: U128, held: Option<U128>) -> u128 {
        as_callback(match held {
            Some(h) => PromiseResult::Successful(near_sdk::serde_json::to_vec(&h).unwrap()),
            None => PromiseResult::Failed,
        });
        c.on_sync_balance(seq, max).0
    }

    fn outcome(p: PromiseOrValue<PayOutcome>) -> Option<PayOutcome> {
        match p {
            PromiseOrValue::Value(o) => Some(o),
            PromiseOrValue::Promise(_) => None,
        }
    }

    /// Owner = accounts(0); agent = accounts(1) with $5/day and $0.50/call; wallet holds $10.
    fn setup() -> Contract {
        call_as(owner(), 0);
        let mut c = Contract::new(owner(), token());
        c.set_agent(agent(), usdc_cents(500), usdc_cents(50), None);
        c.approve_vendor(vendor());
        call_as(token(), 0);
        c.ft_on_transfer(owner(), usdc_cents(1000), String::new());
        c
    }

    fn spent(c: &Contract) -> u128 {
        c.get_agent(agent()).unwrap().spent_today.0
    }

    fn pay(c: &mut Contract, to: AccountId, amount: U128) -> PromiseOrValue<PayOutcome> {
        call_as(agent(), 0);
        c.pay(to, amount, Some("nonce-1".into()))
    }

    #[test]
    fn allowed_payment_transfers_and_settles() {
        let mut c = setup();
        assert!(outcome(pay(&mut c, vendor(), usdc_cents(25))).is_none());
        assert_eq!(c.get_log_entry(0).unwrap().status, Status::Pending);
        assert_eq!(spent(&c), usdc_cents(25).0);
        assert_eq!(c.get_balance().0, usdc_cents(975).0);

        let out = settle(&mut c, 0, true);
        assert_eq!(out.status, Status::Paid);
        let entry = c.get_log_entry(0).unwrap();
        assert_eq!(entry.status, Status::Paid);
        assert!(entry.settled_at.is_some());
        assert_eq!(entry.memo.as_deref(), Some("nonce-1"));
    }

    #[test]
    fn failed_transfer_restores_budget_and_balance() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(25));
        let out = settle(&mut c, 0, false);
        assert_eq!(out.status, Status::Failed);
        call_as(owner(), 0);
        assert_eq!(spent(&c), 0);
        assert_eq!(c.get_balance().0, usdc_cents(1000).0);
    }

    #[test]
    fn unknown_vendor_is_blocked_and_logged() {
        let mut c = setup();
        let out = outcome(pay(
            &mut c,
            "attacker.near".parse().unwrap(),
            usdc_cents(10),
        ))
        .unwrap();
        assert_eq!(out.status, Status::Blocked);
        assert_eq!(out.reason.as_deref(), Some("recipient not in allowlist"));
        assert_eq!(spent(&c), 0);
        assert_eq!(c.get_balance().0, usdc_cents(1000).0);
        let log = c.get_log(None, None);
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].status, Status::Blocked);
    }

    #[test]
    fn per_call_max_is_enforced() {
        let mut c = setup();
        let out = outcome(pay(&mut c, vendor(), usdc_cents(51))).unwrap();
        assert_eq!(out.reason.as_deref(), Some("amount exceeds per-call max"));
    }

    #[test]
    fn daily_cap_is_enforced_and_resets_next_day() {
        let mut c = setup();
        for _ in 0..10 {
            assert!(outcome(pay(&mut c, vendor(), usdc_cents(50))).is_none());
        }
        let out = outcome(pay(&mut c, vendor(), usdc_cents(1))).unwrap();
        assert_eq!(out.reason.as_deref(), Some("daily cap exceeded"));

        call_as(agent(), NANOS_PER_DAY);
        assert_eq!(spent(&c), 0);
        assert!(outcome(c.pay(vendor(), usdc_cents(50), None)).is_none());
    }

    #[test]
    fn insufficient_balance_is_blocked() {
        call_as(owner(), 0);
        let mut c = Contract::new(owner(), token());
        c.set_agent(agent(), usdc_cents(500), usdc_cents(50), None);
        c.approve_vendor(vendor());
        let out = outcome(pay(&mut c, vendor(), usdc_cents(10))).unwrap();
        assert_eq!(out.reason.as_deref(), Some("insufficient wallet balance"));
    }

    #[test]
    fn frozen_agent_is_blocked() {
        let mut c = setup();
        call_as(owner(), 0);
        c.set_frozen(agent(), true);
        let out = outcome(pay(&mut c, vendor(), usdc_cents(10))).unwrap();
        assert_eq!(out.reason.as_deref(), Some("agent is frozen"));
    }

    #[test]
    fn approving_vendor_lets_retry_succeed() {
        let mut c = setup();
        let newcomer: AccountId = "new-vendor.near".parse().unwrap();
        let first = outcome(pay(&mut c, newcomer.clone(), usdc_cents(10))).unwrap();
        assert_eq!(first.status, Status::Blocked);
        call_as(owner(), 0);
        c.approve_vendor(newcomer.clone());
        assert!(outcome(pay(&mut c, newcomer, usdc_cents(10))).is_none());
        assert_eq!(c.get_log_length(), 2);
    }

    #[test]
    #[should_panic(expected = "unsupported token")]
    fn deposit_from_other_token_is_rejected() {
        let mut c = setup();
        call_as("fake-usdc.near".parse().unwrap(), 0);
        c.ft_on_transfer(owner(), usdc_cents(1000), String::new());
    }

    #[test]
    #[should_panic(expected = "payment is not pending")]
    fn blocked_entry_cannot_be_settled() {
        let mut c = setup();
        pay(&mut c, "attacker.near".parse().unwrap(), usdc_cents(10));
        settle(&mut c, 0, true);
    }

    #[test]
    #[should_panic(expected = "caller is not a registered agent")]
    fn unregistered_caller_cannot_pay() {
        let mut c = setup();
        call_as(accounts(2), 0);
        c.pay(vendor(), usdc_cents(10), None);
    }

    #[test]
    #[should_panic(expected = "attach at least 50 Tgas")]
    fn pay_requires_enough_gas() {
        let mut c = setup();
        testing_env!(ctx(agent(), 0).prepaid_gas(Gas::from_tgas(10)).build());
        c.pay(vendor(), usdc_cents(10), None);
    }

    #[test]
    #[should_panic(expected = "owner only")]
    fn agent_cannot_approve_vendors() {
        let mut c = setup();
        call_as(agent(), 0);
        c.approve_vendor("attacker.near".parse().unwrap());
    }

    #[test]
    #[should_panic(expected = "insufficient balance")]
    fn owner_cannot_withdraw_reserved_funds() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(50));
        call_as(owner(), 0);
        c.withdraw(usdc_cents(1000));
    }

    #[test]
    fn transfer_that_returns_a_value_still_counts_as_paid() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(25));
        let out = settle_with(&mut c, 0, PromiseResult::Successful(vec![b'x'; 200]));
        assert_eq!(out.status, Status::Paid);
        assert_eq!(c.get_balance().0, usdc_cents(975).0);
    }

    #[test]
    fn ownership_moves_in_two_steps() {
        let mut c = setup();
        let new_owner = accounts(3);
        call_as(owner(), 0);
        c.propose_owner(Some(new_owner.clone()));
        assert_eq!(c.get_owner(), owner());
        assert_eq!(c.get_pending_owner(), Some(new_owner.clone()));
        call_as(new_owner.clone(), 0);
        c.accept_owner();
        assert_eq!(c.get_owner(), new_owner);
        assert_eq!(c.get_pending_owner(), None);
        c.approve_vendor("another.near".parse().unwrap());
    }

    #[test]
    #[should_panic(expected = "caller is not the proposed owner")]
    fn only_the_proposed_owner_can_accept() {
        let mut c = setup();
        call_as(owner(), 0);
        c.propose_owner(Some(accounts(3)));
        call_as(agent(), 0);
        c.accept_owner();
    }

    #[test]
    #[should_panic(expected = "owner only")]
    fn agent_cannot_propose_an_owner() {
        let mut c = setup();
        call_as(agent(), 0);
        c.propose_owner(Some(agent()));
    }

    #[test]
    #[should_panic(expected = "per_call_max cannot exceed daily_cap")]
    fn invalid_limits_are_rejected() {
        let mut c = setup();
        call_as(owner(), 0);
        c.set_agent(accounts(2), usdc_cents(10), usdc_cents(20), None);
    }

    fn attacker() -> AccountId {
        "attacker.near".parse().unwrap()
    }

    /// Five blocked attempts with the default breaker of 5.
    fn trip_breaker(c: &mut Contract) {
        for i in 1..=5 {
            let out = outcome(pay(c, attacker(), usdc_cents(10))).unwrap();
            assert_eq!(out.status, Status::Blocked);
            assert_eq!(c.get_agent(agent()).unwrap().blocked_today, i);
        }
    }

    #[test]
    fn fifth_blocked_attempt_freezes_the_agent() {
        let mut c = setup();
        trip_breaker(&mut c);
        let a = c.get_agent(agent()).unwrap();
        assert!(a.policy.frozen);
        assert_eq!(c.get_log_length(), 5);
    }

    #[test]
    #[should_panic(expected = "agent frozen: too many blocked attempts today")]
    fn attempts_after_the_breaker_trips_are_not_logged() {
        let mut c = setup();
        trip_breaker(&mut c);
        pay(&mut c, vendor(), usdc_cents(10));
    }

    #[test]
    fn unfreezing_resets_the_breaker() {
        let mut c = setup();
        trip_breaker(&mut c);
        call_as(owner(), 0);
        c.set_frozen(agent(), false);
        assert_eq!(c.get_agent(agent()).unwrap().blocked_today, 0);
        assert!(outcome(pay(&mut c, vendor(), usdc_cents(10))).is_none());
    }

    #[test]
    fn lowering_the_limit_below_todays_count_freezes_the_agent() {
        let mut c = setup();
        for _ in 0..3 {
            pay(&mut c, attacker(), usdc_cents(10));
        }
        assert!(!c.get_agent(agent()).unwrap().policy.frozen);
        call_as(owner(), 0);
        c.set_agent(agent(), usdc_cents(500), usdc_cents(50), Some(2));
        let a = c.get_agent(agent()).unwrap();
        assert!(a.policy.frozen);
        assert_eq!(a.policy.max_blocked_per_day, 2);
        assert_eq!(a.blocked_today, 3);
    }

    #[test]
    #[should_panic(expected = "agent frozen: too many blocked attempts today")]
    fn agent_frozen_by_a_lower_limit_cannot_pay() {
        let mut c = setup();
        for _ in 0..3 {
            pay(&mut c, attacker(), usdc_cents(10));
        }
        call_as(owner(), 0);
        c.set_agent(agent(), usdc_cents(500), usdc_cents(50), Some(2));
        pay(&mut c, vendor(), usdc_cents(10));
    }

    #[test]
    fn breaker_count_resets_each_day_but_the_freeze_stays() {
        let mut c = setup();
        trip_breaker(&mut c);
        call_as(agent(), NANOS_PER_DAY);
        let out = outcome(c.pay(vendor(), usdc_cents(10), None)).unwrap();
        assert_eq!(out.reason.as_deref(), Some("agent is frozen"));
        assert_eq!(c.get_agent(agent()).unwrap().blocked_today, 1);
    }

    #[test]
    fn sync_balance_credits_plain_transfers_up_to_the_cap() {
        let mut c = setup();
        // Someone sent $3 with plain ft_transfer: the token reports $13.
        let held = Some(usdc_cents(1300));
        assert_eq!(sync(&mut c, 0, usdc_cents(100), held), usdc_cents(100).0);
        assert_eq!(c.get_balance().0, usdc_cents(1100).0);
        assert_eq!(sync(&mut c, 0, usdc_cents(500), held), usdc_cents(200).0);
        assert_eq!(c.get_balance().0, usdc_cents(1300).0);
        // Nothing left to credit.
        assert_eq!(sync(&mut c, 0, usdc_cents(500), held), 0);
    }

    #[test]
    fn sync_balance_counts_payments_in_flight() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(25));
        // The $0.25 is still in the token account until the transfer runs.
        assert_eq!(sync(&mut c, 0, usdc_cents(500), Some(usdc_cents(1000))), 0);
        assert_eq!(c.get_balance().0, usdc_cents(975).0);
    }

    #[test]
    fn sync_balance_gives_up_if_a_transfer_resolved_meanwhile() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(25));
        let seq = c.settle_seq;
        settle(&mut c, 0, true);
        // The token balance was read before the payment went out.
        assert_eq!(
            sync(&mut c, seq, usdc_cents(500), Some(usdc_cents(1000))),
            0
        );
        assert_eq!(c.get_balance().0, usdc_cents(975).0);
        assert_eq!(sync(&mut c, 0, usdc_cents(500), None), 0);
    }

    #[test]
    #[should_panic(expected = "owner only")]
    fn only_the_owner_can_sync() {
        let mut c = setup();
        call_as(agent(), 0);
        c.sync_balance(usdc_cents(1));
    }

    // ---------- reconcile_payment: a callback that never ran ----------
    //
    // A test simulates a lost callback by not calling `on_pay_transfer`.

    /// Sends the owner's claim, as of block time `at`.
    fn claim(c: &mut Contract, id: u32, tokens_moved: bool, at: u64) -> PromiseOrValue<PayOutcome> {
        call_as(owner(), at);
        c.reconcile_payment(id, tokens_moved)
    }

    /// Runs the callback of a `tokens_moved = false` claim, as if the token reported `held`.
    fn finish_claim(c: &mut Contract, id: u32, seq: u64, held: Option<U128>) -> PayOutcome {
        as_callback(match held {
            Some(h) => PromiseResult::Successful(near_sdk::serde_json::to_vec(&h).unwrap()),
            None => PromiseResult::Failed,
        });
        c.on_reconcile_unmoved(id, seq)
    }

    /// Everything the contract counts as its own.
    fn counted(c: &Contract) -> u128 {
        c.balance + c.reserved
    }

    /// Runs a call that must panic, returns its message, and checks that the
    /// books did not change. A real panic also rolls back the state, so this
    /// shows the check comes before any change.
    fn refused(c: &mut Contract, f: impl FnOnce(&mut Contract)) -> String {
        let before = (c.balance, c.reserved, c.settle_seq);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(c)));
        let Err(payload) = result else {
            panic!("the call should have panicked");
        };
        assert_eq!((c.balance, c.reserved, c.settle_seq), before);
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default()
    }

    #[test]
    #[should_panic(expected = "owner only")]
    fn reconcile_is_for_the_owner_only() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(25));
        call_as(agent(), NANOS_PER_DAY);
        c.reconcile_payment(0, true);
    }

    #[test]
    #[should_panic(expected = "unknown log entry")]
    fn reconcile_refuses_an_unknown_entry() {
        let mut c = setup();
        claim(&mut c, 7, true, NANOS_PER_DAY);
    }

    #[test]
    fn reconcile_refuses_entries_that_are_not_pending() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(25));
        settle(&mut c, 0, true);
        pay(&mut c, vendor(), usdc_cents(25));
        settle(&mut c, 1, false);
        let blocked = outcome(pay(&mut c, accounts(3), usdc_cents(25))).unwrap();
        assert_eq!(blocked.status, Status::Blocked);
        for id in [0, 1, 2] {
            for moved in [true, false] {
                let text = refused(&mut c, |c| {
                    claim(c, id, moved, NANOS_PER_DAY);
                });
                assert!(text.contains("payment is not pending"), "{text}");
            }
        }
    }

    #[test]
    fn reconcile_waits_a_full_day() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(25));
        for moved in [true, false] {
            let text = refused(&mut c, |c| {
                claim(c, 0, moved, NANOS_PER_DAY - 1);
            });
            assert!(text.contains("too recent"), "{text}");
        }
        assert!(outcome(claim(&mut c, 0, true, NANOS_PER_DAY)).is_some());
    }

    #[test]
    fn reconcile_of_moved_tokens_only_drops_the_reservation() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(25));
        let seq = c.settle_seq;

        let out = outcome(claim(&mut c, 0, true, NANOS_PER_DAY)).unwrap();

        assert_eq!(out.status, Status::Paid);
        assert_eq!(c.reserved, 0);
        assert_eq!(c.balance, usdc_cents(975).0);
        assert_eq!(c.settle_seq, seq + 1);
        let entry = c.get_log_entry(0).unwrap();
        assert_eq!(entry.status, Status::Paid);
        assert!(entry.reason.unwrap().contains("reconciled by the owner"));
        // No block is known in which the transfer settled.
        assert_eq!(entry.settled_at, None);
        call_as(owner(), 0);
        assert_eq!(
            spent(&c),
            usdc_cents(25).0,
            "the budget must not go back up"
        );
    }

    #[test]
    fn reconcile_of_unmoved_tokens_returns_them_after_reading_the_token() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(25));
        let seq = c.settle_seq;

        assert!(outcome(claim(&mut c, 0, false, NANOS_PER_DAY)).is_none());
        assert_eq!(
            c.reserved,
            usdc_cents(25).0,
            "nothing changes before the read"
        );
        assert_eq!(c.get_log_entry(0).unwrap().status, Status::Pending);

        let out = finish_claim(&mut c, 0, seq, Some(usdc_cents(1000)));

        assert_eq!(out.status, Status::Failed);
        assert_eq!(c.reserved, 0);
        assert_eq!(c.balance, usdc_cents(1000).0);
        assert_eq!(c.settle_seq, seq + 1);
        let entry = c.get_log_entry(0).unwrap();
        assert_eq!(entry.status, Status::Failed);
        assert!(entry.reason.unwrap().contains("reconciled by the owner"));
        call_as(owner(), 0);
        assert_eq!(
            spent(&c),
            usdc_cents(25).0,
            "the budget must not go back up"
        );
    }

    #[test]
    fn reconcile_refuses_unmoved_when_the_token_holds_less_than_counted() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(25));
        let seq = c.settle_seq;
        claim(&mut c, 0, false, NANOS_PER_DAY);
        let short = counted(&c) - 1;
        let text = refused(&mut c, |c| {
            finish_claim(c, 0, seq, Some(U128(short)));
        });
        assert!(text.contains("does not cover"), "{text}");
        assert_eq!(c.get_log_entry(0).unwrap().status, Status::Pending);
    }

    #[test]
    fn reconcile_refuses_unmoved_when_the_token_cannot_be_read() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(25));
        let seq = c.settle_seq;
        claim(&mut c, 0, false, NANOS_PER_DAY);
        let text = refused(&mut c, |c| {
            finish_claim(c, 0, seq, None);
        });
        assert!(text.contains("could not read"), "{text}");
    }

    #[test]
    fn reconcile_refuses_unmoved_when_a_transfer_resolved_during_the_read() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(25));
        pay(&mut c, vendor(), usdc_cents(25));
        let seq = c.settle_seq;
        claim(&mut c, 0, false, NANOS_PER_DAY);
        let held = counted(&c);
        settle(&mut c, 1, true);
        let text = refused(&mut c, |c| {
            finish_claim(c, 0, seq, Some(U128(held)));
        });
        assert!(text.contains("try again"), "{text}");
        assert_eq!(c.get_log_entry(0).unwrap().status, Status::Pending);
    }

    #[test]
    fn reconcile_cannot_release_the_same_payment_twice() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(25));
        let seq = c.settle_seq;
        let held = counted(&c);
        // Two claims read the token, or the payment settles meanwhile.
        claim(&mut c, 0, false, NANOS_PER_DAY);
        claim(&mut c, 0, false, NANOS_PER_DAY);
        finish_claim(&mut c, 0, seq, Some(U128(held)));
        let text = refused(&mut c, |c| {
            finish_claim(c, 0, seq + 1, Some(U128(held)));
        });
        assert!(text.contains("payment is not pending"), "{text}");
        assert_eq!(c.balance, usdc_cents(1000).0);
    }

    #[test]
    fn a_late_callback_after_reconcile_changes_nothing() {
        for moved in [true, false] {
            let mut c = setup();
            pay(&mut c, vendor(), usdc_cents(25));
            let seq = c.settle_seq;
            claim(&mut c, 0, moved, NANOS_PER_DAY);
            if !moved {
                finish_claim(&mut c, 0, seq, Some(usdc_cents(1000)));
            }
            for ok in [true, false] {
                let text = refused(&mut c, |c| {
                    settle(c, 0, ok);
                });
                assert!(text.contains("payment is not pending"), "{text}");
            }
        }
    }

    #[test]
    fn a_wrong_reconcile_of_moved_tokens_leaves_them_for_sync_balance() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(25));
        // The owner says the tokens moved, but they are still in the wallet.
        claim(&mut c, 0, true, NANOS_PER_DAY);
        let held = usdc_cents(1000);
        let seq = c.settle_seq;
        assert_eq!(
            sync(&mut c, seq, usdc_cents(500), Some(held)),
            usdc_cents(25).0
        );
        assert_eq!(c.balance, held.0);
        assert_eq!(sync(&mut c, seq, usdc_cents(500), Some(held)), 0);
    }

    #[test]
    fn reconcile_gives_up_a_balance_read_already_in_flight() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(25));
        let seq = c.settle_seq;
        claim(&mut c, 0, true, NANOS_PER_DAY);
        // This read came before the reconcile, so its balance is out of date.
        assert_eq!(
            sync(&mut c, seq, usdc_cents(500), Some(usdc_cents(1000))),
            0
        );
    }

    #[test]
    fn reconcile_leaves_other_payments_reserved() {
        let mut c = setup();
        pay(&mut c, vendor(), usdc_cents(25));
        pay(&mut c, vendor(), usdc_cents(10));
        claim(&mut c, 0, true, NANOS_PER_DAY);
        assert_eq!(c.reserved, usdc_cents(10).0);
        assert_eq!(c.get_log_entry(1).unwrap().status, Status::Pending);
        settle(&mut c, 1, true);
        assert_eq!(c.reserved, 0);
        assert_eq!(c.balance, usdc_cents(965).0);
    }

    #[test]
    fn withdraw_after_reconcile_is_limited_to_the_new_balance() {
        for (moved, balance) in [(true, 975), (false, 1000)] {
            let mut c = setup();
            pay(&mut c, vendor(), usdc_cents(25));
            let seq = c.settle_seq;
            claim(&mut c, 0, moved, NANOS_PER_DAY);
            if !moved {
                finish_claim(&mut c, 0, seq, Some(usdc_cents(1000)));
            }
            let text = refused(&mut c, |c| {
                call_as(owner(), 0);
                c.withdraw(U128(usdc_cents(balance).0 + 1));
            });
            assert!(text.contains("insufficient balance"), "{text}");
            call_as(owner(), 0);
            c.withdraw(usdc_cents(balance));
            assert_eq!(c.balance, 0);
        }
    }

    /// Small seeded generator, so a failure repeats without a new dependency.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self, below: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % below
        }
    }

    /// Compares the books with a model of what the token holds, across random
    /// payments, deposits, withdrawals, settled transfers, lost callbacks and
    /// truthful claims.
    #[test]
    fn random_calls_with_reconcile_keep_the_books_equal_to_the_tokens() {
        for seed in 1..=8u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let mut c = setup();
            let mut held = c.balance;
            let mut now = 0u64;
            call_as(owner(), now);
            c.set_agent(
                agent(),
                U128(u128::from(u64::MAX)),
                U128(u128::from(u64::MAX)),
                None,
            );
            let mut in_flight: Vec<(u32, u128)> = Vec::new();
            let mut stuck: Vec<(u32, u128, bool)> = Vec::new();

            for step in 0..400 {
                match rng.next(7) {
                    0 | 1 => {
                        let amount = 1 + u128::from(rng.next(500_000));
                        if amount <= c.balance {
                            call_as(agent(), now);
                            let id = c.get_log_length();
                            let paid = c.pay(vendor(), U128(amount), None);
                            assert!(outcome(paid).is_none(), "seed {seed} step {step}");
                            in_flight.push((id, amount));
                        }
                    }
                    2 if !in_flight.is_empty() => {
                        let (id, amount) = in_flight.remove(0);
                        let ok = rng.next(2) == 0;
                        if ok {
                            held -= amount;
                        }
                        settle(&mut c, id, ok);
                    }
                    3 if !in_flight.is_empty() => {
                        // The callback is lost. The transfer may or may not have moved.
                        let (id, amount) = in_flight.remove(0);
                        let moved = rng.next(2) == 0;
                        if moved {
                            held -= amount;
                        }
                        stuck.push((id, amount, moved));
                    }
                    4 => {
                        now += NANOS_PER_DAY;
                        // A claim for `moved = false` passes only when nothing
                        // else is stuck with moved tokens, so settle those first.
                        stuck.sort_by_key(|s| !s.2);
                        for (id, _, moved) in stuck.drain(..) {
                            let seq = c.settle_seq;
                            let out = claim(&mut c, id, moved, now);
                            if !moved {
                                assert!(outcome(out).is_none());
                                finish_claim(&mut c, id, seq, Some(U128(held)));
                            }
                        }
                    }
                    5 => {
                        let amount = 1 + u128::from(rng.next(100_000));
                        call_as(token(), now);
                        c.ft_on_transfer(owner(), U128(amount), String::new());
                        held += amount;
                    }
                    _ if c.balance > 0 => {
                        let amount = 1 + u128::from(rng.next(100_000)).min(c.balance - 1);
                        call_as(owner(), now);
                        c.withdraw(U128(amount));
                        held -= amount;
                        as_callback(PromiseResult::Successful(vec![]));
                        c.on_withdraw(U128(amount));
                    }
                    _ => {}
                }
                let reserved: u128 = in_flight.iter().map(|p| p.1).sum::<u128>()
                    + stuck.iter().map(|s| s.1).sum::<u128>();
                let moved: u128 = stuck.iter().filter(|s| s.2).map(|s| s.1).sum();
                assert_eq!(c.reserved, reserved, "seed {seed} step {step}");
                assert_eq!(counted(&c), held + moved, "seed {seed} step {step}");
            }
        }
    }
}
