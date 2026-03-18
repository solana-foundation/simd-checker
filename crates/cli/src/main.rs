use anyhow::Result;
use clap::Parser;
use log::{debug, info};
use solana_client::{rpc_client::RpcClient, rpc_config::CommitmentConfig};
use solana_keypair::Keypair;
use solana_signer::Signer;
use std::path::Path;
use std::sync::Arc;
use test_common::{
    collect_dependency_features, collect_feature_deps, start_surfnet, ActivationContext, Manifest,
    RpcContext, TestOutcome, TestReport, TestResult,
};

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

    /// Output format: text, json, yaml
    #[arg(long, default_value = "text")]
    output: String,

    /// Write json/yaml output to a file instead of stdout
    #[arg(long)]
    output_file: Option<String>,
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
    let tests = all_tests();

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

    let mut results: Vec<TestResult> = Vec::new();

    for (id, config) in &entries {
        let Some(test) = tests.get(id.as_str()) else {
            let outcome = TestOutcome::Skip {
                reason: "No test implementation found".to_string(),
            };
            results.push(TestResult::new(
                format!("SIMD-{:04} {}", config.number, id),
                &outcome,
                None,
            ));
            continue;
        };

        info!("Starting test {}", id);

        // For localnet, run two passes: deactivated then activated.
        // For other networks, run once with the live feature state.
        let passes: Vec<(&str, bool, Option<Vec<solana_pubkey::Pubkey>>)> =
            if cli.network == "localnet" {
                vec![
                    (
                        "deactivated",
                        false,
                        Some(collect_dependency_features(&manifest, id)),
                    ),
                    ("activated", true, Some(collect_feature_deps(&manifest, id))),
                ]
            } else {
                let activated = config.feature_activation.is_activated_on(&cli.network);
                vec![("live", activated, None)]
            };

        for (pass_name, expect_activated, features) in &passes {
            let label = if cli.network == "localnet" {
                format!("SIMD-{:04} {} ({})", config.number, id, pass_name)
            } else {
                format!("SIMD-{:04} {}", config.number, id)
            };

            let (surfnet_handle, rpc_client) = if let Some(features) = features {
                debug!("Feature gates for {} ({}): {:?}", id, pass_name, features);
                let handle = start_surfnet(features.clone()).await?;
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
                        let outcome = TestOutcome::Fail {
                            message: format!("Failed to load program keypair: {e}"),
                        };
                        results.push(TestResult::new(label, &outcome, None));
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
                expect_activated: *expect_activated,
            };
            if let Err(err_outcome) = test.deploy_or_skip_program(&ctx) {
                if let Some(h) = surfnet_handle {
                    h.kill();
                }
                results.push(TestResult::new(label, &err_outcome, None));
                continue;
            }

            // Detect on-chain activation and build context
            let detected = test.detect_feature_activated(&ctx);
            let activation = ActivationContext {
                expected: *expect_activated,
                detected: Some(detected),
            };

            let outcome = test.run_rpc(ctx).await?;

            if let Some(h) = surfnet_handle {
                h.kill();
            }

            results.push(TestResult::new(label, &outcome, Some(activation)));
        }
    }

    let report = TestReport::new(results);

    match cli.output.as_str() {
        "json" => {
            let json = serde_json::to_string_pretty(&report)?;
            if let Some(ref path) = cli.output_file {
                std::fs::write(path, &json)?;
            } else {
                println!("{json}");
            }
        }
        "yaml" | "yml" => {
            let yaml = serde_yaml::to_string(&report)?;
            if let Some(ref path) = cli.output_file {
                std::fs::write(path, &yaml)?;
            } else {
                print!("{yaml}");
            }
        }
        _ => {
            // text format (default) — matches previous output
            println!();
            for result in &report.results {
                let status = match result.status.as_str() {
                    "pass" => "[PASS]",
                    "fail" => "[FAIL]",
                    "skip" => "[SKIP]",
                    _ => "[????]",
                };
                let activation_str = match &result.activation {
                    Some(ctx) => format!(" [{}]", ctx),
                    None => String::new(),
                };
                println!(
                    "{:6} {}{} - {}",
                    status, result.label, activation_str, result.message,
                );
            }
            println!(
                "\nSummary: {} passed, {} failed, {} skipped",
                report.summary.passed, report.summary.failed, report.summary.skipped,
            );
        }
    }

    if report.any_fail() {
        std::process::exit(1);
    }

    Ok(())
}
