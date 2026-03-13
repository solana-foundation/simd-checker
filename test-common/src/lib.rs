use anyhow::Result;
use async_trait::async_trait;
use solana_client::rpc_client::RpcClient;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use std::sync::Arc;

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

pub struct RpcContext {
    pub rpc_client: Arc<RpcClient>,
    pub payer: Keypair,
    pub network_name: String,
}

#[async_trait]
pub trait SimdTest: Send + Sync {
    fn info(&self) -> TestInfo;
    async fn run_rpc(&self, ctx: RpcContext) -> Result<TestOutcome>;
}
