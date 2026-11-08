//! Private-policy demo, from nothing on a local NEAR sandbox, in one command:
//!
//!   scripts/demo-zk.sh            run the story and exit
//!   scripts/demo-zk.sh --serve    then keep the public dashboard running
//!
//! Or against contracts that are already deployed (see `docs/testnet.md`). There
//! it never prints the private policy, so that a recording does not reveal it:
//!
//!   scripts/demo-zk.sh --network testnet --config deploy/testnet.json
//!
//! An AI agent buys data over x402, proving to the contract that the payment
//! obeys a spending policy the chain never sees. It is then told to redirect a
//! payment to a vendor outside that policy, and the contract refuses.
//!
//! What the demo reports comes from the chain: transactions are read back by
//! hash, and balances from the token. If the outcome differs from the story, the
//! demo stops with an error instead of telling it anyway.
//!
//! Vendor and amount stay public in every token transfer. Only the policy is
//! private. The token is a mock with no value, and the trusted setup is local.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, ensure, Context, Result};
use ark_std::rand::rngs::OsRng;
use clap::{Parser, ValueEnum};
use near_api::types::{AccountId, CryptoHash};
use near_api::{NetworkConfig, Signer};
use nearx402::agent::{payment_http_client, quote};
use nearx402::deployment::{self, check_limited_keys, DemoConfig, PolicyParams, SetupPaths};
use nearx402::facilitator::{self, FacilitatorConfig};
use nearx402::policy::{PayReceipt, PayZkArgs, PayZkResult, PolicyClient, Status};
use nearx402::rejection;
use nearx402::server;
use nearx402::vendor::{market_data_vendor, VendorConfig};
use nearx402::world::{World, DAILY_CAP, PER_CALL_MAX, WALLET_FUNDING};
use nearx402::x402::{self, NearPolicyProof, PaymentPayload};
use nearx402::zk::{
    access_keys, b64, chain_agent, chain_id, contains, contract_storage, pay_args, token_balance,
    tx_evidence, verifying_key_args, TxEvidence,
};
use serde_json::{json, Value};
use zx402_zk::ark_bn254::Bn254;
use zx402_zk::ark_groth16::ProvingKey;
use zx402_zk::policy::{self, PreparedPayment, ZkWallet};
use zx402_zk::proof_bytes;
use zx402_zk::store::{load_proving_key, Reconciled, WalletFile};

/// The market data the agent buys, in token base units.
const PRICE: u128 = 50_000;
/// Rejected attempts one run makes, each counted by the circuit breaker.
const REJECTED_ATTEMPTS: u32 = 2;

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Network {
    /// A clean local sandbox, created for this run.
    Sandbox,
    /// Contracts already deployed, named in `--config`.
    Testnet,
}

#[derive(Parser)]
#[command(about = "Private spend policy demo")]
struct Args {
    /// Keep the vendors and public dashboard running after the story.
    #[arg(long)]
    serve: bool,
    /// Port for the vendors and the dashboard.
    #[arg(long, default_value_t = 8402)]
    port: u16,
    #[arg(long, value_enum, default_value_t = Network::Sandbox)]
    network: Network,
    /// The deployment to use with `--network testnet`.
    #[arg(long, default_value = "deploy/testnet.json")]
    config: PathBuf,
}

struct Ui {
    color: bool,
}

