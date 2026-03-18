use anyhow::Result;
use async_trait::async_trait;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::{AccountMeta, Message};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_system_interface::instruction::create_account;
use solana_transaction::Transaction;
use spl_associated_token_account_interface::{
    address::get_associated_token_address, instruction::create_associated_token_account,
};
use spl_token_interface::instruction::{initialize_mint2, mint_to, transfer, transfer_checked};
use std::str::FromStr;
use test_common::{RpcContext, SimdTest, TestOutcome};

const MINT_SIZE: u64 = 82;
const DECIMALS: u8 = 9;

pub struct Simd0266Test;

fn token_program_id() -> Pubkey {
    Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").unwrap()
}

fn batch_transfer_ix(
    source: &Pubkey,
    dest: &Pubkey,
    authority: &Pubkey,
    amount: u64,
) -> Instruction {
    let amount_bytes = amount.to_le_bytes();
    let mut data = vec![0u8; 23]; // 1 + 2 * (2 + 1 + 8)
    data[0] = 255; // Batch

    // Transfer 1
    data[1] = 3; // num_accounts
    data[2] = 9; // data_len
    data[3] = 3; // Transfer discriminator
    data[4..12].copy_from_slice(&amount_bytes);

    // Transfer 2
    data[12] = 3; // num_accounts
    data[13] = 9; // data_len
    data[14] = 3; // Transfer discriminator
    data[15..23].copy_from_slice(&amount_bytes);

    Instruction::new_with_bytes(
        token_program_id(),
        &data,
        vec![
            AccountMeta::new(*source, false),
            AccountMeta::new(*dest, false),
            AccountMeta::new_readonly(*authority, true),
            AccountMeta::new(*source, false),
            AccountMeta::new(*dest, false),
            AccountMeta::new_readonly(*authority, true),
        ],
    )
}

fn simulate_cu(
    ctx: &RpcContext,
    instructions: &[Instruction],
    signers: &[&Keypair],
) -> Result<(Option<String>, u64)> {
    let blockhash = ctx.rpc_client.get_latest_blockhash()?;
    let message = Message::new(instructions, Some(&ctx.payer.pubkey()));
    let mut all_signers: Vec<&Keypair> = vec![&ctx.payer];
    all_signers.extend_from_slice(signers);
    let tx = Transaction::new(&all_signers[..], message, blockhash);
    let result = ctx.rpc_client.simulate_transaction(&tx)?;
    let cus = result.value.units_consumed.unwrap_or(0);
    let err = result.value.err.map(|e| format!("{e}"));
    Ok((err, cus))
}

#[async_trait]
impl SimdTest for Simd0266Test {
    fn program(&self) -> Option<test_common::ProgramDeployment> {
        Some(test_common::ProgramDeployment {
            keypair_path: "programs/simd_0266/program-keypair.json".to_string(),
            so_path: "target/deploy/simd_0266.so".to_string(),
        })
    }

