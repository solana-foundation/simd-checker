use anyhow::Result;
use clap::Parser;
use log::{debug, info};
use solana_client::{rpc_client::RpcClient, rpc_config::CommitmentConfig};
use solana_keypair::Keypair;
use solana_signer::Signer;
use std::path::Path;
use std::sync::Arc;
use test_common::{
    collect_dependency_features, collect_feature_deps, start_surfnet, start_surfnet_with_upstream,
    ActivationContext, E2eContext, Manifest, RequirementChecker, RpcContext, TestOutcome,
    TestReport, TestResult,
};

use tests::{all_e2e_tests, all_simd_tests};

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

    /// Run e2e tests even when their requirements are unmet. The test's
    /// status will still be reported as PENDING, but its output (message
    /// and tx signatures) will be captured.
    #[arg(long)]
    run_pending: bool,
}

fn rpc_url_for_network(network: &str) -> String {
    match network {
        "localnet" => "http://127.0.0.1:8899".to_string(),
        "testnet" => "https://api.testnet.solana.com".to_string(),
        "devnet" => "https://api.devnet.solana.com".to_string(),
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
    let tests = all_simd_tests();

    // Collect manifest entries, filtered and sorted by SIMD number
    let mut entries: Vec<_> = manifest
        .iter_simds()
        .filter(|(id, config)| {
            if let Some(ref f) = cli.filter {
                id.contains(f) || config.number.to_string().contains(f)
            } else {
                true
            }
        })
        .collect();
    entries.sort_by_key(|(_, config)| config.number);

    // Collect e2e checks, filtered
    let mut e2e_entries: Vec<_> = manifest
        .iter_e2e_checks()
        .filter(|(id, config)| {
            if let Some(ref f) = cli.filter {
                id.contains(f) || config.description.contains(f)
            } else {
                true
            }
        })
        .collect();
    e2e_entries.sort_by_key(|(id, _)| (*id).clone());

    if entries.is_empty() && e2e_entries.is_empty() {
        println!("No tests matched the filter.");
        return Ok(());
    }

    info!(
        "Running {} SIMD test(s) and {} e2e check(s) on {}...",
        entries.len(),
        e2e_entries.len(),
        cli.network
    );

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
        let passes = if cli.network == "localnet" {
            vec![
                (
                    "deactivated",
                    false,
                    Some((
                        collect_dependency_features(&manifest, id),
                        vec![config.feature_activation.address],
                    )),
                ),
                (
                    "activated",
                    true,
                    Some((collect_feature_deps(&manifest, id), vec![])),
                ),
            ]
        } else {
            let activated = config.feature_activation.is_activated_on(&cli.network);
            vec![("live", activated, None)]
        };

        for (pass_name, expect_activated, features) in passes {
            let label = if cli.network == "localnet" {
                format!("SIMD-{:04} {} ({})", config.number, id, pass_name)
            } else {
                format!("SIMD-{:04} {}", config.number, id)
            };

            let (surfnet_handle, rpc_client) =
                if let Some((activated_features, deactivated_features)) = features {
                    debug!(
                        "Activated feature gates for {} ({}): {:?}",
                        id, pass_name, activated_features
                    );
                    debug!(
                        "Deactivated feature gates for {} ({}): {:?}",
                        id, pass_name, deactivated_features
                    );
                    let handle = start_surfnet(activated_features, deactivated_features).await?;
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
            let mut resolved_program_id = config
                .test_deployment_for(&cli.network)
                .and_then(|deployment| deployment.address);
            if resolved_program_id.is_none() {
                if let Some(deployment) = test.program() {
                    let program_kp =
                        match load_or_generate_program_keypair(&deployment.keypair_path) {
                            Ok(kp) => kp,
                            Err(e) => {
                                if let Some(h) = surfnet_handle {
                                    h.kill();
                                }
                                let outcome = TestOutcome::Fail {
                                    message: format!("Failed to load program keypair: {e}"),
                                    tx_signatures: vec![],
                                };
                                results.push(TestResult::new(label, &outcome, None));
                                continue;
                            }
                        };

                    resolved_program_id = Some(program_kp.pubkey());
                }
            }

            let ctx = RpcContext {
                rpc_client: Arc::clone(&rpc_client),
                payer: payer.insecure_clone(),
                network_name: cli.network.clone(),
                program_id: resolved_program_id.expect("Could not resolve program id"),
                feature_gate: config.feature_activation.address,
                expect_activated: expect_activated,
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
                expected: expect_activated,
                detected: Some(detected),
            };

            let outcome = test.run_rpc(ctx).await?;

            if let Some(h) = surfnet_handle {
                h.kill();
            }

            results.push(TestResult::new(label, &outcome, Some(activation)));
        }
    }

    // ----- E2E checks -----
    let e2e_tests = all_e2e_tests();
    for (id, config) in &e2e_entries {
        let label = format!("E2E {}", id);
        info!("Starting e2e check {}", id);

        // Resolve feature gates up-front so a misconfigured manifest fails loudly.
        let resolved_gates = match config.requires.resolved_feature_gates(&manifest) {
            Ok(g) => g,
            Err(e) => {
                let outcome = TestOutcome::Fail {
                    message: format!("Failed to resolve feature gates: {e}"),
                    tx_signatures: vec![],
                };
                results.push(TestResult::new(label, &outcome, None));
                continue;
            }
        };

        // Spin up runtime: surfnet for localnet (with required gates enabled),
        // live RpcClient otherwise.
        let (surfnet_handle, e2e_rpc_client, e2e_rpc_url) = if cli.network == "localnet" {
            let enable: Vec<_> = resolved_gates.iter().map(|(_, pk)| *pk).collect();
            // Use mainnet as the upstream RPC so that account-clone (e.g.
            // token-2022 BPF program at TokenzQd...) just works without a
            // separate `clone_program` step.
            let handle = match start_surfnet_with_upstream(enable, vec![]).await {
                Ok(h) => h,
                Err(e) => {
                    let outcome = TestOutcome::Fail {
                        message: format!("Failed to start surfnet: {e}"),
                        tx_signatures: vec![],
                    };
                    results.push(TestResult::new(label, &outcome, None));
                    continue;
                }
            };
            let url = handle.rpc_url.clone();
            let client = Arc::new(RpcClient::new_with_commitment(
                &url,
                CommitmentConfig::confirmed(),
            ));
            if let Err(e) = airdrop(&client, &payer) {
                handle.kill();
                let outcome = TestOutcome::Fail {
                    message: format!("Airdrop failed: {e}"),
                    tx_signatures: vec![],
                };
                results.push(TestResult::new(label, &outcome, None));
                continue;
            }
            (Some(handle), client, url)
        } else {
            (None, Arc::clone(&rpc_client), url.clone())
        };

        // Check requirements against whichever runtime we're on. Surfpool's
        // configured upstream RPC will fall through for missing accounts on
        // localnet, so the same check is meaningful in both cases.
        let unmet =
            match RequirementChecker::new(&e2e_rpc_client).check(&config.requires, &manifest) {
                Ok(u) => u,
                Err(e) => {
                    if let Some(h) = surfnet_handle {
                        h.kill();
                    }
                    let outcome = TestOutcome::Fail {
                        message: format!("Requirement check failed: {e}"),
                        tx_signatures: vec![],
                    };
                    results.push(TestResult::new(label, &outcome, None));
                    continue;
                }
            };

        if !unmet.is_empty() && !cli.run_pending {
            if let Some(h) = surfnet_handle {
                h.kill();
            }
            let outcome = TestOutcome::Pending {
                unmet,
                message: None,
                tx_signatures: vec![],
            };
            results.push(TestResult::new(label, &outcome, None));
            continue;
        }

        // Look up the test impl.
        let Some(test) = e2e_tests.get(config.test.as_str()) else {
            if let Some(h) = surfnet_handle {
                h.kill();
            }
            let outcome = TestOutcome::Skip {
                reason: format!(
                    "No e2e test implementation registered for '{}'",
                    config.test
                ),
            };
            results.push(TestResult::new(label, &outcome, None));
            continue;
        };

        let ctx = E2eContext {
            rpc_client: Arc::clone(&e2e_rpc_client),
            rpc_url: e2e_rpc_url.clone(),
            payer: payer.insecure_clone(),
            network_name: cli.network.clone(),
            required_feature_gates: resolved_gates.iter().map(|(_, pk)| *pk).collect(),
            required_programs: config.requires.programs.iter().map(|p| p.address).collect(),
        };

        let outcome = match test.run(ctx).await {
            Ok(o) => o,
            Err(e) => TestOutcome::Fail {
                message: format!("e2e test errored: {e}"),
                tx_signatures: vec![],
            },
        };

        // If we got here with unmet requirements (because of --run-pending),
        // demote any outcome to PENDING while preserving the message and tx
        // signatures captured during the run.
        let outcome = if !unmet.is_empty() {
            let (message, tx_signatures) = match outcome {
                TestOutcome::Pass {
                    message,
                    tx_signatures,
                } => (Some(message), tx_signatures),
                TestOutcome::Fail {
                    message,
                    tx_signatures,
                } => (Some(message), tx_signatures),
                TestOutcome::Skip { reason } => (Some(reason), vec![]),
                TestOutcome::Pending {
                    message,
                    tx_signatures,
                    ..
                } => (message, tx_signatures),
            };
            TestOutcome::Pending {
                unmet,
                message,
                tx_signatures,
            }
        } else {
            outcome
        };

        if let Some(h) = surfnet_handle {
            h.kill();
        }
        results.push(TestResult::new(label, &outcome, None));
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
                    "pending" => "[PENDING]",
                    _ => "[????]",
                };
                let activation_str = match &result.activation {
                    Some(ctx) => format!(" [{}]", ctx),
                    None => String::new(),
                };
                println!(
                    "{:9} {}{} - {}",
                    status, result.label, activation_str, result.message,
                );
                for unmet in &result.unmet {
                    println!("          - {}", unmet);
                }
                for tx in &result.tx_signatures {
                    let tx_status = if tx.success { "ok" } else { "err" };
                    let tx_error = tx
                        .error
                        .as_ref()
                        .map(|error| format!(" ({error})"))
                        .unwrap_or_default();
                    println!(
                        "          tx {} [{}]: {}{}",
                        tx.label, tx_status, tx.signature, tx_error
                    );
                }
            }
            println!(
                "\nSummary: {} passed, {} failed, {} pending, {} skipped",
                report.summary.passed,
                report.summary.failed,
                report.summary.pending,
                report.summary.skipped,
            );
        }
    }

    if report.any_fail() {
        std::process::exit(1);
    }

    Ok(())
}
