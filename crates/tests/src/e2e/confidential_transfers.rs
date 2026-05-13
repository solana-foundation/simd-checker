// End-to-end confidential-transfer check.
//
// Exercises the full token-2022 ConfidentialTransfer extension flow against
// a target network and asserts that:
//   1. The auditor can decrypt the on-chain transfer ciphertext to the
//      cleartext amount (validates the ZK ElGamal Proof program at runtime).
//   2. Public balances are consistent post-deposit / post-withdraw.
//
// Localnet note: surfnet is started with an upstream RPC URL by the CLI
// runner (see `crates/cli/src/main.rs` and `start_surfnet_with_upstream`)
// so the token-2022 BPF program is cloned on demand. If localnet ever
// starts up in offline mode, the runner's RequirementChecker will mark
// this test `Pending` rather than panicking here.
//
// Versions chosen to match Confidential-Balances-Sample as of 2026-05.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::sync::Arc;

use solana_client::{
    nonblocking::rpc_client::RpcClient as NonblockingRpcClient,
    rpc_client::RpcClient as BlockingRpcClient,
    rpc_config::{CommitmentConfig, RpcSendTransactionConfig, RpcTransactionConfig},
};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::Transaction;
use std::num::NonZeroI8;

use solana_transaction_status_client_types::{
    EncodedTransaction, UiMessage, UiTransactionEncoding,
};
use spl_token_2022::{
    extension::{
        confidential_transfer::{
            account_info::TransferAccountInfo, instruction::{
                configure_account as ct_configure_account, PubkeyValidityProofData,
                TransferInstructionData,
            }, ConfidentialTransferAccount,
        },
        BaseStateWithExtensions, ExtensionType,
    },
    instruction::decode_instruction_data,
    solana_zk_sdk::{
        encryption::{
            auth_encryption::AeKey,
            elgamal::{ElGamalCiphertext, ElGamalKeypair},
        },
        zk_elgamal_proof_program::proof_data::ZkProofData,
    },
};
use spl_token_client::{
    client::{ProgramRpcClient, ProgramRpcClientSendTransaction, RpcClientResponse},
    token::{ExtensionInitializationParams, ProofAccountWithCiphertext, Token},
};
use spl_token_confidential_transfer_proof_extraction::instruction::ProofLocation;
use spl_token_confidential_transfer_proof_generation::{
    try_combine_lo_hi_ciphertexts, TRANSFER_AMOUNT_LO_BITS,
};

use super::proofs_v5;
use test_common::{E2eContext, E2eTest, LabeledTransactionSignature, TestOutcome};

const MINT_DECIMALS: u8 = 2;
const MINT_AMOUNT: u64 = 100_00; // 100.00 tokens
const DEPOSIT_AMOUNT: u64 = 50_00;
const TRANSFER_AMOUNT: u64 = 50_00;
const WITHDRAW_AMOUNT: u64 = 20_00;

pub struct ConfidentialTransfersE2e;

#[async_trait(?Send)]
impl E2eTest for ConfidentialTransfersE2e {
    fn id(&self) -> &'static str {
        "e2e_confidential_transfers"
    }

    async fn run(&self, ctx: E2eContext) -> Result<TestOutcome> {
        // Localnet skip: surfnet/litesvm bundles an older agave whose
        // on-chain `ZkE1Gama1Proof…` builtin is built against
        // `solana-zk-sdk` 4.x, while this test now generates proofs with
        // 5.0.1 to match remote networks (testnet/devnet on agave 4.x).
        // Running on localnet would fail with
        // `SigmaProof(PubkeyValidity, AlgebraicRelation)` — same error
        // the old v4-proof path produced on testnet, just for the
        // opposite-skew reason.
        if ctx.network_name == "localnet" {
            return Ok(TestOutcome::Skip {
                reason: "localnet bundles an older agave whose ZK ElGamal proof verifier is \
                         zk-sdk 4.x; this test generates v5 proofs to match remote networks. \
                         Run against devnet/testnet/mainnet."
                    .to_string(),
            });
        }

        let mut sigs: Vec<LabeledTransactionSignature> = Vec::new();
        match run_inner(&ctx, &mut sigs).await {
            Ok(message) => Ok(TestOutcome::Pass {
                message,
                tx_signatures: sigs,
            }),
            Err(e) => Ok(TestOutcome::Fail {
                message: format!("confidential transfers failed: {e:#}"),
                tx_signatures: sigs,
            }),
        }
    }
}

