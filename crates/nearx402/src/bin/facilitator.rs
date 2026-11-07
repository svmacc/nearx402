//! Standalone x402 facilitator for the `near-policy` scheme.
//!
//! Vendors on any stack (Node, Python, Rust) send it the client's `X-PAYMENT`
//! and their payment requirements; it checks the payment on NEAR.
//!
//!   cargo run -p nearx402 --bin facilitator -- \
//!       --token-id usdc.example.testnet --policy-wasm res/policy.wasm \
//!       --api-key "$(openssl rand -hex 32):data-api.example.testnet"
//!
//! Endpoints: `GET /supported` (public), `POST /verify` and `POST /settle`
//! (`Authorization: Bearer <api key>`).

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use near_api::types::{AccountId, CryptoHash};
use near_api::NetworkConfig;
use nearx402::facilitator::{self, ApiKey, Facilitator, FacilitatorConfig};
use tokio::net::TcpListener;

#[derive(Parser)]
#[command(about = "x402 facilitator for nearx402 near-policy payments")]
struct Args {
    /// NEAR RPC endpoint.
    #[arg(
        long,
        env = "NEAR_RPC_URL",
        default_value = "https://rpc.testnet.near.org"
    )]
    rpc_url: String,
    /// x402 network name reported to clients.
    #[arg(long, env = "X402_NETWORK", default_value = "near-testnet")]
    network: String,
    /// NEP-141 token the policy contracts must pay in.
    #[arg(long, env = "TOKEN_ID")]
    token_id: AccountId,
    /// Trusted policy code hash (base58), as shown by `near state` or the demo.
    #[arg(long, env = "POLICY_CODE_HASH", conflicts_with = "policy_wasm")]
    policy_code_hash: Option<String>,
    /// Or: compute the trusted hash from the policy wasm you built.
    #[arg(long, env = "POLICY_WASM")]
    policy_wasm: Option<PathBuf>,
    /// Address to listen on. Use 0.0.0.0 on a server.
    #[arg(long, env = "HOST", default_value = "127.0.0.1")]
    host: IpAddr,
    #[arg(long, env = "PORT", default_value_t = 8403)]
    port: u16,
    /// Vendor credential `KEY:vendor.testnet[,other.testnet]`. Repeat for
    /// more vendors; in the env var, separate entries with spaces.
    #[arg(long = "api-key", env = "FACILITATOR_API_KEYS", value_delimiter = ' ')]
    api_keys: Vec<String>,
    /// Run without API keys: anyone can call /verify and /settle. Local use only.
    #[arg(long, conflicts_with = "api_keys")]
    allow_anonymous: bool,
    /// Where redeemed payments are kept, so a restart cannot redeem one twice.
    #[arg(long, env = "REDEEMED_FILE", default_value = "redeemed.tsv")]
    redeemed_file: PathBuf,
    /// Refuse payments older than this many seconds. Must exceed the vendors'
    /// nonce lifetime (300 s).
    #[arg(long, env = "MAX_PAYMENT_AGE_SECS", default_value_t = facilitator::DEFAULT_MAX_PAYMENT_AGE.as_secs())]
    max_payment_age_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let trusted_policy_code_hash = match (&args.policy_code_hash, &args.policy_wasm) {
        (Some(h), _) => h
            .parse::<CryptoHash>()
            .map_err(|e| anyhow!("--policy-code-hash is not a base58 hash: {e:?}"))?,
        (None, Some(path)) => CryptoHash::hash(
            &std::fs::read(path).with_context(|| format!("read {}", path.display()))?,
        ),
        (None, None) => {
            return Err(anyhow!(
                "pass --policy-code-hash or --policy-wasm (for example res/policy.wasm)"
            ))
        }
    };

    let api_keys = args
        .api_keys
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| ApiKey::parse(s))
        .collect::<Result<Vec<_>>>()?;
    if api_keys.is_empty() && !args.allow_anonymous {
        return Err(anyhow!(
            "pass --api-key KEY:vendor.testnet (generate one with: openssl rand -hex 32), \
             or --allow-anonymous for local testing"
        ));
    }

    let cfg = FacilitatorConfig {
        network: NetworkConfig::from_rpc_url("custom", args.rpc_url.parse()?),
        rpc_url: args.rpc_url.clone(),
        network_name: args.network.clone(),
        token_id: args.token_id.clone(),
        trusted_policy_code_hash,
        api_keys: api_keys.clone(),
        max_payment_age: Duration::from_secs(args.max_payment_age_secs),
        redeemed_file: Some(args.redeemed_file.clone()),
    };
    if args.max_payment_age_secs <= 300 {
        return Err(anyhow!(
            "--max-payment-age-secs must exceed the 300 s vendor nonce lifetime"
        ));
    }
    let app = facilitator::router(Arc::new(Facilitator::new(cfg)?));
    let listener = TcpListener::bind((args.host, args.port)).await?;
    println!(
        "x402 facilitator (near-policy) on http://{}\n  network {} via {}\n  token {}\n  trusted policy code {}",
        listener.local_addr()?,
        args.network,
        args.rpc_url,
        args.token_id,
        trusted_policy_code_hash
    );
    println!(
        "  redeemed payments in {} (max payment age {} s)",
        args.redeemed_file.display(),
        args.max_payment_age_secs
    );
    if api_keys.is_empty() {
        println!("  WARNING: no API keys, anyone can call /verify and /settle");
    }
    for k in &api_keys {
        let vendors: Vec<String> = k.vendors.iter().map(|v| v.to_string()).collect();
        let prefix: String = k.key.chars().take(4).collect();
        println!("  API key {prefix}… for {}", vendors.join(", "));
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