    async fn run_rpc(&self, ctx: RpcContext) -> Result<TestOutcome> {
        let payer_pubkey = ctx.payer.pubkey();
        let token_program = token_program_id();

        let is_activated = self.detect_feature_activated(&ctx);

        // --- Set up token infrastructure ---
        let mint = Keypair::new();
        let mint_pubkey = mint.pubkey();
        let mint_rent = ctx
            .rpc_client
            .get_minimum_balance_for_rent_exemption(MINT_SIZE as usize)?;

        let recipient = Keypair::new();
        let recipient_pubkey = recipient.pubkey();

        let source_ata = get_associated_token_address(&payer_pubkey, &mint_pubkey);
        let dest_ata = get_associated_token_address(&recipient_pubkey, &mint_pubkey);

        // Tx 1: Create mint account + initialize mint
        let blockhash = ctx.rpc_client.get_latest_blockhash()?;
        let tx = Transaction::new(
            &[&ctx.payer, &mint],
            Message::new(
                &[
                    create_account(
                        &payer_pubkey,
                        &mint_pubkey,
                        mint_rent,
                        MINT_SIZE,
                        &token_program,
                    ),
                    initialize_mint2(&token_program, &mint_pubkey, &payer_pubkey, None, DECIMALS)?,
                ],
                Some(&payer_pubkey),
            ),
            blockhash,
        );
        ctx.rpc_client.send_and_confirm_transaction(&tx)?;

        // Tx 2: Create ATAs + mint tokens to source
        let blockhash = ctx.rpc_client.get_latest_blockhash()?;
        let tx = Transaction::new(
            &[&ctx.payer],
            Message::new(
                &[
                    create_associated_token_account(
                        &payer_pubkey,
                        &payer_pubkey,
                        &mint_pubkey,
                        &token_program,
                    ),
                    create_associated_token_account(
                        &payer_pubkey,
                        &recipient_pubkey,
                        &mint_pubkey,
                        &token_program,
                    ),
                    mint_to(
                        &token_program,
                        &mint_pubkey,
                        &source_ata,
                        &payer_pubkey,
                        &[],
                        1_000_000_000,
                    )?,
                ],
                Some(&payer_pubkey),
            ),
            blockhash,
        );
        ctx.rpc_client.send_and_confirm_transaction(&tx)?;

        // --- Measure CUs via simulation ---
        let mut results = Vec::new();

        // Transfer
        let (transfer_err, transfer_cus) = simulate_cu(
            &ctx,
            &[transfer(
                &token_program,
                &source_ata,
                &dest_ata,
                &payer_pubkey,
                &[],
                100,
            )?],
            &[],
        )?;
        if let Some(err) = transfer_err {
            return Ok(TestOutcome::Fail {
                message: format!("Transfer simulation failed: {err}"),
            });
        }
        results.push(format!("Transfer: {transfer_cus} CUs"));

        // TransferChecked
        let (tc_err, transfer_checked_cus) = simulate_cu(
            &ctx,
            &[transfer_checked(
                &token_program,
                &source_ata,
                &mint_pubkey,
                &dest_ata,
                &payer_pubkey,
                &[],
                100,
                DECIMALS,
            )?],
            &[],
        )?;
        if let Some(err) = tc_err {
            return Ok(TestOutcome::Fail {
                message: format!("TransferChecked simulation failed: {err}"),
            });
        }
        results.push(format!("TransferChecked: {transfer_checked_cus} CUs"));

        // MintTo
        let (mt_err, mint_to_cus) = simulate_cu(
            &ctx,
            &[mint_to(
                &token_program,
                &mint_pubkey,
                &source_ata,
                &payer_pubkey,
                &[],
                100,
            )?],
            &[],
        )?;
        if let Some(err) = mt_err {
            return Ok(TestOutcome::Fail {
                message: format!("MintTo simulation failed: {err}"),
            });
        }
        results.push(format!("MintTo: {mint_to_cus} CUs"));

        // --- Assert CU expectations ---
        if is_activated {
            // p-token: expect very low CUs (< 1000 per instruction)
            if transfer_cus > 1000 {
                return Ok(TestOutcome::Fail {
                    message: format!(
                        "Feature activated but Transfer CUs too high: {transfer_cus} (expected < 1000)"
                    ),
                });
            }
            if transfer_checked_cus > 1000 {
                return Ok(TestOutcome::Fail {
                    message: format!(
                        "Feature activated but TransferChecked CUs too high: {transfer_checked_cus} (expected < 1000)"
                    ),
                });
            }
            if mint_to_cus > 1000 {
                return Ok(TestOutcome::Fail {
                    message: format!(
                        "Feature activated but MintTo CUs too high: {mint_to_cus} (expected < 1000)"
                    ),
                });
            }

            // Test batch transfer (direct to token program)
            let (batch_err, batch_cus) = simulate_cu(
                &ctx,
                &[batch_transfer_ix(&source_ata, &dest_ata, &payer_pubkey, 50)],
                &[],
            )?;
            if let Some(err) = batch_err {
                return Ok(TestOutcome::Fail {
                    message: format!("Batch transfer failed when activated: {err}"),
                });
            }
            results.push(format!("Batch (2x transfer): {batch_cus} CUs"));

            // Test batch transfer via CPI (through on-chain program)
            let expect_activated_byte: u8 = 1;
            let amount: u64 = 50;
            let (cpi_err, cpi_batch_cus) = simulate_cu(
                &ctx,
                &[Instruction::new_with_bytes(
                    ctx.program_id,
                    &{
                        let mut d = [0u8; 9];
                        d[0] = expect_activated_byte;
                        d[1..9].copy_from_slice(&amount.to_le_bytes());
                        d
                    },
                    vec![
                        AccountMeta::new_readonly(ctx.feature_gate, false),
                        AccountMeta::new_readonly(token_program, false),
                        AccountMeta::new(source_ata, false),
                        AccountMeta::new(dest_ata, false),
                        AccountMeta::new_readonly(payer_pubkey, true),
                    ],
                )],
                &[],
            )?;
            if let Some(err) = cpi_err {
                return Ok(TestOutcome::Fail {
                    message: format!("CPI batch transfer failed when activated: {err}"),
                });
            }
            results.push(format!("CPI Batch (2x transfer): {cpi_batch_cus} CUs"));
        } else {
            // SPL Token: expect higher CUs (> 2000 per instruction)
            if transfer_cus < 2000 {
                return Ok(TestOutcome::Fail {
                    message: format!(
                        "Feature not activated but Transfer CUs too low: {transfer_cus} (expected > 2000)"
                    ),
                });
            }
            if transfer_checked_cus < 2000 {
                return Ok(TestOutcome::Fail {
                    message: format!(
                        "Feature not activated but TransferChecked CUs too low: {transfer_checked_cus} (expected > 2000)"
                    ),
                });
            }
            if mint_to_cus < 2000 {
                return Ok(TestOutcome::Fail {
                    message: format!(
                        "Feature not activated but MintTo CUs too low: {mint_to_cus} (expected > 2000)"
                    ),
                });
            }
        }

        Ok(TestOutcome::Pass {
            message: results.join(" | "),
        })
    }
}

pub fn register() -> Box<dyn SimdTest> {
    Box::new(Simd0266Test)
}
