use anyhow::Result;
use async_trait::async_trait;
use log::debug;
use solana_client::rpc_client::RpcClient;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use std::collections::HashSet;
use std::sync::Arc;
use surfpool_types::{SimnetConfig, SimnetEvent, SurfpoolConfig, SvmFeatureConfig};

use crate::surfpool::deploy_program_surfpool;

pub mod manifest;
mod surfpool;
mod util;

pub use manifest::{FeatureConfig, Manifest};

pub fn collect_feature_deps(manifest: &Manifest, simd_id: &str) -> Vec<Pubkey> {
    let mut pubkeys = Vec::new();
    let mut visited = HashSet::new();
    collect_feature_deps_inner(manifest, simd_id, &mut pubkeys, &mut visited);
    pubkeys
}

fn collect_feature_deps_inner(
    manifest: &Manifest,
    simd_id: &str,
    pubkeys: &mut Vec<Pubkey>,
    visited: &mut HashSet<String>,
) {
    if !visited.insert(simd_id.to_string()) {
        return;
    }
    if let Some(config) = manifest.get(simd_id) {
        pubkeys.push(config.feature_activation.address);
        for dep in &config.depends_on {
            collect_feature_deps_inner(manifest, dep, pubkeys, visited);
        }
    }
}

pub struct SurfnetHandle {
    simnet_commands_tx: crossbeam::channel::Sender<surfpool_types::SimnetCommand>,
    _thread_handle: std::thread::JoinHandle<()>,
}

impl SurfnetHandle {
    pub fn kill(self) {
        let _ = self
            .simnet_commands_tx
            .send(surfpool_types::SimnetCommand::Terminate(None));
    }
}

pub async fn start_surfnet(features_to_enable: Vec<Pubkey>) -> Result<SurfnetHandle> {
    let (surfnet_svm, simnet_events_rx, geyser_events_rx) =
        surfpool_core::surfnet::svm::SurfnetSvm::default();

    let mut feature_config = SvmFeatureConfig::new();
    for pubkey in features_to_enable {
        feature_config = feature_config.enable(pubkey);
    }
    debug!("Surfnet feature config: {:?}", feature_config);

    let config = SurfpoolConfig {
        simnets: vec![SimnetConfig {
            offline_mode: true,
            remote_rpc_url: None,
            instruction_profiling_enabled: false,
            max_profiles: 1,
            feature_config,
            ..Default::default()
        }],
        ..Default::default()
    };

    let (simnet_commands_tx, simnet_commands_rx) = crossbeam::channel::unbounded();
    let (subgraph_commands_tx, _) = crossbeam::channel::unbounded();

    let handle_tx = simnet_commands_tx.clone();

    // start_local_surfnet's future is !Send (crossbeam internals), so we
    // run it on a dedicated thread with its own single-threaded tokio runtime.
    let thread_handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime for surfnet");
        rt.block_on(async move {
            let _ = surfpool_core::start_local_surfnet(
                surfnet_svm,
                config,
                subgraph_commands_tx,
                simnet_commands_tx,
                simnet_commands_rx,
                geyser_events_rx,
            )
            .await;
        });
    });

    // Wait for the surfnet to signal readiness on the events channel.
    loop {
        match simnet_events_rx.recv() {
            Ok(SimnetEvent::Ready(_)) => {
                debug!("Surfnet ready");
                break;
            }
            Ok(SimnetEvent::Aborted(msg)) => anyhow::bail!("Surfnet aborted: {msg}"),
            Ok(evt) => {
                debug!("Surfnet event: {:?}", evt);
                continue;
            }
            Err(_) => anyhow::bail!("Surfnet event channel closed unexpectedly"),
        }
    }

    Ok(SurfnetHandle {
        simnet_commands_tx: handle_tx,
        _thread_handle: thread_handle,
    })
}

#[derive(Debug)]
pub enum TestOutcome {
    Pass { message: String },
    Fail { message: String },
    Skip { reason: String },
}

impl TestOutcome {
    pub fn is_fail(&self) -> bool {
        matches!(self, TestOutcome::Fail { .. })
    }

    pub fn label(&self) -> &str {
        match self {
            TestOutcome::Pass { .. } => "PASS",
            TestOutcome::Fail { .. } => "FAIL",
            TestOutcome::Skip { .. } => "SKIP",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            TestOutcome::Pass { message } => message,
            TestOutcome::Fail { message } => message,
            TestOutcome::Skip { reason } => reason,
        }
    }
}

pub struct ProgramDeployment {
    pub keypair_path: String,
    pub so_path: String,
}

pub struct RpcContext {
    pub rpc_client: Arc<RpcClient>,
    pub payer: Keypair,
    pub network_name: String,
    pub program_id: Pubkey,
    pub feature_gate: Pubkey,
}

impl RpcContext {
    fn is_program_deployed(&self) -> bool {
        match self.rpc_client.get_account(&self.program_id) {
            Ok(account) => account.executable,
            Err(_) => false,
        }
    }
}

#[async_trait]
pub trait SimdTest: Send + Sync {
    fn program(&self) -> Option<ProgramDeployment> {
        None
    }

    fn deploy_or_skip_program(&self, ctx: &RpcContext) -> Result<(), TestOutcome> {
        let Some(ProgramDeployment { so_path, .. }) = self.program() else {
            return Err(TestOutcome::Fail {
                message: format!("No program binary found"),
            });
        };
        if ctx.network_name == "localnet" {
            if let Err(e) = deploy_program_surfpool(&ctx, &so_path) {
                Err(TestOutcome::Fail {
                    message: format!("Program deployment failed: {e}"),
                })
            } else {
                Ok(())
            }
        } else if !ctx.is_program_deployed() {
            Err(TestOutcome::Skip {
                reason: format!(
                    "Program {} not deployed on {}",
                    ctx.program_id, ctx.network_name
                ),
            })
        } else {
            Ok(())
        }
    }

    async fn run_rpc(&self, ctx: RpcContext) -> Result<TestOutcome>;
}
