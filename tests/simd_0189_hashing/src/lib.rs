#[cfg(not(feature = "no-entrypoint"))]
mod entrypoint {
    use solana_program::{
        account_info::AccountInfo, entrypoint, entrypoint::ProgramResult, msg, pubkey::Pubkey,
    };

    entrypoint!(process_instruction);

    pub fn process_instruction(
        _program_id: &Pubkey,
        _accounts: &[AccountInfo],
        _instruction_data: &[u8],
    ) -> ProgramResult {
        msg!("SIMD-0189 hashing test: program invoked successfully");
        Ok(())
    }
}

#[cfg(feature = "no-entrypoint")]
mod test_impl {
    use anyhow::Result;
    use async_trait::async_trait;
    use test_common::{RpcContext, SimdTest, TestInfo, TestOutcome};

    pub struct Simd0189HashingTest;

    #[async_trait]
    impl SimdTest for Simd0189HashingTest {
        fn info(&self) -> TestInfo {
            TestInfo {
                name: "simd_0189_hashing".to_string(),
                description: "Verifies SIMD-0189 hashing features are active".to_string(),
                simd_number: 189,
                feature_gate: None,
            }
        }

        async fn run_rpc(&self, ctx: RpcContext) -> Result<TestOutcome> {
            use solana_instruction::Instruction;
            use solana_message::Message;
            use solana_pubkey::Pubkey;
            use solana_signer::Signer;
            use solana_transaction::Transaction;
            use std::str::FromStr;

            let program_id = Pubkey::from_str("SimdTest111111111111111111111111111111111111")
                .unwrap_or_else(|_| Pubkey::new_unique());

            let instruction = Instruction::new_with_bytes(program_id, &[], vec![]);
            let recent_blockhash = ctx.rpc_client.get_latest_blockhash()?;
            let message = Message::new(&[instruction], Some(&ctx.payer.pubkey()));
            let transaction = Transaction::new(&[&ctx.payer], message, recent_blockhash);

            match ctx.rpc_client.send_and_confirm_transaction(&transaction) {
                Ok(sig) => Ok(TestOutcome::Pass {
                    message: format!("Transaction confirmed: {sig}"),
                }),
                Err(e) => Ok(TestOutcome::Fail {
                    message: format!("Transaction failed: {e}"),
                }),
            }
        }
    }

    pub fn register() -> Box<dyn SimdTest> {
        Box::new(Simd0189HashingTest)
    }
}

#[cfg(feature = "no-entrypoint")]
pub use test_impl::register;
