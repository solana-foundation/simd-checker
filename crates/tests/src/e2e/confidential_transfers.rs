use anyhow::Result;
use async_trait::async_trait;
use test_common::{E2eContext, E2eTest, TestOutcome};

pub struct ConfidentialTransfersE2e;

#[async_trait]
impl E2eTest for ConfidentialTransfersE2e {
    fn id(&self) -> &'static str {
        "e2e_confidential_transfers"
    }

    async fn run(&self, _ctx: E2eContext) -> Result<TestOutcome> {
        // TODO:
        //  1. Create a token-2022 mint with confidential-transfer extension.
        //  2. Initialize a confidential account for the payer.
        //  3. Deposit, apply pending balance, withdraw.
        //  4. Assert balances and proof verification succeed.
        Ok(TestOutcome::Skip {
            reason: "confidential_transfers e2e not yet implemented".to_string(),
        })
    }
}

pub fn register() -> Box<dyn E2eTest> {
    Box::new(ConfidentialTransfersE2e)
}
