//! Finance's setup for a private-policy agent. Runs off chain, signs nothing, and
//! needs no account: it writes files, and prints the commands that use them.
//!
//!   zk-setup init --contract policy.testnet --agent agent.testnet \
//!       --cap 1000000 --per-call 500000 \
//!       --vendor data.testnet --vendor gpu.testnet --out .zx402/testnet
//!   zk-setup recover --dir .zx402/testnet
//!
//! Run this on finance's machine, not the agent's. Whoever ran the setup could
//! have kept its randomness and could forge proofs. It is a local setup, not a
//! ceremony (SD-007).

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use near_api::types::AccountId;
use nearx402::deployment::{self, PolicyParams, SetupPaths};

#[derive(Parser)]
#[command(about = "Sets up a private spending policy, off chain")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generates keys and a private policy, and saves them.
    Init {
        /// The policy contract.
        #[arg(long)]
        contract: AccountId,
        /// The agent the policy applies to.
        #[arg(long)]
        agent: AccountId,
        /// Daily cap, in token base units (1000000 is 1.00 mUSDC).
        #[arg(long)]
        cap: u128,
        /// Limit per call, in token base units.
        #[arg(long)]
        per_call: u128,
        /// An allowed vendor. Repeat for each one.
        #[arg(long = "vendor", required = true)]
        vendors: Vec<AccountId>,
        /// Directory for the output. Do not commit it.
        #[arg(long)]
        out: PathBuf,
    },
    /// Starts from a fresh spending state, after the wallet's state was lost.
    Recover {
        /// The directory `init` wrote.
        #[arg(long)]
        dir: PathBuf,
    },
}

fn call_command(contract: &str, method: &str, args: &std::path::Path) -> String {
    format!(
        "near contract call-function as-transaction {contract} {method} file-args {} \
         prepaid-gas '100 Tgas' attached-deposit '0 NEAR' sign-as <FINANCE_ACCOUNT> \
         network-config testnet sign-with-legacy-keychain send",
        args.display()
    )
}

fn print_agent_files(paths: &SetupPaths) {
    println!("\nGive the agent's machine, and nothing else:");
    println!("  {}   (not secret)", paths.proving_key.display());
    println!(
        "  {}   (the private policy: readable by its owner only)",
        paths.wallet.display()
    );
}

fn main() -> Result<()> {
    match Args::parse().command {
        Command::Init {
            contract,
            agent,
            cap,
            per_call,
            vendors,
            out,
        } => {
            let params = PolicyParams {
                contract: contract.clone(),
                agent,
                cap,
                per_call,
                vendors,
            };
            let (paths, _) = deployment::init(&out, &params)?;
            println!("Setup saved in {}. Nothing was sent.", out.display());
            println!("It is a local setup, not a ceremony: not production-safe.");
            println!("\nFinance sends these, which carry commitments only:");
            println!(
                "  {}",
                call_command(
                    contract.as_str(),
                    "set_zk_verifying_key",
                    &paths.verifying_key_args
                )
            );
            println!(
                "  {}",
                call_command(contract.as_str(), "set_zk_agent", &paths.agent_args)
            );
            print_agent_files(&paths);
        }
        Command::Recover { dir } => {
            let paths = deployment::recover(&dir)?;
            println!("A fresh state is recorded. Nothing was sent, and nothing changes until");
            println!("finance sends this, which also resets today's spending:");
            println!(
                "  {}",
                call_command("<POLICY_CONTRACT>", "set_zk_agent", &paths.agent_args)
            );
            print_agent_files(&paths);
        }
    }
    Ok(())
}
