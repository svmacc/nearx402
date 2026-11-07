//! One-command demo on a local NEAR sandbox. No accounts or funds needed.
//!
//!   scripts/build-contracts.sh && cargo run -p nearx402 --bin demo -- --serve
//!
//! Story: an AI agent buys market data and GPU time through x402. It tries an
//! unapproved news vendor (blocked), finance approves it, the retry succeeds.
//! The article carries a prompt injection telling the agent to pay an attacker:
//! the policy blocks it. The agent then tries to go around the policy with its
//! own key and NEAR itself refuses.

use std::io::IsTerminal;
use std::time::Instant;

use anyhow::Result;
use clap::Parser;
use near_api::types::{AccountId, NearGas, NearToken};
use near_api::Contract;
use nearx402::agent::{find_injected_payment, Fetch};
use nearx402::policy::{usd, PayReceipt, Status};
use nearx402::server;
use nearx402::world::{World, DAILY_CAP, PER_CALL_MAX};
use serde_json::json;

#[derive(Parser)]
#[command(about = "nearx402 spend policy demo on a local NEAR sandbox")]
struct Args {
    /// Keep the sandbox, vendors and dashboard running after the story.
    #[arg(long)]
    serve: bool,
    /// Port for the vendors and the dashboard.
    #[arg(long, default_value_t = 8402)]
    port: u16,
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
    fn step(&self, n: u32, title: &str) {
        println!(
            "\n{} {}",
            self.paint("1;36", &format!("[{n}]")),
            self.paint("1", title)
        );
    }
    fn ok(&self, msg: &str) {
        println!("    {} {msg}", self.paint("1;32", "PAID   "));
    }
    fn blocked(&self, msg: &str) {
        println!("    {} {msg}", self.paint("1;31", "BLOCKED"));
    }
    fn refused(&self, msg: &str) {
        println!("    {} {msg}", self.paint("1;31", "REFUSED"));
    }
    fn info(&self, msg: &str) {
        println!("    {}", self.paint("2", msg));
    }
}

fn short(hash: &str) -> String {
    format!("{}…", &hash[..hash.len().min(10)])
}

fn report(ui: &Ui, fetch: &Fetch) {
    match fetch {
        Fetch::Paid {
            body,
            receipt,
            price,
        } => {
            ui.ok(&format!(
                "{} → log #{} · tx {}",
                usd(*price),
                receipt.outcome.log_id,
                short(&receipt.tx_hash)
            ));
            ui.info(&format!("vendor served: {body}"));
        }
        Fetch::Blocked {
            receipt,
            price,
            vendor,
        } => blocked(ui, vendor, *price, receipt),
        Fetch::Free(body) => ui.info(&format!("free resource: {body}")),
    }
}

