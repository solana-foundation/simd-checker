use anyhow::Result;
use async_trait::async_trait;
use solana_client::rpc_client::RpcClient;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use std::sync::Arc;

use crate::surfpool::deploy_program_surfpool;

pub mod manifest;
mod surfpool;
mod util;

pub use manifest::{FeatureConfig, Manifest};

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
            Err(TestOutcome::Fail {
                message: format!(
                    "Program {} not deployed on {}",
                    ctx.program_id,
                    ctx.network_name
                ),
            })
        } else {
            Ok(())
        }
    }

    async fn run_rpc(&self, ctx: RpcContext) -> Result<TestOutcome>;
}