impl Ui {
    fn paint(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    fn scene(&self, n: u32, title: &str) {
        println!(
            "\n{} {}",
            self.paint("1;36", &format!("[{n}]")),
            self.paint("1", title)
        );
    }
    fn line(&self, label: &str, msg: &str) {
        println!("    {:<14} {msg}", self.paint("2", label));
    }
    fn good(&self, label: &str, msg: &str) {
        println!("    {} {msg}", self.paint("1;32", &format!("{label:<14}")));
    }
    fn bad(&self, label: &str, msg: &str) {
        println!("    {} {msg}", self.paint("1;31", &format!("{label:<14}")));
    }
}

/// Formats token base units (6 decimals), keeping every significant digit.
fn musdc(amount: u128) -> String {
    let frac = format!("{:06}", amount % 1_000_000);
    let trimmed = frac.trim_end_matches('0');
    let digits = if trimmed.len() < 2 {
        &frac[..2]
    } else {
        trimmed
    };
    format!("{}.{digits} mUSDC", amount / 1_000_000)
}

fn describe(event: &Value) -> String {
    let name = event["event"].as_str().unwrap_or("?");
    let d = &event["data"][0];
    match name {
        "ft_transfer" => format!(
            "NEP-141 ft_transfer: {} -> {}, {}",
            d["old_owner_id"].as_str().unwrap_or("?"),
            d["new_owner_id"].as_str().unwrap_or("?"),
            musdc(
                d["amount"]
                    .as_str()
                    .and_then(|a| a.parse().ok())
                    .unwrap_or(0)
            ),
        ),
        "zk_proof_verified" => "zk_proof_verified: the Groth16 check passed".into(),
        "payment_blocked" => format!(
            "payment_blocked: {}",
            d["reason"].as_str().unwrap_or("no reason")
        ),
        other => other.to_string(),
    }
}

/// Prints a transaction as the node reports it.
fn show_tx(ui: &Ui, tx: &TxEvidence) {
    ui.line("transaction", &tx.hash);
    ui.line(
        "call",
        &format!("{} -> {}.{}", tx.signer, tx.receiver, tx.method),
    );
    for step in &tx.steps {
        for event in &step.events {
            ui.line(
                "on chain",
                &format!("{}: {}", step.executor, describe(event)),
            );
        }
    }
    ui.line(
        "gas",
        &format!(
            "{:.2} Tgas for the whole transaction, {} receipts",
            tx.total_gas() as f64 / 1e12,
            tx.steps.len()
        ),
    );
}

/// Forces a proof for any statement, as a compromised agent would.
fn forced_proof(
    wallet: &ZkWallet,
    pk: &ProvingKey<Bn254>,
    vendor: &AccountId,
    amount: u128,
    memo: &str,
    day: u64,
) -> Result<PreparedPayment> {
    let (public, witness, next) =
        wallet.statement(vendor.as_str(), amount, memo, day, &mut OsRng)?;
    Ok(PreparedPayment {
        proof: policy::prove(pk, public, witness, &mut OsRng)?,
        public,
        next,
    })
}

/// A directory for one sandbox run's files, removed when the run ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self> {
        let dir = std::env::temp_dir().join(format!("zx402-demo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Everything the story needs. How it gets here differs by network; the story
/// does not, and the wallet goes through its file on both.
struct Stage {
    /// Keeps a local sandbox alive, if this run made one.
    _world: Option<World>,
    _scratch: Option<Scratch>,
    /// Whether the output may show the private policy. Only for a throwaway
    /// sandbox policy: a recorded testnet run must not reveal the real one.
    reveal: bool,
    /// Where the evidence comes from, for the closing line.
    source: String,
    network: NetworkConfig,
    rpc_url: String,
    policy_id: AccountId,
    token_id: AccountId,
    data_api: AccountId,
    attacker: AccountId,
    agent_id: AccountId,
    signer: Arc<Signer>,
    vendors: VendorConfig,
    facilitator: FacilitatorConfig,
    pk: ProvingKey<Bn254>,
    wallet: WalletFile,
    explorer_tx_url: Option<String>,
}

fn read_json(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

/// Creates everything on a clean local sandbox, and shows finance's side.
async fn sandbox_stage(ui: &Ui) -> Result<Stage> {
    println!("Starting a clean local NEAR sandbox (no accounts or funds needed)...");
    let started = Instant::now();
    let w = World::start_bare().await?;
    println!("Ready in {:.1}s.", started.elapsed().as_secs_f32());
    let data_api = w.vendor("data-api").account_id.clone();
    let gpu_cloud = w.vendor("gpu-cloud").account_id.clone();

    ui.scene(0, "Finance sets a private spending policy");
    ui.line(
        "off chain",
        &format!(
            "cap {} per day, {} per call, allowed vendors {data_api} and {gpu_cloud}",
            musdc(DAILY_CAP),
            musdc(PER_CALL_MAX)
        ),
    );
    ui.line(
        "note",
        "these limits are printed here as narration; they are never sent to the chain",
    );
    let (agent_id, signer) = w.create_zk_agent("zk-agent").await?;
    let scratch = Scratch::new()?;
    let t = Instant::now();
    let (paths, pk): (SetupPaths, _) = deployment::init(
        &scratch.0,
        &PolicyParams {
            contract: w.policy_id.clone(),
            agent: agent_id.clone(),
            cap: DAILY_CAP,
            per_call: PER_CALL_MAX,
            vendors: vec![data_api.clone(), gpu_cloud],
        },
    )?;
    ui.line(
        "setup",
        &format!(
            "Groth16 keys generated in {} ms. Local setup, not a ceremony: not production-safe",
            t.elapsed().as_millis()
        ),
    );
    w.owner_call(
        "set_zk_verifying_key",
        read_json(&paths.verifying_key_args)?,
    )
    .await?;
    w.owner_call("set_zk_agent", read_json(&paths.agent_args)?)
        .await?;
    let wallet = WalletFile::open(&paths.wallet)?;
    ui.line(
        "on chain",
        &format!(
            "{} holds {} and only a commitment: {}",
            w.policy_id,
            musdc(WALLET_FUNDING),
            b64(wallet.wallet.opening.commitment()?)
        ),
    );
    ui.line(
        "agent key",
        &format!("{agent_id} can only call policy.pay_zk; it holds no funds"),
    );

    Ok(Stage {
        reveal: true,
        source: "the local sandbox".into(),
        network: w.network.clone(),
        rpc_url: w.rpc_url.clone(),
        policy_id: w.policy_id.clone(),
        token_id: w.token_id.clone(),
        attacker: w.attacker_id.clone(),
        data_api,
        agent_id,
        signer,
        vendors: w.vendor_config(),
        facilitator: w.facilitator_config(),
        pk,
        wallet,
        explorer_tx_url: None,
        _world: Some(w),
        _scratch: Some(scratch),
    })
}

/// Checks a deployment before spending anything. Each check stops a run that
/// would otherwise burn gas, count against the breaker, or claim something false.
async fn deployed_stage(ui: &Ui, path: &Path) -> Result<Stage> {
    let cfg = DemoConfig::load(path)?;
    let chain = chain_id(&cfg.rpc_url).await.context("reach the RPC")?;
    ensure!(
        chain != "mainnet",
        "this demo uses a mock token with an open faucet, and must never run on mainnet"
    );
    let network = NetworkConfig::from_rpc_url("custom", cfg.rpc_url.parse()?);
    let client = PolicyClient::new(network.clone(), cfg.policy.clone());

    ui.scene(0, "Checking the deployment before spending anything");
    // Scheme and host only: a recording must not show an API key in the URL.
    ui.line(
        "chain",
        &format!("{chain}, through {}", rejection::public_url(&cfg.rpc_url)),
    );

    let wasm = std::fs::read(&cfg.policy_wasm)
        .with_context(|| format!("read {}", cfg.policy_wasm.display()))?;
    let trusted = CryptoHash::hash(&wasm);
    let deployed = client.code_hash().await?;
    ensure!(
        deployed == trusted,
        "{} runs code hash {deployed}, and {} has {trusted}. Point policy_wasm at the build \
         you deployed",
        cfg.policy,
        cfg.policy_wasm.display()
    );
    let token = client.token_id().await?;
    ensure!(
        token == cfg.token,
        "{} pays in {token}, not {}",
        cfg.policy,
        cfg.token
    );
    ui.line(
        "contract",
        &format!("{} runs code {deployed}, paying in {token}", cfg.policy),
    );
    let policy_keys = access_keys(&cfg.rpc_url, &cfg.policy).await?;
    let locked = policy_keys["keys"].as_array().is_some_and(Vec::is_empty);
    ui.line(
        "lock",
        if locked {
            "the contract has no access keys, so its code cannot change"
        } else {
            "the contract still has an access key, so its owner can change its code"
        },
    );

    let keys = access_keys(&cfg.rpc_url, &cfg.agent).await?;
    let count = check_limited_keys(&keys, &cfg.policy, "pay_zk")?;
    let signer = Signer::from_access_keyfile(cfg.agent_key_file.clone())
        .with_context(|| format!("load the agent key {}", cfg.agent_key_file.display()))?;
    ui.line(
        "agent key",
        &format!(
            "{}: read from the chain, {count} key(s), each limited to {}.pay_zk",
            cfg.agent, cfg.policy
        ),
    );

    let pk = load_proving_key(&cfg.agent_dir.join("proving.key"))?;
    let on_chain = client
        .zk_verifying_key()
        .await?
        .context("the contract has no verifying key; finance must send set_zk_verifying_key")?;
    ensure!(
        on_chain == verifying_key_args(&pk.vk),
        "this proving key comes from another setup than the contract's verifying key, so \
         every proof would fail and count against the breaker"
    );
    ui.line(
        "keys",
        "the proving key matches the contract's verifying key",
    );

    let mut wallet = WalletFile::open(&cfg.agent_dir.join("wallet.json"))?;
    ensure!(
        wallet.wallet.contract == cfg.policy.as_str() && wallet.wallet.agent == cfg.agent.as_str(),
        "this wallet file belongs to {} on {}",
        wallet.wallet.agent,
        wallet.wallet.contract
    );
    ensure!(
        wallet
            .wallet
            .opening
            .vendors
            .iter()
            .any(|v| v == cfg.vendor.as_str()),
        "{} is not on this wallet's allowlist",
        cfg.vendor
    );
    ensure!(
        !wallet
            .wallet
            .opening
            .vendors
            .iter()
            .any(|v| v == cfg.outside_recipient.as_str()),
        "{} is on this wallet's allowlist, so it is not outside the policy",
        cfg.outside_recipient
    );
    let view = client.zk_agent(&cfg.agent).await?.with_context(|| {
        format!(
            "{} is not registered; finance must send set_zk_agent",
            cfg.agent
        )
    })?;
    let reconciled = wallet.reconcile(&chain_agent(&view)?)?;
    ui.line(
        "wallet file",
        match reconciled {
            Reconciled::Current => "agrees with the chain's commitments",
            Reconciled::Adopted => {
                "caught up with the chain: an earlier run's payment landed after it stopped"
            }
        },
    );

    ensure!(!view.frozen, "{} is frozen", cfg.agent);
    ensure!(
        view.blocked_today + REJECTED_ATTEMPTS < view.max_blocked_per_day,
        "the circuit breaker is at {} of {} today, and this run's {REJECTED_ATTEMPTS} rejected \
         attempts would freeze the agent. Run again after midnight UTC",
        view.blocked_today,
        view.max_blocked_per_day
    );
    let balance = client.balance().await?;
    ensure!(
        balance >= PRICE,
        "the wallet holds {}, and this run spends {}",
        musdc(balance),
        musdc(PRICE)
    );
    ui.line(
        "wallet",
        &format!(
            "{}, with the policy itself held off chain and not printed",
            musdc(balance)
        ),
    );

    let vendors = VendorConfig {
        network_name: cfg.network_name.clone(),
        token_id: cfg.token.clone(),
        trusted_policy_code_hash: trusted,
        vendors: vec![market_data_vendor(cfg.vendor.clone())],
        nonce_secret: rand::random(),
    };
    let facilitator = FacilitatorConfig {
        network: network.clone(),
        rpc_url: cfg.rpc_url.clone(),
        network_name: cfg.network_name.clone(),
        token_id: cfg.token.clone(),
        trusted_policy_code_hash: trusted,
        api_keys: vec![],
        max_payment_age: facilitator::DEFAULT_MAX_PAYMENT_AGE,
        redeemed_file: None,
    };
    Ok(Stage {
        _world: None,
        _scratch: None,
        reveal: false,
        source: format!("chain {chain}"),
        network,
        rpc_url: cfg.rpc_url,
        policy_id: cfg.policy,
        token_id: cfg.token,
        data_api: cfg.vendor,
        attacker: cfg.outside_recipient,
        agent_id: cfg.agent,
        signer,
        vendors,
        facilitator,
        pk,
        wallet,
        explorer_tx_url: cfg.explorer_tx_url,
    })
}

struct Run<'a> {
    network: &'a NetworkConfig,
    client: &'a PolicyClient,
    rpc_url: &'a str,
    token_id: &'a AccountId,
    ui: &'a Ui,
    agent_id: AccountId,
    signer: Arc<Signer>,
    transactions: server::Transactions,
    explorer_tx_url: Option<String>,
}

impl Run<'_> {
    /// Sends `pay_zk`, reads the transaction back from the node, and publishes
    /// it for the dashboard.
    async fn send(&self, args: &PayZkArgs) -> Result<(PayReceipt, TxEvidence)> {
        let result = self
            .client
            .pay_zk(&self.agent_id, self.signer.clone(), args)
            .await?;
        let receipt = match result {
            PayZkResult::Executed { receipt, .. } => receipt,
            PayZkResult::Reverted { reason, .. } => bail!("pay_zk reverted: {reason}"),
        };
        let tx = tx_evidence(self.rpc_url, &receipt.tx_hash, &self.agent_id).await?;
        show_tx(self.ui, &tx);
        if let Some(url) = &self.explorer_tx_url {
            self.ui.line("explorer", &url.replace("{hash}", &tx.hash));
        }
        if let Ok(mut list) = self.transactions.lock() {
            list.push(json!({ "log_id": receipt.outcome.log_id, "tx_hash": receipt.tx_hash }));
        }
        Ok((receipt, tx))
    }

    async fn balance(&self, account: &AccountId) -> Result<u128> {
        token_balance(self.network, self.token_id, account).await
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let ui = Ui {
        color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    };

    let stage = match args.network {
        Network::Sandbox => sandbox_stage(&ui).await?,
        Network::Testnet => deployed_stage(&ui, &args.config).await?,
    };
    let Stage {
        _world,
        _scratch,
        reveal,
        source,
        network,
        rpc_url,
        policy_id,
        token_id,
        data_api,
        attacker,
        agent_id,
        signer,
        vendors,
        facilitator,
        pk,
        mut wallet,
        explorer_tx_url,
    } = stage;

    let network_name = vendors.network_name.clone();
    let served = server::serve(
        args.port,
        vendors,
        facilitator,
        rpc_url.clone(),
        policy_id.to_string(),
    )
    .await?;
    let base = format!("http://{}", served.addr);
    let http = payment_http_client();
    let policy_client = PolicyClient::new(network.clone(), policy_id.clone());
    // Compared with each offer, before proving or signing anything.
    let terms = policy_client.accepted_terms(&network_name).await?;
    let data_url = format!("{base}/data-api/market-data");

    let run = Run {
        network: &network,
        client: &policy_client,
        rpc_url: &rpc_url,
        token_id: &token_id,
        ui: &ui,
        agent_id: agent_id.clone(),
        signer,
        transactions: served.transactions.clone(),
        explorer_tx_url,
    };
    let day = policy_client.day().await?;

    // ---------- scene 1 ----------
    ui.scene(1, "The agent buys market data for 0.05 mUSDC");
    let vendor_before = run.balance(&data_api).await?;
    let treasury_before = policy_client.balance().await?;

    let offer = quote(&http, &data_url, &terms).await?;
    let (req, price) = (offer.requirements, offer.price);
    let nonce = req.extra.nonce.clone();
    ensure!(
        price == PRICE,
        "expected a price of 0.05 mUSDC, got {price}"
    );
    ensure!(req.pay_to == data_api.as_str(), "unexpected payTo");
    ui.good(
        "HTTP 402",
        &format!(
            "GET /data-api/market-data: pay {} to {}",
            musdc(price),
            req.pay_to
        ),
    );

    // Stays with the agent until it claims: the public memo only commits to it.
    let receipt_secret = x402::new_receipt_secret();
    let memo = x402::payment_memo(&nonce, &receipt_secret)?;

    let t = Instant::now();
    let payment = wallet
        .wallet
        .prepare(&pk, data_api.as_str(), price, &memo, day, &mut OsRng)?
        .map_err(|refusal| anyhow::anyhow!("the wallet refused an honest payment: {refusal}"))?;
    ui.line(
        "proof",
        &format!(
            "built on the agent's machine in {} ms, {} bytes",
            t.elapsed().as_millis(),
            proof_bytes(&payment.proof).len()
        ),
    );

    // Saved before sending: a crash from here on can be recovered from the chain.
    wallet.begin(payment.next)?;
    let (receipt, tx) = run
        .send(&pay_args(&data_api, price, &memo, &payment, day))
        .await?;
    ensure!(
        receipt.outcome.status == Status::Paid,
        "payment did not settle: {:?}",
        receipt.outcome
    );
    ensure!(
        tx.has_event("zk_proof_verified"),
        "no proof-verified event on chain"
    );
    ensure!(
        tx.transfers(&token_id) == vec![(data_api.to_string(), price)],
        "token transfer on chain differs: {:?}",
        tx.transfers(&token_id)
    );
    let settled = policy_client
        .zk_agent(&agent_id)
        .await?
        .context("the agent is registered")?;
    ensure!(
        wallet.reconcile(&chain_agent(&settled)?)? == Reconciled::Adopted,
        "the chain did not move to the state this payment proved"
    );
    ui.good(
        "PROOF VERIFIED",
        &format!(
            "the contract checked the proof, then paid: log #{}",
            receipt.outcome.log_id
        ),
    );

    let vendor_after = run.balance(&data_api).await?;
    ensure!(
        vendor_after == vendor_before + price,
        "vendor balance is wrong"
    );
    ui.line(
        "vendor",
        &format!(
            "{data_api}: {} -> {}",
            musdc(vendor_before),
            musdc(vendor_after)
        ),
    );

    let claim = |secret: String| {
        x402::encode(&PaymentPayload {
            x402_version: x402::VERSION,
            scheme: req.scheme.clone(),
            network: req.network.clone(),
            payload: NearPolicyProof {
                policy_contract: policy_id.to_string(),
                log_id: receipt.outcome.log_id,
                nonce: nonce.clone(),
                tx_hash: receipt.tx_hash.clone(),
                receipt_secret: secret,
            },
        })
    };

    // Everything but the secret can be read from the chain, so an observer can
    // try to claim this payment first.
    // Each refusal below is read from the vendor's 402 body, so a refusal for
    // another reason, such as an RPC rate limit, is not mistaken for the check
    // being shown. Nothing here retries a claim or a payment.
    let observer_secret = x402::new_receipt_secret();
    let observer = http
        .get(&data_url)
        .header(x402::PAYMENT_HEADER, claim(observer_secret.clone())?)
        .send()
        .await?;
    let observer_status = observer.status();
    ensure!(
        observer_status == 402,
        "someone without the secret claimed the payment: HTTP {observer_status}"
    );
    let observer = rejection::explain(
        observer_status.as_u16(),
        &observer.text().await.unwrap_or_default(),
        &[&observer_secret, &receipt_secret],
    );
    if observer.cause == rejection::Cause::SecretMismatch {
        ui.line(
            "observer",
            "rebuilds the receipt from public data, without the agent's secret: HTTP 402, the secret does not match",
        );
    } else {
        ui.bad(
            "observer",
            &format!("refused, but not for the secret: {observer}. This does not show the secret's protection"),
        );
    }

    let paid = format!(
        "paid on chain as log #{}, transaction {}",
        receipt.outcome.log_id, receipt.tx_hash
    );
    let payment_header = claim(receipt_secret.clone())?;
    let served_data = http
        .get(&data_url)
        .header(x402::PAYMENT_HEADER, &payment_header)
        .send()
        .await?;
    let served_status = served_data.status();
    if served_status != 200 {
        let why = rejection::explain(
            served_status.as_u16(),
            &served_data.text().await.unwrap_or_default(),
            &[&receipt_secret],
        );
        bail!("paid but not served: {paid}, and the vendor refused the claim: {why}. Do not pay again; nothing was retried");
    }
    ui.good(
        "HTTP 200",
        &format!("the vendor served: {}", served_data.text().await?),
    );
    ensure!(
        observer.cause == rejection::Cause::SecretMismatch,
        "the observer was refused for another reason ({observer}), so this run does not show that the secret protects the payment"
    );

    let replay = http
        .get(&data_url)
        .header(x402::PAYMENT_HEADER, &payment_header)
        .send()
        .await?;
    let replay_status = replay.status();
    ensure!(replay_status == 402, "a receipt must only work once");
    let replay = rejection::explain(
        replay_status.as_u16(),
        &replay.text().await.unwrap_or_default(),
        &[&receipt_secret],
    );
    ensure!(
        replay.cause == rejection::Cause::AlreadyRedeemed,
        "the replay was refused for another reason ({replay}), so this run does not show single use"
    );
    ui.line(
        "replay",
        "the same receipt again: HTTP 402, already redeemed, a receipt buys one response",
    );

    // ---------- scene 2 ----------
    ui.scene(
        2,
        "A compromised agent tries to redirect a payment outside the policy",
    );
    let attacker_before = run.balance(&attacker).await?;
    let treasury_mid = policy_client.balance().await?;
    let state_before = wallet.wallet.state.commitment();
    let blocked_before = settled.blocked_today;
    ui.line(
        "before",
        &format!(
            "{attacker}: {}. Wallet: {}",
            musdc(attacker_before),
            musdc(treasury_mid)
        ),
    );

    let refusal = wallet
        .wallet
        .prepare(&pk, attacker.as_str(), price, "injected", day, &mut OsRng)?
        .err()
        .context("an honest wallet must refuse this payment")?;
    ui.line(
        "honest wallet",
        &format!("refuses to prove: {refusal}. Nothing is sent"),
    );

    println!(
        "\n    {}",
        ui.paint("1", "Attempt A: run the prover anyway, naming the attacker")
    );
    let forged = forced_proof(&wallet.wallet, &pk, &attacker, price, "injected", day)?;
    let (a, tx_a) = run
        .send(&pay_args(&attacker, price, "injected", &forged, day))
        .await?;

    println!(
        "\n    {}",
        ui.paint(
            "1",
            "Attempt B: take a valid proof for data-api, send it to the attacker"
        )
    );
    let req_b = quote(&http, &data_url, &terms).await?.requirements;
    let valid = forced_proof(
        &wallet.wallet,
        &pk,
        &data_api,
        price,
        &req_b.extra.nonce,
        day,
    )?;
    let (b, tx_b) = run
        .send(&pay_args(&attacker, price, &req_b.extra.nonce, &valid, day))
        .await?;

    for (name, outcome, tx) in [("A", &a, &tx_a), ("B", &b, &tx_b)] {
        ensure!(
            outcome.outcome.status == Status::Blocked
                && outcome.outcome.reason.as_deref() == Some("invalid proof"),
            "attempt {name} was not rejected: {:?}",
            outcome.outcome
        );
        ensure!(
            tx.transfers(&token_id).is_empty(),
            "attempt {name} moved tokens"
        );
        ensure!(
            !tx.has_event("zk_proof_verified"),
            "attempt {name} passed verification"
        );
    }
    let attacker_after = run.balance(&attacker).await?;
    let treasury_after = policy_client.balance().await?;
    let view = policy_client
        .zk_agent(&agent_id)
        .await?
        .context("the agent is registered")?;
    ensure!(attacker_after == attacker_before, "the attacker was paid");
    ensure!(treasury_after == treasury_mid, "the wallet balance moved");
    ensure!(
        view.state_commitment == b64(state_before),
        "the rejected attempts changed the spending state"
    );
    ensure!(
        view.blocked_today == blocked_before + REJECTED_ATTEMPTS,
        "both attempts must be counted"
    );
    println!();
    ui.bad(
        "REJECTED",
        "the contract refused both: invalid proof. No token transfer in either transaction",
    );
    ui.line(
        "after",
        &format!(
            "{attacker}: {} -> {}. Wallet: {} -> {}",
            musdc(attacker_before),
            musdc(attacker_after),
            musdc(treasury_mid),
            musdc(treasury_after)
        ),
    );
    ui.line(
        "breaker",
        &format!(
            "{} of {} blocked attempts today; at the limit the agent freezes",
            view.blocked_today, view.max_blocked_per_day
        ),
    );

    // ---------- scene 3 ----------
    ui.scene(3, "What anyone can read on chain");
    let public_agents = serde_json::to_value(
        near_api::Contract(policy_id.clone())
            .call_function("get_agents", json!({}))
            .read_only::<Value>()
            .fetch_from(&network)
            .await?
            .data,
    )?;
    let public_vendors = near_api::Contract(policy_id.clone())
        .call_function("get_vendors", json!({}))
        .read_only::<Value>()
        .fetch_from(&network)
        .await?
        .data;
    ensure!(public_agents == json!([]), "an agent's limits are public");
    ensure!(public_vendors == json!([]), "an allowlist is public");
    ui.line("views", "get_agents() = [], get_vendors() = []");

    let storage = contract_storage(&rpc_url, &policy_id).await?;
    let opening = &wallet.wallet.opening;
    let named = |private: String, public: &str| {
        if reveal {
            private
        } else {
            public.to_string()
        }
    };
    let absent = |name: &str| {
        ui.good(
            "NOT IN STORAGE",
            &format!(
                "{name}: absent from all {} bytes of contract storage",
                storage.len()
            ),
        );
    };
    let unpaid: Vec<&String> = opening
        .vendors
        .iter()
        .filter(|v| v.as_str() != data_api.as_str())
        .collect();
    for vendor in &unpaid {
        ensure!(
            !contains(&storage, vendor.as_bytes()),
            "{} is in contract storage",
            named(vendor.to_string(), "an allowed vendor not yet paid")
        );
    }
    if reveal {
        unpaid.iter().for_each(|vendor| absent(vendor));
    } else if !unpaid.is_empty() {
        // One line whatever their number, which is private too.
        absent("allowed vendors not yet paid");
    }
    for (name, bytes) in [
        (
            named(format!("the cap, {}", musdc(opening.cap)), "the daily cap"),
            opening.cap.to_le_bytes(),
        ),
        (
            named(
                format!("the per-call limit, {}", musdc(opening.per_call)),
                "the per-call limit",
            ),
            opening.per_call.to_le_bytes(),
        ),
    ] {
        ensure!(!contains(&storage, &bytes), "{name} is in contract storage");
        absent(&name);
    }
    ensure!(
        contains(&storage, data_api.as_bytes()),
        "a paid vendor should be visible"
    );
    ensure!(
        contains(&storage, attacker.as_bytes()),
        "a rejected recipient should be visible"
    );
    ui.bad(
        "STILL PUBLIC",
        &format!("{data_api} and the amount paid: every token transfer shows them"),
    );
    ui.bad(
        "STILL PUBLIC",
        &format!("{attacker}: a rejected attempt's recipient is in its arguments"),
    );
    ui.bad(
        "STILL PUBLIC",
        "a paid vendor is visibly allowed, and a day's payments lower-bound the cap",
    );
    ui.line("agent", "the agent must hold the policy to prove it. A compromised agent reads it, but cannot break it");

    // ---------- summary ----------
    println!("\nResult: every check passed, read back from {source}.");
    println!(
        "    paid {} to {data_api}; {attacker} received {}; wallet {} -> {}",
        musdc(vendor_after - vendor_before),
        musdc(attacker_after - attacker_before),
        musdc(treasury_before),
        musdc(treasury_after)
    );

    if args.serve {
        println!("\nPublic dashboard: {base}/   (Ctrl+C to stop)");
        tokio::signal::ctrl_c().await?;
    }
    served.handle.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::musdc;

    #[test]
    fn amounts_keep_every_significant_digit() {
        assert_eq!(musdc(50_000), "0.05 mUSDC");
        assert_eq!(musdc(49_999), "0.049999 mUSDC");
        assert_eq!(musdc(10_000_000), "10.00 mUSDC");
        assert_eq!(musdc(9_950_000), "9.95 mUSDC");
        assert_eq!(musdc(0), "0.00 mUSDC");
    }
}