async fn run_inner(
    ctx: &E2eContext,
    sigs: &mut Vec<LabeledTransactionSignature>,
) -> Result<String> {
    // ---- Required program: token-2022 ----
    let expected_token_program = spl_token_2022::id();
    let token_program_id = ctx
        .required_programs
        .first()
        .copied()
        .context("missing required token-2022 program in e2e context")?;
    if token_program_id != expected_token_program {
        anyhow::bail!(
            "required program {} does not match spl_token_2022::id() {}",
            token_program_id,
            expected_token_program
        );
    }

    // ---- Nonblocking RPC + token-client wrapper ----
    let nonblocking_rpc = Arc::new(NonblockingRpcClient::new_with_commitment(
        ctx.rpc_url.clone(),
        CommitmentConfig::confirmed(),
    ));
    let program_client = Arc::new(ProgramRpcClient::new(
        Arc::clone(&nonblocking_rpc),
        ProgramRpcClientSendTransaction,
    ));

    // ---- Keypairs ----
    let payer_arc: Arc<dyn Signer> = Arc::new(ctx.payer.insecure_clone());
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    let sender_owner = Keypair::new();
    let recipient_owner = Keypair::new();
    let auditor = ElGamalKeypair::new_rand();

    let token = Token::new(
        program_client,
        &token_program_id,
        &mint.pubkey(),
        Some(MINT_DECIMALS),
        payer_arc,
    );

    // ---- 1. Create mint with ConfidentialTransferMint extension ----
    let sig = token
        .create_mint(
            &mint_authority.pubkey(),
            None,
            vec![ExtensionInitializationParams::ConfidentialTransferMint {
                authority: Some(mint_authority.pubkey()),
                auto_approve_new_accounts: true,
                auditor_elgamal_pubkey: Some((*auditor.pubkey()).into()),
            }],
            &[&mint],
        )
        .await
        .context("create_mint")?;
    sigs.push(label("create-mint", sig));

    // ---- 2. Sender confidential account ----
    let sender_token_account_kp = Keypair::new();
    let sender_token_account = sender_token_account_kp.pubkey();
    let sig = token
        .create_auxiliary_token_account_with_extension_space(
            &sender_token_account_kp,
            &sender_owner.pubkey(),
            vec![ExtensionType::ConfidentialTransferAccount],
        )
        .await
        .context("create sender token account")?;
    sigs.push(label("sender-account-create", sig));

    let sender_elgamal =
        ElGamalKeypair::new_from_signer(&sender_owner, &sender_token_account.to_bytes())
            .map_err(|e| anyhow::anyhow!("sender elgamal derive: {e}"))?;
    let sender_aes = AeKey::new_from_signer(&sender_owner, &sender_token_account.to_bytes())
        .map_err(|e| anyhow::anyhow!("sender aes derive: {e}"))?;
    self_verify_pubkey_validity(&sender_elgamal)
        .context("self-verify sender pubkey-validity proof")?;

    // Configure-account uses a v5-generated PubkeyValidity proof (see
    // proofs_v5.rs for the version-skew rationale). We bypass
    // `spl-token-client`'s `confidential_transfer_configure_token_account`
    // wrapper because it internally calls the v4 `PubkeyValidityProofData::new`,
    // which produces proofs that the v5 on-chain `ZkE1Gama1Proof…` builtin
    // rejects with `AlgebraicRelation`.
    let sender_elgamal_v5 =
        proofs_v5::derive_elgamal_v5(&sender_owner, &sender_token_account.to_bytes())?;
    let sender_aes_v5 = proofs_v5::derive_ae_v5(&sender_owner, &sender_token_account.to_bytes())?;
    let sig = configure_account_with_v5_proof(
        nonblocking_rpc.as_ref(),
        &ctx.payer,
        &sender_owner,
        &sender_token_account,
        &mint.pubkey(),
        &sender_elgamal_v5,
        &sender_aes_v5,
    )
    .await
    .with_context(|| {
        format!(
            "configure sender account (v5 proof — sender_elgamal_pubkey={})",
            sender_elgamal.pubkey()
        )
    })?;
    sigs.push(label_sig("sender-account-configure", sig));

    // ---- 3. Recipient confidential account ----
    let recipient_token_account_kp = Keypair::new();
    let recipient_token_account = recipient_token_account_kp.pubkey();
    let sig = token
        .create_auxiliary_token_account_with_extension_space(
            &recipient_token_account_kp,
            &recipient_owner.pubkey(),
            vec![ExtensionType::ConfidentialTransferAccount],
        )
        .await
        .context("create recipient token account")?;
    sigs.push(label("recipient-account-create", sig));

    let recipient_elgamal =
        ElGamalKeypair::new_from_signer(&recipient_owner, &recipient_token_account.to_bytes())
            .map_err(|e| anyhow::anyhow!("recipient elgamal derive: {e}"))?;
    let recipient_aes =
        AeKey::new_from_signer(&recipient_owner, &recipient_token_account.to_bytes())
            .map_err(|e| anyhow::anyhow!("recipient aes derive: {e}"))?;
    self_verify_pubkey_validity(&recipient_elgamal)
        .context("self-verify recipient pubkey-validity proof")?;

    let recipient_elgamal_v5 =
        proofs_v5::derive_elgamal_v5(&recipient_owner, &recipient_token_account.to_bytes())?;
    let recipient_aes_v5 =
        proofs_v5::derive_ae_v5(&recipient_owner, &recipient_token_account.to_bytes())?;
    let sig = configure_account_with_v5_proof(
        nonblocking_rpc.as_ref(),
        &ctx.payer,
        &recipient_owner,
        &recipient_token_account,
        &mint.pubkey(),
        &recipient_elgamal_v5,
        &recipient_aes_v5,
    )
    .await
    .context("configure recipient account (v5 proof)")?;
    sigs.push(label_sig("recipient-account-configure", sig));

    // ---- 4. Mint public tokens to sender ----
    let sig = token
        .mint_to(
            &sender_token_account,
            &mint_authority.pubkey(),
            MINT_AMOUNT,
            &[&mint_authority],
        )
        .await
        .context("mint_to sender")?;
    sigs.push(label("mint-to-sender", sig));

    // ---- 5. Deposit half to confidential balance ----
    let sig = token
        .confidential_transfer_deposit(
            &sender_token_account,
            &sender_owner.pubkey(),
            DEPOSIT_AMOUNT,
            MINT_DECIMALS,
            &[&sender_owner],
        )
        .await
        .context("deposit to confidential balance")?;
    sigs.push(label("sender-deposit", sig));

    // ---- 6. Apply pending balance (sender) ----
    let sig = token
        .confidential_transfer_apply_pending_balance(
            &sender_token_account,
            &sender_owner.pubkey(),
            None,
            sender_elgamal.secret(),
            &sender_aes,
            &[&sender_owner],
        )
        .await
        .context("apply pending balance (sender)")?;
    sigs.push(label("sender-apply-pending", sig));

    // ---- 7. Confidential transfer (v5 proofs via context-state accounts) ----
    //
    // Inline-proof transfer txs blow the 1232-byte limit (3 proofs × ~1KB).
    // We pre-generate proofs with zk-sdk 5.0.1 (to match the on-chain
    // verifier on agave 4.x), submit each proof to its own context-state
    // account, then reference those accounts from a small Transfer
    // instruction. After the transfer settles we close the context-state
    // accounts to recover rent.
    let transfer_sig = run_confidential_transfer(
        &token,
        nonblocking_rpc.as_ref(),
        &ctx.payer,
        &sender_owner,
        &sender_token_account,
        &recipient_token_account,
        &sender_aes,
        &sender_elgamal_v5,
        &sender_aes_v5,
        recipient_elgamal.pubkey(),
        auditor.pubkey(),
        TRANSFER_AMOUNT,
        sigs,
    )
    .await
    .context("confidential transfer")?;

    // ---- 8. Apply pending balance (recipient) ----
    let sig = token
        .confidential_transfer_apply_pending_balance(
            &recipient_token_account,
            &recipient_owner.pubkey(),
            None,
            recipient_elgamal.secret(),
            &recipient_aes,
            &[&recipient_owner],
        )
        .await
        .context("apply pending balance (recipient)")?;
    sigs.push(label("recipient-apply-pending", sig));

    // ---- 9. Withdraw to recipient public balance (v5 proofs via context-state accounts) ----
    let _ = run_confidential_withdraw(
        &token,
        nonblocking_rpc.as_ref(),
        &ctx.payer,
        &recipient_owner,
        &recipient_token_account,
        &recipient_aes,
        &recipient_elgamal_v5,
        &recipient_aes_v5,
        WITHDRAW_AMOUNT,
        sigs,
    )
    .await
    .context("withdraw from recipient confidential balance")?;

    // ---- 10. Auditor decrypt assertion ----
    let decrypted = auditor_decrypt_transfer_amount(&ctx.rpc_client, &transfer_sig, &auditor)
        .context("auditor decrypt")?;
    if decrypted != TRANSFER_AMOUNT {
        anyhow::bail!(
            "auditor decrypted amount mismatch: expected {}, got {}",
            TRANSFER_AMOUNT,
            decrypted
        );
    }

    // ---- 11. Balance assertions ----
    let sender_state = token
        .get_account_info(&sender_token_account)
        .await
        .context("fetch sender account state")?;
    let recipient_state = token
        .get_account_info(&recipient_token_account)
        .await
        .context("fetch recipient account state")?;
    let expected_sender_public = MINT_AMOUNT - DEPOSIT_AMOUNT;
    let expected_recipient_public = WITHDRAW_AMOUNT;
    if sender_state.base.amount != expected_sender_public {
        anyhow::bail!(
            "sender public balance mismatch: expected {}, got {}",
            expected_sender_public,
            sender_state.base.amount
        );
    }
    if recipient_state.base.amount != expected_recipient_public {
        anyhow::bail!(
            "recipient public balance mismatch: expected {}, got {}",
            expected_recipient_public,
            recipient_state.base.amount
        );
    }

    Ok(format!(
        "confidential transfer end-to-end verified: auditor decrypted {}, sender public {}, recipient public {}",
        decrypted, sender_state.base.amount, recipient_state.base.amount
    ))
}

