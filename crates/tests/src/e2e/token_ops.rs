//! Hand-rolled orchestration helpers replacing `spl_token_client::Token`.
//!
//! `spl-token-client = 0.18` hard-pins `spl-token-2022 = ^10.0.0`, so it
//! cannot be used alongside `spl-token-2022 = 11.0.0`. Every operation
//! previously delegated to `Token::*` is implemented here against the
//! `spl-token-2022-interface = 3.0.0` instruction builders directly.
//!
//! Send-and-confirm matches `crates/tests/src/simd_0266.rs:228-264`:
//! `skip_preflight = true` + status polling — testnet-resilient against
//! `Blockhash not found` preflight races.

use anyhow::{anyhow, bail, Context, Result};
use log::debug;
use solana_client::{
    nonblocking::rpc_client::RpcClient,
    rpc_config::{CommitmentConfig, RpcSendTransactionConfig},
};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_system_interface::instruction as system_instruction;
use solana_transaction::Transaction;
use std::time::{Duration, Instant};

use spl_token_2022_interface::{
    extension::{
        confidential_transfer::{
            instruction as ct_ix, ConfidentialTransferAccount, DecryptableBalance,
        },
        BaseStateWithExtensions, ExtensionType, StateWithExtensionsOwned,
    },
    instruction as token_ix,
    state::{Account, Mint},
};

use solana_zk_elgamal_proof_interface::{
    instruction::{ContextStateInfo, ProofInstruction},
    proof_data::{PubkeyValidityProofData, ZkProofData},
};
use solana_zk_sdk::{
    encryption::{auth_encryption::AeKey, elgamal::ElGamalKeypair},
    zk_elgamal_proof_program::pubkey_validity::build_pubkey_validity_proof_data,
};
use solana_zk_sdk_pod::encryption::elgamal::PodElGamalPubkey;
use spl_token_confidential_transfer_proof_extraction::instruction::ProofLocation;

/// Maximum proof bytes to write per `spl_record::Write` instruction so the
/// resulting tx stays under the 1232-byte VersionedTransaction limit.
const RECORD_WRITE_CHUNK: usize = 900;

/// Maximum pending balance credit counter (matches spl-token-client default).
pub const MAX_PENDING_BALANCE_CREDIT_COUNTER: u64 = 65_536;