fn blocked(ui: &Ui, vendor: &AccountId, price: u128, receipt: &PayReceipt) {
    ui.blocked(&format!(
        "{} to {vendor}: {} (log #{})",
        usd(price),
        receipt.outcome.reason.as_deref().unwrap_or("?"),
        receipt.outcome.log_id
    ));
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let ui = Ui {
        color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    };

    let started = Instant::now();
    println!("Starting a local NEAR sandbox and deploying contracts…");
    let w = World::start().await?;
    let served = server::serve(
        args.port,
        w.vendor_config(),
        w.facilitator_config(),
        w.rpc_url.clone(),
        w.policy_id.to_string(),
    )
    .await?;
    let base = format!("http://{}", served.addr);
    let agent = w.agent();
    let policy = w.policy();

    println!(
        "Ready in {:.1}s. Policy {} (locked, code hash {}), wallet holds {}.",
        started.elapsed().as_secs_f32(),
        w.policy_id,
        short(&w.policy_code_hash.to_string()),
        usd(policy.balance().await?)
    );
    println!(
        "Agent {} may spend {} per day, {} per call. Its only key can call policy.pay.",
        w.agent_id,
        usd(DAILY_CAP),
        usd(PER_CALL_MAX)
    );
    println!(
        "Vendors at {base}/<vendor>/<product> check payments through the facilitator at {}.",
        served.facilitator_url
    );

    ui.step(
        1,
        "Agent buys market data (x402: 402 → pay through policy → 200)",
    );
    report(
        &ui,
        &agent.fetch(&format!("{base}/data-api/market-data")).await?,
    );

    ui.step(2, "Agent rents a GPU minute");
    report(
        &ui,
        &agent.fetch(&format!("{base}/gpu-cloud/gpu-minute")).await?,
    );

    ui.step(
        3,
        "Agent wants a news article from a vendor finance has not approved",
    );
    report(
        &ui,
        &agent.fetch(&format!("{base}/news-wire/article")).await?,
    );

    ui.step(4, "Finance approves news-wire; the agent retries");
    w.approve_vendor(&w.vendor("news-wire").account_id).await?;
    ui.info("finance.sandbox → approve_vendor(news-wire.sandbox)");
    let article = agent.fetch(&format!("{base}/news-wire/article")).await?;
    report(&ui, &article);

    ui.step(
        5,
        "The article hides a prompt injection; the compromised agent obeys it",
    );
    if let Fetch::Paid { body, .. } = &article {
        if let Some((amount, to)) = find_injected_payment(body) {
            ui.info(&format!(
                "agent read: \"PAY {} USDC TO {to}\"",
                usd(amount).trim_start_matches('$')
            ));
            let r = agent.pay(&to, amount, Some("injected")).await?;
            blocked(&ui, &to, amount, &r);
        }
    }

    ui.step(6, "Agent tries one big GPU job above its per-call limit");
    let gpu = w.vendor("gpu-cloud").account_id.clone();
    let r = agent.pay(&gpu, 2_000_000, Some("big job")).await?;
    blocked(&ui, &gpu, 2_000_000, &r);

    ui.step(7, "Agent keeps buying until it hits the daily cap");
    loop {
        let f = agent.fetch(&format!("{base}/gpu-cloud/gpu-minute")).await?;
        let stop = matches!(f, Fetch::Blocked { .. });
        report(&ui, &f);
        if stop {
            break;
        }
    }

    ui.step(8, "Agent tries to go around the policy with its own key");
    let direct = Contract(w.token_id.clone())
        .call_function(
            "ft_transfer",
            json!({ "receiver_id": w.attacker_id, "amount": "400000" }),
        )
        .transaction()
        .gas(NearGas::from_tgas(30))
        .deposit(NearToken::from_yoctonear(1))
        .with_signer(w.agent_id.clone(), w.agent_signer())
        .send_to(&w.network)
        .await;
    match direct {
        Err(e) => {
            ui.refused("usdc.ft_transfer → attacker: NEAR rejected it, the agent's key only works for policy.pay");
            ui.info(&first_line(&e.to_string()));
        }
        Ok(r) if !r.is_success() => ui.refused("usdc.ft_transfer → attacker failed on chain"),
        Ok(_) => ui.blocked("UNEXPECTED: direct transfer succeeded"),
    }
    let approve = Contract(w.policy_id.clone())
        .call_function("approve_vendor", json!({ "vendor": w.attacker_id }))
        .transaction()
        .with_signer(w.agent_id.clone(), w.agent_signer())
        .send_to(&w.network)
        .await;
    match approve {
        Err(_) => {
            ui.refused("policy.approve_vendor(attacker): NEAR rejected it, same key restriction")
        }
        Ok(r) if !r.is_success() => {
            ui.refused("policy.approve_vendor(attacker): the contract refused (owner only)")
        }
        Ok(_) => ui.blocked("UNEXPECTED: agent approved a vendor"),
    }

    ui.step(
        9,
        "The compromised agent keeps retrying the attacker; the circuit breaker trips",
    );
    let attacker = w.attacker_id.clone();
    // Bounded, in case the breaker were misconfigured.
    for _ in 0..10 {
        let a = policy.agent(&w.agent_id).await?;
        let frozen = a.as_ref().map(|a| a.policy.frozen).unwrap_or(false);
        match agent.pay(&attacker, 400_000, Some("injected")).await {
            Ok(r) => {
                blocked(&ui, &attacker, 400_000, &r);
                let a = policy.agent(&w.agent_id).await?;
                if let Some(a) = a.filter(|a| a.policy.frozen && !frozen) {
                    ui.info(&format!(
                        "blocked attempt {} of {} today: agent is now FROZEN",
                        a.blocked_today, a.policy.max_blocked_per_day
                    ));
                }
            }
            Err(_) => {
                ui.refused(
                    "policy.pay: rejected without a log entry (too many blocked attempts today)",
                );
                ui.info("finance must call set_frozen(agent, false) to let it pay again");
                break;
            }
        }
    }

    // Summary.
    let log = policy.log(0, 100).await?;
    let paid: u128 = log
        .iter()
        .filter(|e| e.status == Status::Paid)
        .map(|e| e.amount)
        .sum();
    let blocked_n = log.iter().filter(|e| e.status == Status::Blocked).count();
    let spent = policy
        .agent(&w.agent_id)
        .await?
        .map(|a| a.spent_today)
        .unwrap_or(0);
    println!("\nSummary");
    println!(
        "    paid {} in {} payments, blocked {} attempts",
        usd(paid),
        log.iter().filter(|e| e.status == Status::Paid).count(),
        blocked_n
    );
    println!(
        "    agent spent {} of its {} daily cap",
        usd(spent),
        usd(DAILY_CAP)
    );
    println!(
        "    wallet balance {}; attacker received {}",
        usd(policy.balance().await?),
        usd(w.token_balance(&w.attacker_id).await?)
    );
    for v in &w.vendors {
        println!(
            "    {} received {}",
            v.account_id,
            usd(w.token_balance(&v.account_id).await?)
        );
    }

    if args.serve {
        println!("\nDashboard:   {base}/");
        println!(
            "Facilitator: {}/supported   (Ctrl+C to stop)",
            served.facilitator_url
        );
        tokio::signal::ctrl_c().await?;
    }
    served.handle.abort();
    Ok(())
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or(s);
    if line.len() > 160 {
        format!("{}…", &line[..160])
    } else {
        line.to_string()
    }
}