/// Generate a fresh `PubkeyValidityProofData` from the given ElGamal keypair
/// and verify it client-side. Fails fast with a clear error if proof
/// generation or self-verification fails — this isolates client-side bugs
/// from on-chain validator issues when the same call later fails on
/// `confidential_transfer_configure_token_account` (which generates its
/// own randomized proof internally).
fn self_verify_pubkey_validity(keypair: &ElGamalKeypair) -> Result<()> {
    let data = PubkeyValidityProofData::new(keypair)
        .map_err(|e| anyhow::anyhow!("PubkeyValidityProofData::new failed: {e:?}"))?;
    data.verify_proof().map_err(|e| {
        anyhow::anyhow!(
            "client-side pubkey-validity proof self-verify failed: {e:?}. \
             This indicates a broken ElGamal keypair or local zk-sdk bug."
        )
    })?;
    Ok(())
}

/// Build and send the `ConfigureAccount` transaction with a v5-generated
/// `PubkeyValidityProofData`. Returns the on-chain signature.
///
/// This bypasses `spl-token-client`'s `confidential_transfer_configure_token_account`
/// (which internally generates a v4 proof) in favour of building the two-
/// instruction sequence (`ConfigureAccount` + `VerifyPubkeyValidity`) by
/// hand with a v5-generated proof whose transcript matches the validator's
/// on-chain verifier (zk-sdk 5.0.1 on agave 4.x networks).
async fn configure_account_with_v5_proof(
    rpc: &NonblockingRpcClient,
    payer: &Keypair,
    owner: &Keypair,
    token_account: &solana_pubkey::Pubkey,
    mint: &solana_pubkey::Pubkey,
    elgamal_v5: &solana_zk_sdk_v5::encryption::elgamal::ElGamalKeypair,
    aes_v5: &solana_zk_sdk_v5::encryption::auth_encryption::AeKey,
) -> Result<Signature> {
    // Maximum pending balance credit counter — same default
    // `spl-token-client` uses when not overridden.
    const MAX_PENDING_BALANCE_CREDIT_COUNTER: u64 = 65_536;

    let proof_data = proofs_v5::pubkey_validity_proof(elgamal_v5)
        .context("generate v5 PubkeyValidity proof")?;
    let decryptable_zero = proofs_v5::encrypt_zero_balance_v4(aes_v5)
        .context("encrypt zero decryptable balance under v5 AeKey")?;

    // Proof is placed at instruction index 1 (one after ConfigureAccount).
    let proof_location = ProofLocation::InstructionOffset(
        NonZeroI8::new(1).expect("1 != 0"),
        &proof_data,
    );

    let instructions = ct_configure_account(
        &spl_token_2022::id(),
        token_account,
        mint,
        &decryptable_zero,
        MAX_PENDING_BALANCE_CREDIT_COUNTER,
        &owner.pubkey(),
        &[],
        proof_location,
    )
    .map_err(|e| anyhow::anyhow!("build configure_account instructions: {e:?}"))?;

    let blockhash = rpc
        .get_latest_blockhash()
        .await
        .context("get_latest_blockhash")?;
    let message = Message::new_with_blockhash(&instructions, Some(&payer.pubkey()), &blockhash);
    let tx = Transaction::new(&[payer, owner], message, blockhash);
    // Skip preflight to avoid `Blockhash not found` flakes on remote
    // networks where the simulating node hasn't seen our freshly-fetched
    // blockhash yet. Match the pattern used by `simd_0266.rs:228-264`.
    let signature = rpc
        .send_transaction_with_config(
            &tx,
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        )
        .await
        .context("send configure_account transaction")?;
    rpc.confirm_transaction_with_commitment(&signature, CommitmentConfig::confirmed())
        .await
        .context("confirm configure_account transaction")?;
    // Surface on-chain errors that skip_preflight=true would otherwise
    // hide. Fetch the signature status and bail with the chain error.
    let statuses = rpc
        .get_signature_statuses(&[signature])
        .await
        .context("get_signature_statuses for configure_account")?;
    if let Some(Some(status)) = statuses.value.into_iter().next() {
        if let Some(err) = status.err {
            anyhow::bail!(
                "configure_account transaction {signature} failed on-chain: {err}"
            );
        }
    }
    Ok(signature)
}

