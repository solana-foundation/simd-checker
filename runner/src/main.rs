use anyhow::Result;
use clap::Parser;
use solana_client::rpc_client::RpcClient;
use solana_keypair::Keypair;
use solana_signer::Signer;
use std::sync::Arc;
use test_common::{RpcContext, SimdTest, TestOutcome};

include!(concat!(env!("OUT_DIR"), "/test_registry.rs"));

#[derive(Parser)]
#[command(name = "simd-checker", about = "Verify SIMD feature activations on Solana networks")]
struct Cli {
    /// Filter tests by name or SIMD number
    #[arg(long)]
    filter: Option<String>,

    /// Target network: localnet, testnet, mainnet, or a custom RPC URL
    #[arg(long, default_value = "localnet")]
    network: String,

    /// Path to keypair file (required for testnet/mainnet, defaults to ~/.config/solana/id.json for localnet)
    #[arg(long)]
    keypair: Option<String>,
}

fn rpc_url_for_network(network: &str) -> String {
    match network {
        "localnet" => "http://127.0.0.1:8899".to_string(),
        "testnet" => "https://api.testnet.solana.com".to_string(),
        "mainnet" => "https://api.mainnet-beta.solana.com".to_string(),
        url => url.to_string(),
    }
}

fn load_keypair(path: &str) -> Result<Keypair> {
    let data = std::fs::read_to_string(path)?;
    let bytes: Vec<u8> = serde_json::from_str(&data)?;
    let secret: [u8; 32] = bytes[..32].try_into()?;
    Ok(Keypair::new_from_array(secret))
}

fn default_keypair_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    format!("{home}/.config/solana/id.json")
}

fn resolve_keypair(keypair_arg: &Option<String>, network: &str) -> Result<Keypair> {
    if let Some(ref kp_path) = keypair_arg {
        return load_keypair(kp_path);
    }

    match network {
        "localnet" => {
            let default_path = default_keypair_path();
            load_keypair(&default_path).map_err(|e| {
                anyhow::anyhow!(
                    "No --keypair provided and failed to load default keypair at {}: {}",
                    default_path,
                    e
                )
            })
        }
        _ => anyhow::bail!("--keypair is required for network '{}'", network),
    }
}

fn airdrop(rpc_client: &RpcClient, payer: &Keypair) -> Result<()> {
    let balance = rpc_client.get_balance(&payer.pubkey())?;
    let one_sol = 1_000_000_000;
    if balance < one_sol {
        println!("Airdropping 1 SOL to payer {}...", payer.pubkey());
        let sig = rpc_client.request_airdrop(&payer.pubkey(), one_sol)?;
        rpc_client.confirm_transaction(&sig)?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let tests = all_tests();

    let filtered: Vec<Box<dyn SimdTest>> = tests
        .into_iter()
        .filter(|t| {
            if let Some(ref f) = cli.filter {
                let info = t.info();
                info.name.contains(f) || info.simd_number.to_string().contains(f)
            } else {
                true
            }
        })
        .collect();

    if filtered.is_empty() {
        println!("No tests matched the filter.");
        return Ok(());
    }

    println!(
        "Running {} test(s) on {}...\n",
        filtered.len(),
        cli.network
    );

    let payer = resolve_keypair(&cli.keypair, &cli.network)?;
    let url = rpc_url_for_network(&cli.network);
    let rpc_client = Arc::new(RpcClient::new(url));

    if cli.network == "localnet" {
        airdrop(&rpc_client, &payer)?;
    }

    let mut results: Vec<(String, TestOutcome)> = Vec::new();

    for test in &filtered {
        let info = test.info();
        let label = format!("SIMD-{:04} {}", info.simd_number, info.name);

        let ctx = RpcContext {
            rpc_client: Arc::clone(&rpc_client),
            payer: payer.insecure_clone(),
            network_name: cli.network.clone(),
        };
        let outcome = test.run_rpc(ctx).await?;

        results.push((label, outcome));
    }

    // Print results table
    println!();
    let mut any_fail = false;
    for (label, outcome) in &results {
        let status = match outcome {
            TestOutcome::Pass { .. } => "[PASS]",
            TestOutcome::Fail { .. } => {
                any_fail = true;
                "[FAIL]"
            }
            TestOutcome::Skip { .. } => "[SKIP]",
        };
        println!("{:6} {} - {}", status, label, outcome.message());
    }

    let pass_count = results.iter().filter(|(_, o)| matches!(o, TestOutcome::Pass { .. })).count();
    let fail_count = results.iter().filter(|(_, o)| o.is_fail()).count();
    let skip_count = results.iter().filter(|(_, o)| matches!(o, TestOutcome::Skip { .. })).count();

    println!(
        "\nSummary: {} passed, {} failed, {} skipped",
        pass_count, fail_count, skip_count
    );

    if any_fail {
        std::process::exit(1);
    }

    Ok(())
}
