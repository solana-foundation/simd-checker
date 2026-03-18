use anyhow::Result;
use clap::Parser;
use log::{debug, info};
use solana_client::{rpc_client::RpcClient, rpc_config::CommitmentConfig};
use solana_keypair::Keypair;
use solana_signer::Signer;
use std::path::Path;
use std::sync::Arc;
use test_common::{collect_feature_deps, start_surfnet, Manifest, RpcContext, TestOutcome};

use tests::all_tests;

#[derive(Parser)]
#[command(
    name = "simd-checker",
    about = "Verify SIMD feature activations on Solana networks"
)]
struct Cli {
    /// Filter tests by name or SIMD number
    #[arg(long)]
    filter: Option<String>,

    /// Target network: localnet, devnet, testnet, mainnet, or a custom RPC URL
    #[arg(long, default_value = "localnet")]
    network: String,

    /// Path to keypair file (required for testnet/mainnet, defaults to ~/.config/solana/id.json for localnet)
    #[arg(long)]
    keypair: Option<String>,

    /// Path to the manifest YAML file
    #[arg(long, default_value = "manifest.yaml")]
    manifest: String,
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

fn load_or_generate_program_keypair(path: &str) -> Result<Keypair> {
    if let Ok(data) = std::fs::read_to_string(path) {
        debug!("Reading keypair at path: {}", path);
        let bytes: Vec<u8> = serde_json::from_str(&data)?;
        let secret: [u8; 32] = bytes[..32].try_into().map_err(|_| {
            anyhow::anyhow!(
                "Invalid program keypair at {}: expected at least 32 bytes",
                path
            )
        })?;
        return Ok(Keypair::new_from_array(secret));
    }

    debug!("Generating new program keypair at {path}...");
    let kp = Keypair::new();
    let bytes: Vec<u8> = kp.to_bytes().to_vec();
    let json = serde_json::to_string(&bytes)?;
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)?;
    Ok(kp)
}

fn airdrop(rpc_client: &RpcClient, payer: &Keypair) -> Result<()> {
    let balance = rpc_client.get_balance(&payer.pubkey())?;
    let one_sol = 1_000_000_000;
    if balance < one_sol {
        info!("Airdropping 1 SOL to payer {}...", payer.pubkey());
        let sig = rpc_client.request_airdrop(&payer.pubkey(), one_sol)?;
        rpc_client.confirm_transaction(&sig)?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let cli = Cli::parse();

    let manifest = Manifest::load(Path::new(&cli.manifest))?;
    let mut tests = all_tests();

    // Collect manifest entries, filtered and sorted by SIMD number
    let mut entries: Vec<_> = manifest
        .iter()
        .filter(|(id, config)| {
            if let Some(ref f) = cli.filter {
                id.contains(f) || config.number.to_string().contains(f)
            } else {
                true
            }
        })
        .collect();
    entries.sort_by_key(|(_, config)| config.number);

    if entries.is_empty() {
        println!("No tests matched the filter.");
        return Ok(());
    }

    info!("Running {} test(s) on {}...", entries.len(), cli.network);

    let payer = resolve_keypair(&cli.keypair, &cli.network)?;
    let url = rpc_url_for_network(&cli.network);
    debug!("Resolved RPC URL: {}", url);
    debug!("Network: {}", cli.network);
    if let Some(ref f) = cli.filter {
        debug!("Filter: {}", f);
    }
    let rpc_client = Arc::new(RpcClient::new_with_commitment(
        &url,
        CommitmentConfig::confirmed(),
    ));

    let mut results: Vec<(String, TestOutcome)> = Vec::new();

    for (id, config) in &entries {
        let label = format!("SIMD-{:04} {}", config.number, id);

        let Some(test) = tests.remove(id.as_str()) else {
            results.push((
                label,
                TestOutcome::Skip {
                    reason: "No test implementation found".to_string(),
                },
            ));
            continue;
        };

        info!("Starting test {}", id);

        // Start a fresh surfnet for each localnet test
        let (surfnet_handle, rpc_client) = if cli.network == "localnet" {
            let features = collect_feature_deps(&manifest, id);
            debug!("Feature deps for {}: {:?}", id, features);
            let handle = start_surfnet(features).await?;
            debug!("Surfnet RPC url: {}", handle.rpc_url);
            let client = Arc::new(RpcClient::new_with_commitment(
                &handle.rpc_url,
                CommitmentConfig::confirmed(),
            ));
            airdrop(&client, &payer)?;
            (Some(handle), client)
        } else {
            (None, Arc::clone(&rpc_client))
        };

        // Handle program deployment/checking
        let mut resolved_program_id = None;
        if let Some(deployment) = test.program() {
            let program_kp = match load_or_generate_program_keypair(&deployment.keypair_path) {
                Ok(kp) => kp,
                Err(e) => {
                    if let Some(h) = surfnet_handle {
                        h.kill();
                    }
                    results.push((
                        label,
                        TestOutcome::Fail {
                            message: format!("Failed to load program keypair: {e}"),
                        },
                    ));
                    continue;
                }
            };

            resolved_program_id = Some(program_kp.pubkey());
        }

        let ctx = RpcContext {
            rpc_client: Arc::clone(&rpc_client),
            payer: payer.insecure_clone(),
            network_name: cli.network.clone(),
            program_id: resolved_program_id.expect("Could not resolve program id"),
            feature_gate: config.feature_activation.address,
        };
        if let Err(err_outcome) = test.deploy_or_skip_program(&ctx) {
            if let Some(h) = surfnet_handle {
                h.kill();
            }
            results.push((label, err_outcome));
            continue;
        }
        let outcome = test.run_rpc(ctx).await?;

        if let Some(h) = surfnet_handle {
            h.kill();
        }

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

    let pass_count = results
        .iter()
        .filter(|(_, o)| matches!(o, TestOutcome::Pass { .. }))
        .count();
    let fail_count = results.iter().filter(|(_, o)| o.is_fail()).count();
    let skip_count = results
        .iter()
        .filter(|(_, o)| matches!(o, TestOutcome::Skip { .. }))
        .count();

    println!(
        "\nSummary: {} passed, {} failed, {} skipped",
        pass_count, fail_count, skip_count
    );

    if any_fail {
        std::process::exit(1);
    }

    Ok(())
}