/// Wrap a raw `Signature` as a `LabeledTransactionSignature` with success=true.
fn label_sig(name: &str, sig: Signature) -> LabeledTransactionSignature {
    LabeledTransactionSignature {
        label: name.to_string(),
        signature: sig.to_string(),
        success: true,
        error: None,
    }
}

fn label(name: &str, resp: RpcClientResponse) -> LabeledTransactionSignature {
    let signature = match resp {
        RpcClientResponse::Signature(sig) => sig.to_string(),
        RpcClientResponse::Transaction(_) => "<offline tx>".to_string(),
        RpcClientResponse::Simulation(_) => "<simulation>".to_string(),
    };
    LabeledTransactionSignature {
        label: name.to_string(),
        signature,
        success: true,
        error: None,
    }
}

/// Extract a [`Signature`] from a [`RpcClientResponse`] returned by the
/// `spl_token_client::Token` API. Errors if the response is not a signed
/// online send.
fn extract_signature(resp: &RpcClientResponse) -> Result<Signature> {
    match resp {
        RpcClientResponse::Signature(sig) => Ok(*sig),
        _ => anyhow::bail!("expected RpcClientResponse::Signature"),
    }
}

/// Fetch the transfer transaction, locate the `Transfer` instruction, and
/// decrypt the auditor ciphertext into a cleartext `u64` amount.
fn auditor_decrypt_transfer_amount(
    rpc: &BlockingRpcClient,
    sig: &Signature,
    auditor: &ElGamalKeypair,
) -> Result<u64> {
    let config = RpcTransactionConfig {
        encoding: Some(UiTransactionEncoding::Json),
        commitment: Some(CommitmentConfig::confirmed()),
        max_supported_transaction_version: Some(0),
    };
    let tx = rpc
        .get_transaction_with_config(sig, config)
        .context("fetch transfer transaction")?;
    let EncodedTransaction::Json(ui_tx) = tx.transaction.transaction else {
        anyhow::bail!("unexpected transaction encoding");
    };
    let UiMessage::Raw(raw) = ui_tx.message else {
        anyhow::bail!("unexpected message encoding");
    };
    // The transfer tx may include compute-budget ixs in addition to the
    // ConfidentialTransfer::Transfer ix; scan defensively.
    for ix in &raw.instructions {
        let Ok(input) = bs58::decode(&ix.data).into_vec() else {
            continue;
        };
        if input.is_empty() {
            continue;
        }
        let payload = &input[1..];
        if let Ok(decoded) = decode_instruction_data::<TransferInstructionData>(payload) {
            let ct_lo: ElGamalCiphertext = decoded
                .transfer_amount_auditor_ciphertext_lo
                .try_into()
                .map_err(|e| anyhow::anyhow!("decode auditor ciphertext lo: {e:?}"))?;
            let ct_hi: ElGamalCiphertext = decoded
                .transfer_amount_auditor_ciphertext_hi
                .try_into()
                .map_err(|e| anyhow::anyhow!("decode auditor ciphertext hi: {e:?}"))?;
            let combined = try_combine_lo_hi_ciphertexts(&ct_lo, &ct_hi, TRANSFER_AMOUNT_LO_BITS)
                .ok_or_else(|| anyhow::anyhow!("failed to combine ciphertexts"))?;
            let decrypted = auditor.secret().decrypt(&combined);
            let amount = decrypted
                .decode_u32()
                .ok_or_else(|| anyhow::anyhow!("failed to decode u32"))?;
            return Ok(amount as u64);
        }
    }
    anyhow::bail!("no TransferInstructionData found in transfer transaction");
}

