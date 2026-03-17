#[cfg(not(feature = "no-entrypoint"))]
mod entrypoint {
    use pinocchio::{entrypoint, error::ProgramError, AccountView, Address, ProgramResult};
    use program_common::{FeatureGateAccount, TestFailure, FEATURE_GATE_PROGRAM};
    use solana_program_log::log;
    use solana_sysvar::get_sysvar;

    entrypoint!(process_instruction);

    #[repr(C)]
    pub struct Rent {
        pub lamports_per_byte: u64,
        pub exemption_threshold: f64,
        pub burn_percent: u8,
    }

    pub fn process_instruction(
        _program_id: &Address,
        accounts: &[AccountView],
        instruction_data: &[u8],
    ) -> ProgramResult {
        let expect_activated = instruction_data[0] != 0;
        let [feature] = accounts else {
            return Err(ProgramError::InvalidAccountData);
        };

        let feature_bytes = feature.try_borrow()?.to_vec();
        let feature_gate = if feature_bytes.len() < 9 {
            FeatureGateAccount {
                activated: false,
                epoch: 0,
            }
        } else {
            FeatureGateAccount {
                activated: feature_bytes[0] != 0,
                epoch: u64::from_le_bytes(feature_bytes[1..9].try_into().unwrap()),
            }
        };

        let is_owned = feature.owned_by(&FEATURE_GATE_PROGRAM);
        let is_activated = feature_gate.activated;
        let activation_epoch = feature_gate.epoch;
        let status = if is_activated {
            format!("Activated in slot {}", activation_epoch)
        } else if is_owned {
            "Pending activation".to_string()
        } else {
            "Not Activated".to_string()
        };
        // Assert that the account is actually activated in the runtime

        if is_activated && !expect_activated {
            return Err(ProgramError::Custom(
                TestFailure::ActivatedWhenNotExpected as u32,
            ));
        } else if !is_activated && expect_activated {
            return Err(ProgramError::Custom(
                TestFailure::NotActivatedWhenExpected as u32,
            ));
        }
        log!("SIMD-0194 Feature gate");
        log!("----------------------------");
        log!("Status: {}\n", status.as_str());

        // let mut rent_bytes = [0u8; 24]; // Allocate a buffer to hold the Rent sysvar data
        // let sysvar_id = solana_sysvar::rent::id(); // Get the ID of the Rent sysvar
        // let offset = 0; // Offset to read from the sysvar data
        // get_sysvar(&mut rent_bytes[..17], &sysvar_id, offset, 17)?;

        // let rent: Rent = unsafe { core::mem::transmute(rent_bytes) };
        // let exemption = rent.exemption_threshold.to_string();
        // log!("Lamports Per Byte: {}", rent.lamports_per_byte); // Log the lamports per byte from the Rent sysvar
        // log!("Exemption Threshold: {}", exemption.as_str()); // Log the exemption threshold from the Rent sysvar
        // log!("Burn Percent: {}", rent.burn_percent);
        // if is_activated {
        // } else {
        // }
        Ok(())
    }
}

#[cfg(feature = "no-entrypoint")]
mod test_impl {
    use anyhow::Result;
    use async_trait::async_trait;
    use solana_message::AccountMeta;
    use test_common::{RpcContext, SimdTest, TestOutcome};

    pub struct Simd0194Test;

    #[async_trait]
    impl SimdTest for Simd0194Test {
        fn program(&self) -> Option<test_common::ProgramDeployment> {
            Some(test_common::ProgramDeployment {
                keypair_path: "tests/simd_0194/program-keypair.json".to_string(),
                so_path: "target/deploy/simd_0194.so".to_string(),
            })
        }

        async fn run_rpc(&self, ctx: RpcContext) -> Result<TestOutcome> {
            use solana_instruction::Instruction;
            use solana_message::Message;
            use solana_signer::Signer;
            use solana_transaction::Transaction;

            let program_id = ctx.program_id;
            let feature_id = ctx.feature_gate;

            let instruction = Instruction::new_with_bytes(
                program_id,
                &[1],
                vec![AccountMeta::new_readonly(feature_id, false)],
            );
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
        Box::new(Simd0194Test)
    }
}

#[cfg(feature = "no-entrypoint")]
pub use test_impl::register;