/// Send a transaction with `skip_preflight = true`, poll for confirmation,
/// then fetch the signature status and bail if the tx failed on-chain.
pub async fn send_ixs(
    rpc: &RpcClient,
    payer: &Keypair,
    signers: &[&Keypair],
    instructions: &[Instruction],
) -> Result<Signature> {
    let blockhash = rpc
        .get_latest_blockhash()
        .await
        .context("get_latest_blockhash")?;
    let message = Message::new_with_blockhash(instructions, Some(&payer.pubkey()), &blockhash);

    let mut all_signers: Vec<&Keypair> = vec![payer];
    for s in signers {
        if !all_signers.iter().any(|x| x.pubkey() == s.pubkey()) {
            all_signers.push(*s);
        }
    }

    let tx = Transaction::new(&all_signers, message, blockhash);
    let signature = rpc
        .send_transaction_with_config(
            &tx,
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        )
        .await
        .context("send_transaction_with_config")?;

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if Instant::now() >= deadline {
            bail!("transaction {signature} not confirmed within 60s");
        }
        let statuses = rpc
            .get_signature_statuses(&[signature])
            .await
            .context("get_signature_statuses")?;
        if let Some(Some(status)) = statuses.value.into_iter().next() {
            if let Some(err) = status.err {
                bail!("transaction {signature} failed on-chain: {err}");
            }
            if status.confirmation_status.is_some() {
                return Ok(signature);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[allow(dead_code)]
pub fn _commitment() -> CommitmentConfig {
    CommitmentConfig::confirmed()
}

/// Fetch a token account's parsed `Account` state with extensions.
pub async fn get_token_account_state(
    rpc: &RpcClient,
    token_account: &Pubkey,
) -> Result<StateWithExtensionsOwned<Account>> {
    let data = rpc
        .get_account_data(token_account)
        .await
        .with_context(|| format!("get_account_data {token_account}"))?;
    StateWithExtensionsOwned::<Account>::unpack(data)
        .map_err(|e| anyhow!("unpack token account {token_account}: {e:?}"))
}

/// Read the `ConfidentialTransferAccount` extension out of a parsed state.
pub fn confidential_transfer_extension(
    state: &StateWithExtensionsOwned<Account>,
) -> Result<&ConfidentialTransferAccount> {
    state
        .get_extension::<ConfidentialTransferAccount>()
        .map_err(|e| anyhow!("get ConfidentialTransferAccount extension: {e:?}"))
}

/// Create a mint account + initialize ConfidentialTransferMint + base mint init.
pub async fn create_confidential_mint(
    rpc: &RpcClient,
    payer: &Keypair,
    mint_kp: &Keypair,
    mint_authority: &Pubkey,
    decimals: u8,
    auditor_elgamal_pubkey: Option<PodElGamalPubkey>,
) -> Result<Signature> {
    let space = ExtensionType::try_calculate_account_len::<Mint>(&[
        ExtensionType::ConfidentialTransferMint,
    ])
    .map_err(|e| anyhow!("calc mint len: {e:?}"))?;
    let rent = rpc
        .get_minimum_balance_for_rent_exemption(space)
        .await
        .context("get rent for mint")?;

    let create_account_ix = system_instruction::create_account(
        &payer.pubkey(),
        &mint_kp.pubkey(),
        rent,
        space as u64,
        &spl_token_2022_interface::id(),
    );
    let init_ct_ix = ct_ix::initialize_mint(
        &spl_token_2022_interface::id(),
        &mint_kp.pubkey(),
        Some(*mint_authority),
        true,
        auditor_elgamal_pubkey,
    )
    .map_err(|e| anyhow!("initialize_mint (ct): {e:?}"))?;
    let init_mint_ix = token_ix::initialize_mint(
        &spl_token_2022_interface::id(),
        &mint_kp.pubkey(),
        mint_authority,
        None,
        decimals,
    )
    .map_err(|e| anyhow!("initialize_mint (base): {e:?}"))?;

    send_ixs(
        rpc,
        payer,
        &[mint_kp],
        &[create_account_ix, init_ct_ix, init_mint_ix],
    )
    .await
}

/// Create an auxiliary token account with space pre-allocated for the
/// `ConfidentialTransferAccount` extension, then initialize the base account.
pub async fn create_confidential_token_account(
    rpc: &RpcClient,
    payer: &Keypair,
    account_kp: &Keypair,
    owner: &Pubkey,
    mint: &Pubkey,
) -> Result<Signature> {
    let space = ExtensionType::try_calculate_account_len::<Account>(&[
        ExtensionType::ConfidentialTransferAccount,
    ])
    .map_err(|e| anyhow!("calc account len: {e:?}"))?;
    let rent = rpc
        .get_minimum_balance_for_rent_exemption(space)
        .await
        .context("get rent for token account")?;

    let create_account_ix = system_instruction::create_account(
        &payer.pubkey(),
        &account_kp.pubkey(),
        rent,
        space as u64,
        &spl_token_2022_interface::id(),
    );
    let init_account_ix = token_ix::initialize_account(
        &spl_token_2022_interface::id(),
        &account_kp.pubkey(),
        mint,
        owner,
    )
    .map_err(|e| anyhow!("initialize_account: {e:?}"))?;

    send_ixs(
        rpc,
        payer,
        &[account_kp],
        &[create_account_ix, init_account_ix],
    )
    .await
}

/// Mint public tokens to a token account.
pub async fn mint_to(
    rpc: &RpcClient,
    payer: &Keypair,
    mint: &Pubkey,
    dest: &Pubkey,
    mint_authority: &Keypair,
    amount: u64,
) -> Result<Signature> {
    let ix = token_ix::mint_to(
        &spl_token_2022_interface::id(),
        mint,
        dest,
        &mint_authority.pubkey(),
        &[],
        amount,
    )
    .map_err(|e| anyhow!("mint_to: {e:?}"))?;
    send_ixs(rpc, payer, &[mint_authority], &[ix]).await
}

/// Configure a token account for confidential transfers — builds
/// `[ConfigureAccount, VerifyPubkeyValidity]` with the proof inline.
pub async fn configure_account(
    rpc: &RpcClient,
    payer: &Keypair,
    owner: &Keypair,
    token_account: &Pubkey,
    mint: &Pubkey,
    elgamal_kp: &ElGamalKeypair,
    aes_key: &AeKey,
) -> Result<Signature> {
    let proof_data = build_pubkey_validity_proof_data(elgamal_kp)
        .map_err(|e| anyhow!("build PubkeyValidityProofData: {e:?}"))?;
    // Type bookkeeping so `PubkeyValidityProofData` import is kept honest.
    let _: &PubkeyValidityProofData = &proof_data;
    let decryptable_zero: DecryptableBalance = aes_key.encrypt(0u64).into();

    let proof_location =
        ProofLocation::InstructionOffset(std::num::NonZeroI8::new(1).expect("1 != 0"), &proof_data);

    let instructions = ct_ix::configure_account(
        &spl_token_2022_interface::id(),
        token_account,
        mint,
        &decryptable_zero,
        MAX_PENDING_BALANCE_CREDIT_COUNTER,
        &owner.pubkey(),
        &[],
        proof_location,
    )
    .map_err(|e| anyhow!("build configure_account ixs: {e:?}"))?;

    send_ixs(rpc, payer, &[owner], &instructions).await
}

/// Confidential `Deposit` (public → pending).
pub async fn confidential_deposit(
    rpc: &RpcClient,
    payer: &Keypair,
    owner: &Keypair,
    token_account: &Pubkey,
    mint: &Pubkey,
    amount: u64,
    decimals: u8,
) -> Result<Signature> {
    let ix = ct_ix::deposit(
        &spl_token_2022_interface::id(),
        token_account,
        mint,
        amount,
        decimals,
        &owner.pubkey(),
        &[],
    )
    .map_err(|e| anyhow!("deposit: {e:?}"))?;
    send_ixs(rpc, payer, &[owner], &[ix]).await
}

/// `ApplyPendingBalance`: roll pending into available.
pub async fn apply_pending_balance(
    rpc: &RpcClient,
    payer: &Keypair,
    owner: &Keypair,
    token_account: &Pubkey,
    expected_pending_balance_credit_counter: u64,
    new_decryptable_available_balance: &DecryptableBalance,
) -> Result<Signature> {
    let ix = ct_ix::apply_pending_balance(
        &spl_token_2022_interface::id(),
        token_account,
        expected_pending_balance_credit_counter,
        new_decryptable_available_balance,
        &owner.pubkey(),
        &[],
    )
    .map_err(|e| anyhow!("apply_pending_balance: {e:?}"))?;
    send_ixs(rpc, payer, &[owner], &[ix]).await
}

/// Create a context-state account and verify a proof into it in one tx.
/// Used for proofs small enough to fit (equality, validity).
///
/// **Size ceiling.** This helper packs `system_instruction::create_account`
/// AND `ProofInstruction::encode_verify_proof(...)` (with the full proof data
/// inlined) into a single transaction, plus a second required signer (the new
/// context-state keypair). The 1232-byte raw `VersionedTransaction` limit
/// leaves room for at most ~700 bytes of inline proof data after accounting
/// for tx headers, account keys, blockhash, signatures, and the create-account
/// ix. Anything larger — notably any batched range proof (U64 ≈ 936 B inline,
/// U128 ≈ 1.4 KB inline) — MUST be routed through `create_record_account` +
/// `create_context_state_account_from_record` instead. Note that "U64" vs.
/// "U128" in the name refers to the total committed-value bit length, not the
/// proof byte size: U64 still includes the same ~264-byte
/// `BatchedRangeProofContext` and a 672-byte `PodRangeProofU64`, which is over
/// the inline budget. See `run_confidential_transfer` and
/// `run_confidential_withdraw` in `confidential_transfers.rs` for the
/// record-account pattern.
pub async fn create_context_state_account<ZK, U>(
    rpc: &RpcClient,
    payer: &Keypair,
    ctx_account_kp: &Keypair,
    ctx_authority: &Pubkey,
    proof_data: &ZK,
    verify_proof_ix: ProofInstruction,
) -> Result<Signature>
where
    ZK: bytemuck::Pod + ZkProofData<U>,
    U: bytemuck::Pod,
{
    // ProofContextState<U> = u8 proof_type + Pubkey authority + U context.
    let space = std::mem::size_of::<U>() + 33;
    let rent = rpc
        .get_minimum_balance_for_rent_exemption(space)
        .await
        .context("get rent for context-state account")?;

    let create_ix = system_instruction::create_account(
        &payer.pubkey(),
        &ctx_account_kp.pubkey(),
        rent,
        space as u64,
        &solana_zk_elgamal_proof_interface::id(),
    );
    let verify_ix = verify_proof_ix.encode_verify_proof(
        Some(ContextStateInfo {
            context_state_account: &ctx_account_kp.pubkey(),
            context_state_authority: ctx_authority,
        }),
        proof_data,
    );

    send_ixs(rpc, payer, &[ctx_account_kp], &[create_ix, verify_ix]).await
}

/// Create a context-state account by referencing proof bytes already stored
/// in an `spl_record` account.
pub async fn create_context_state_account_from_record<U>(
    rpc: &RpcClient,
    payer: &Keypair,
    ctx_account_kp: &Keypair,
    ctx_authority: &Pubkey,
    record_account: &Pubkey,
    record_offset: u32,
    verify_proof_ix: ProofInstruction,
) -> Result<Signature>
where
    U: bytemuck::Pod,
{
    let space = std::mem::size_of::<U>() + 33;
    let rent = rpc
        .get_minimum_balance_for_rent_exemption(space)
        .await
        .context("get rent for context-state account")?;

    let create_ix = system_instruction::create_account(
        &payer.pubkey(),
        &ctx_account_kp.pubkey(),
        rent,
        space as u64,
        &solana_zk_elgamal_proof_interface::id(),
    );
    let verify_ix = verify_proof_ix.encode_verify_proof_from_account(
        Some(ContextStateInfo {
            context_state_account: &ctx_account_kp.pubkey(),
            context_state_authority: ctx_authority,
        }),
        record_account,
        record_offset,
    );

    send_ixs(rpc, payer, &[ctx_account_kp], &[create_ix, verify_ix]).await
}

/// Create + initialize an `spl_record` account and chunk-write `proof_bytes`.
/// Returns labelled signatures for every issued tx.
pub async fn create_record_account(
    rpc: &RpcClient,
    payer: &Keypair,
    record_kp: &Keypair,
    authority: &Keypair,
    proof_bytes: &[u8],
) -> Result<Vec<(String, Signature)>> {
    let space = 33 + proof_bytes.len();
    let rent = rpc
        .get_minimum_balance_for_rent_exemption(space)
        .await
        .context("get rent for record account")?;

    let create_ix = system_instruction::create_account(
        &payer.pubkey(),
        &record_kp.pubkey(),
        rent,
        space as u64,
        &spl_record::id(),
    );
    let init_ix = spl_record::instruction::initialize(&record_kp.pubkey(), &authority.pubkey());

    let mut results = Vec::new();
    let sig = send_ixs(rpc, payer, &[record_kp], &[create_ix, init_ix])
        .await
        .context("record: create + initialize")?;
    results.push(("record-create-init".to_string(), sig));

    let mut offset: usize = 0;
    let mut chunk_idx = 0;
    while offset < proof_bytes.len() {
        let end = (offset + RECORD_WRITE_CHUNK).min(proof_bytes.len());
        let chunk = &proof_bytes[offset..end];
        let write_ix = spl_record::instruction::write(
            &record_kp.pubkey(),
            &authority.pubkey(),
            offset as u64,
            chunk,
        );
        let sig = send_ixs(rpc, payer, &[authority], &[write_ix])
            .await
            .with_context(|| format!("record: write chunk {chunk_idx} at offset {offset}"))?;
        results.push((format!("record-write-{chunk_idx}"), sig));
        debug!(
            "spl_record: wrote {} bytes at offset {} (chunk {chunk_idx})",
            chunk.len(),
            offset
        );
        offset = end;
        chunk_idx += 1;
    }

    Ok(results)
}

/// Close a proof context-state account, returning lamports to `destination`.
pub async fn close_context_state_account(
    rpc: &RpcClient,
    payer: &Keypair,
    authority: &Keypair,
    ctx_account: &Pubkey,
    destination: &Pubkey,
) -> Result<Signature> {
    let ix = solana_zk_elgamal_proof_interface::instruction::close_context_state(
        ContextStateInfo {
            context_state_account: ctx_account,
            context_state_authority: &authority.pubkey(),
        },
        destination,
    );
    send_ixs(rpc, payer, &[authority], &[ix]).await
}

/// Close an `spl_record` account, returning lamports to `destination`.
pub async fn close_record_account(
    rpc: &RpcClient,
    payer: &Keypair,
    authority: &Keypair,
    record_account: &Pubkey,
    destination: &Pubkey,
) -> Result<Signature> {
    let ix =
        spl_record::instruction::close_account(record_account, &authority.pubkey(), destination);
    send_ixs(rpc, payer, &[authority], &[ix]).await
}

#[allow(unused_imports)]
pub use solana_zk_elgamal_proof_interface::proof_data::{
    BatchedGroupedCiphertext3HandlesValidityProofData, BatchedRangeProofU128Data,
    BatchedRangeProofU64Data, CiphertextCommitmentEqualityProofData,
};