pub fn register() -> Box<dyn E2eTest> {
    Box::new(ConfidentialTransfersE2e)
}

// ---------------------------------------------------------------------------
// V5 proof + context-state-account orchestration
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn run_confidential_transfer(
    token: &Token<ProgramRpcClientSendTransaction>,
    rpc: &NonblockingRpcClient,
    payer: &Keypair,
    source_owner: &Keypair,
    source_token_account: &solana_pubkey::Pubkey,
    destination_token_account: &solana_pubkey::Pubkey,
    source_aes_v4: &AeKey,
    source_elgamal_v5: &solana_zk_sdk_v5::encryption::elgamal::ElGamalKeypair,
    source_aes_v5: &solana_zk_sdk_v5::encryption::auth_encryption::AeKey,
    destination_elgamal_pubkey_v4: &spl_token_2022::solana_zk_sdk::encryption::elgamal::ElGamalPubkey,
    auditor_elgamal_pubkey_v4: &spl_token_2022::solana_zk_sdk::encryption::elgamal::ElGamalPubkey,
    transfer_amount: u64,
    sigs: &mut Vec<LabeledTransactionSignature>,
) -> Result<Signature> {
    // 1. Fetch current account state for the source.
    let acc_info = token
        .get_account_info(source_token_account)
        .await
        .context("fetch source account info")?;
    let ct_acc = acc_info
        .get_extension::<ConfidentialTransferAccount>()
        .context("get ConfidentialTransferAccount extension")?;

    // 2. Decrypt the current available balance under v5 AES.
    let current_balance =
        proofs_v5::decrypt_decryptable_balance_v5(source_aes_v5, &ct_acc.decryptable_available_balance)
            .context("decrypt current decryptable available balance")?;

    // 3. Reinterpret the on-chain available_balance ciphertext as v5.
    let current_available_ct_v5 =
        proofs_v5::pod_v4_to_v5_ciphertext(&ct_acc.available_balance)
            .context("decode current available balance ciphertext into v5")?;

    // 4. Convert destination + auditor pubkeys to v5.
    let dest_bytes: [u8; 32] = (*destination_elgamal_pubkey_v4).into();
    let auditor_bytes: [u8; 32] = (*auditor_elgamal_pubkey_v4).into();
    let destination_v5 = proofs_v5::elgamal_pubkey_from_bytes_v5(&dest_bytes)?;
    let auditor_v5 = proofs_v5::elgamal_pubkey_from_bytes_v5(&auditor_bytes)?;

    // 5. Generate the three transfer proofs with v5 transcripts.
    let transfer_proofs = proofs_v5::transfer_proof_data_v5(
        &current_available_ct_v5,
        current_balance,
        transfer_amount,
        source_elgamal_v5,
        source_aes_v5,
        &destination_v5,
        &auditor_v5,
    )
    .context("generate v5 transfer proofs")?;

    // 6. Create three context-state accounts (one per proof). The
    // `split_account_creation_and_proof_verification = false` path
    // submits create_account + verify_proof in a single tx; each fits
    // within 1232 bytes for these proof sizes.
    let proof_authority = Keypair::new();
    let equality_kp = Keypair::new();
    let validity_kp = Keypair::new();
    let range_kp = Keypair::new();

    let sig = token
        .confidential_transfer_create_context_state_account(
            &equality_kp.pubkey(),
            &proof_authority.pubkey(),
            &transfer_proofs.equality_proof_data,
            true,
            &[&equality_kp],
        )
        .await
        .context("create equality context-state account")?;
    sigs.push(label("transfer-ctx-equality-create", sig));

    let sig = token
        .confidential_transfer_create_context_state_account(
            &validity_kp.pubkey(),
            &proof_authority.pubkey(),
            &transfer_proofs.ciphertext_validity_proof_data,
            true,
            &[&validity_kp],
        )
        .await
        .context("create ciphertext-validity context-state account")?;
    sigs.push(label("transfer-ctx-validity-create", sig));

    // Range proof (U128 = ~1.4 KB) won't fit in a verify_proof tx even
    // with the split-create path. Use the record-account chunked-write
    // flow instead: write the proof into a record account, then
    // verify_proof_from_account.
    let range_record_kp = Keypair::new();
    let _record_sigs = token
        .confidential_transfer_create_record_account(
            &range_record_kp.pubkey(),
            &proof_authority.pubkey(),
            &transfer_proofs.range_proof_data,
            &range_record_kp,
            &proof_authority,
        )
        .await
        .context("create range record account")?;
    sigs.push(LabeledTransactionSignature {
        label: "transfer-range-record-create".to_string(),
        signature: "<batched>".to_string(),
        success: true,
        error: None,
    });
    let sig = token
        .confidential_transfer_create_context_state_account_from_record::<_, spl_token_2022::solana_zk_sdk::zk_elgamal_proof_program::proof_data::BatchedRangeProofU128Data, _>(
            &range_kp.pubkey(),
            &proof_authority.pubkey(),
            &range_record_kp.pubkey(),
            &[&range_kp],
        )
        .await
        .context("create range context-state account from record")?;
    sigs.push(label("transfer-ctx-range-create", sig));

    // 7. Build TransferAccountInfo from the already-fetched state.
    let account_info = TransferAccountInfo::new(ct_acc);

    // 8. Submit the Transfer instruction referencing the three context accounts.
    let validity_with_ct = ProofAccountWithCiphertext {
        context_state_account: validity_kp.pubkey(),
        ciphertext_lo: transfer_proofs.auditor_ciphertext_lo,
        ciphertext_hi: transfer_proofs.auditor_ciphertext_hi,
    };
    let equality_account = equality_kp.pubkey();
    let range_account = range_kp.pubkey();

    let transfer_resp = token
        .confidential_transfer_transfer(
            source_token_account,
            destination_token_account,
            &source_owner.pubkey(),
            Some(&equality_account),
            Some(&validity_with_ct),
            Some(&range_account),
            transfer_amount,
            Some(account_info),
            // The internal "regenerate proof / recompute new_decryptable"
            // path is skipped because all three proof accounts are Some,
            // but new_decryptable_available_balance is still computed via
            // v4 AES decryption of current decryptable_balance. v4 and v5
            // AES keys derive identically from the same signer+seed, so
            // this produces the byte-identical ciphertext we expect.
            &ElGamalKeypair::new_rand(), // unused when equality_proof_account is Some
            source_aes_v4,
            destination_elgamal_pubkey_v4,
            Some(auditor_elgamal_pubkey_v4),
            &[source_owner],
        )
        .await
        .context("submit Transfer instruction")?;
    let transfer_sig = extract_signature(&transfer_resp)?;
    sigs.push(label("confidential-transfer", transfer_resp));

    // 9. Close the context-state accounts, returning lamports to the payer.
    for (label_name, ctx_kp) in [
        ("transfer-ctx-equality-close", &equality_kp),
        ("transfer-ctx-validity-close", &validity_kp),
        ("transfer-ctx-range-close", &range_kp),
    ] {
        let sig = token
            .confidential_transfer_close_context_state_account(
                &ctx_kp.pubkey(),
                &payer.pubkey(),
                &proof_authority.pubkey(),
                &[&proof_authority],
            )
            .await
            .with_context(|| format!("close context-state account {}", ctx_kp.pubkey()))?;
        sigs.push(label(label_name, sig));
    }

    // Reclaim the record account's lamports too.
    let sig = token
        .confidential_transfer_close_record_account(
            &range_record_kp.pubkey(),
            &payer.pubkey(),
            &proof_authority.pubkey(),
            &[&proof_authority],
        )
        .await
        .context("close range record account")?;
    sigs.push(label("transfer-range-record-close", sig));

    // Suppress unused warning for `rpc` (kept in signature for symmetry
    // with run_confidential_withdraw, which uses it for direct sends).
    let _ = rpc;

    Ok(transfer_sig)
}

