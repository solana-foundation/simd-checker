use anyhow::Result;
use async_trait::async_trait;
use solana_client::rpc_client::RpcClient;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use std::sync::Arc;

use crate::surfpool::deploy_program_surfpool;

mod surfpool;
mod util;

#[derive(Debug, Clone)]
pub struct TestInfo {
    pub name: String,
    pub description: String,
    pub simd_number: u32,
    pub feature_gate: Option<Pubkey>,
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
    fn info(&self) -> TestInfo;
    fn program(&self) -> Option<ProgramDeployment> {
        None
    }

    fn deploy_or_skip_program(&self, ctx: &RpcContext) -> Result<(), TestOutcome> {
        let Some(ProgramDeployment { so_path, .. }) = self.program() else {
            return Err(TestOutcome::Fail {
                message: format!("Not program binary found for program {}", self.info().name),
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
                    "Program {} for test {} not deployed on {}",
                    ctx.program_id,
                    self.info().name,
                    ctx.network_name
                ),
            })
        } else {
            Ok(())
        }
    }
    async fn run_rpc(&self, ctx: RpcContext) -> Result<TestOutcome>;
}