#[allow(clippy::too_many_arguments)]
async fn run_confidential_withdraw(
    token: &Token<ProgramRpcClientSendTransaction>,
    rpc: &NonblockingRpcClient,
    payer: &Keypair,
    owner: &Keypair,
    token_account: &solana_pubkey::Pubkey,
    aes_v4: &AeKey,
    elgamal_v5: &solana_zk_sdk_v5::encryption::elgamal::ElGamalKeypair,
    aes_v5: &solana_zk_sdk_v5::encryption::auth_encryption::AeKey,
    withdraw_amount: u64,
    sigs: &mut Vec<LabeledTransactionSignature>,
) -> Result<()> {
    let _ = rpc;

    let acc_info = token
        .get_account_info(token_account)
        .await
        .context("fetch withdraw source account info")?;
    let ct_acc = acc_info
        .get_extension::<ConfidentialTransferAccount>()
        .context("get ConfidentialTransferAccount extension")?;

    let current_balance =
        proofs_v5::decrypt_decryptable_balance_v5(aes_v5, &ct_acc.decryptable_available_balance)
            .context("decrypt current decryptable balance (withdraw)")?;

    let current_available_ct_v5 =
        proofs_v5::pod_v4_to_v5_ciphertext(&ct_acc.available_balance)?;

    let proofs = proofs_v5::withdraw_proof_data_v5(
        &current_available_ct_v5,
        current_balance,
        withdraw_amount,
        elgamal_v5,
        aes_v5,
    )
    .context("generate v5 withdraw proofs")?;

    let proof_authority = Keypair::new();
    let equality_kp = Keypair::new();
    let range_kp = Keypair::new();

    let sig = token
        .confidential_transfer_create_context_state_account(
            &equality_kp.pubkey(),
            &proof_authority.pubkey(),
            &proofs.equality_proof_data,
            true,
            &[&equality_kp],
        )
        .await
        .context("create withdraw equality context-state account")?;
    sigs.push(label("withdraw-ctx-equality-create", sig));

    let sig = token
        .confidential_transfer_create_context_state_account(
            &range_kp.pubkey(),
            &proof_authority.pubkey(),
            &proofs.range_proof_data,
            true,
            &[&range_kp],
        )
        .await
        .context("create withdraw range context-state account")?;
    sigs.push(label("withdraw-ctx-range-create", sig));

    let equality_account = equality_kp.pubkey();
    let range_account = range_kp.pubkey();

    let withdraw_resp = token
        .confidential_transfer_withdraw(
            token_account,
            &owner.pubkey(),
            Some(&equality_account),
            Some(&range_account),
            withdraw_amount,
            MINT_DECIMALS,
            None,
            elgamal_v5_to_v4(elgamal_v5)?.as_ref(),
            aes_v4,
            &[owner],
        )
        .await
        .context("submit Withdraw instruction")?;
    sigs.push(label("recipient-withdraw", withdraw_resp));

    for (label_name, ctx_kp) in [
        ("withdraw-ctx-equality-close", &equality_kp),
        ("withdraw-ctx-range-close", &range_kp),
    ] {
        let sig = token
            .confidential_transfer_close_context_state_account(
                &ctx_kp.pubkey(),
                &payer.pubkey(),
                &proof_authority.pubkey(),
                &[&proof_authority],
            )
            .await
            .with_context(|| format!("close withdraw context-state account {}", ctx_kp.pubkey()))?;
        sigs.push(label(label_name, sig));
    }

    Ok(())
}

/// Reinterpret a v5 ElGamal keypair as a v4 keypair via the byte-stable
/// secret-key encoding. Used because spl-token-client's withdraw API takes
/// `&ElGamalKeypair` (v4) but we want the v5-derived keypair for symmetry
/// with proof generation. Since v4 and v5 derive identically from the same
/// signer+seed, this is just a type-system bridge — the bytes are equal.
fn elgamal_v5_to_v4(
    v5: &solana_zk_sdk_v5::encryption::elgamal::ElGamalKeypair,
) -> Result<Box<ElGamalKeypair>> {
    let bytes: [u8; 64] = v5.into();
    let v4 = <ElGamalKeypair as std::convert::TryFrom<&[u8]>>::try_from(&bytes[..])
        .map_err(|e| anyhow::anyhow!("ElGamalKeypair v5→v4 bridge: {e:?}"))?;
    Ok(Box::new(v4))
}
