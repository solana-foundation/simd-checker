// Bridge module that generates ZK proofs using `solana-zk-sdk` 5.0.1
// (aliased in Cargo.toml as `solana-zk-sdk-v5`) while the rest of the
// codebase still depends on `solana-zk-sdk` 4.0.0 (transitively, via
// `spl-token-2022 = 10.0.0`).
//
// Why:
//   The on-chain `ZkE1Gama1Proof…` builtin shipped with agave 4.0.0-rc.0
//   (testnet) is built against `solana-zk-sdk 5.0.1`. zk-sdk 5.0 introduced
//   a domain-separated Fiat–Shamir transcript constructor
//   (`Transcript::new_zk_elgamal_transcript(...)`) and moved proof-context
//   hashing inside the sigma proofs module — see the diff between:
//     - solana-zk-sdk-4.0.0/src/zk_elgamal_proof_program/proof_data/pubkey_validity.rs
//     - solana-zk-sdk-5.0.1/src/zk_elgamal_proof_program/proof_data/pubkey_validity.rs
//   Proofs generated with 4.x therefore fail 5.x verification with
//   `SigmaProofVerificationError::AlgebraicRelation` on testnet.
//
// How:
//   - We generate the proof using v5 (which matches the validator).
//   - The proof data structs are `#[repr(C)]` Pod with byte-identical
//     layouts across v4 and v5 (32-byte context + 64-byte proof).
//   - We bytemuck-cast the v5-produced bytes back into the v4 type so the
//     `spl-token-2022` instruction builders (which require v4 `Pod +
//     ZkProofData<U>` bounds) accept them. The instruction bytes that go
//     on the wire are identical to those that 5.0.1 would produce
//     natively.
//
// Scope today:
//   Only `PubkeyValidityProofData` is bridged — that's the proof type
//   required for `ConfigureAccount`, which is the first proof-bearing
//   call in the flow. Transfer / Withdraw require additional proof types
//   (CiphertextCommitmentEquality, BatchedGroupedCiphertext3HandlesValidity,
//   BatchedRangeProofU64/U128) plus state-aware proof construction (the
//   prover must know the current encrypted available balance), and depend
//   on `spl-token-confidential-transfer-proof-generation` which itself
//   pins zk-sdk 4.0.0. Bridging those will follow once the configure path
//   is verified green on testnet.

use anyhow::{Context, Result};
use bytemuck::Pod;
use solana_signer::Signer;

// v4 types (re-exported from spl-token-2022 via solana-zk-sdk-4.0.0).
use spl_token_2022::solana_zk_sdk::{
    encryption::pod::elgamal::PodElGamalCiphertext as PodElGamalCiphertextV4,
    zk_elgamal_proof_program::proof_data::{
        BatchedGroupedCiphertext3HandlesValidityProofData as BatchedGroupedCiphertext3HandlesValidityProofDataV4,
        BatchedRangeProofU128Data as BatchedRangeProofU128DataV4,
        BatchedRangeProofU64Data as BatchedRangeProofU64DataV4,
        CiphertextCommitmentEqualityProofData as CiphertextCommitmentEqualityProofDataV4,
        PubkeyValidityProofData as PubkeyValidityProofDataV4,
    },
};

// v5 types (aliased dep).
use curve25519_dalek::scalar::Scalar as ScalarV5;
use solana_zk_sdk_v5::{
    encryption::{
        auth_encryption::{AeCiphertext as AeCiphertextV5, AeKey as AeKeyV5},
        elgamal::{
            ElGamal as ElGamalV5, ElGamalCiphertext as ElGamalCiphertextV5,
            ElGamalKeypair as ElGamalKeypairV5, ElGamalPubkey as ElGamalPubkeyV5,
        },
        grouped_elgamal::GroupedElGamal as GroupedElGamalV5,
        pedersen::Pedersen as PedersenV5,
    },
    zk_elgamal_proof_program::proof_data::{
        BatchedGroupedCiphertext3HandlesValidityProofData as BatchedGroupedCiphertext3HandlesValidityProofDataV5,
        BatchedRangeProofU128Data as BatchedRangeProofU128DataV5,
        BatchedRangeProofU64Data as BatchedRangeProofU64DataV5,
        CiphertextCommitmentEqualityProofData as CiphertextCommitmentEqualityProofDataV5,
        PubkeyValidityProofData as PubkeyValidityProofDataV5, ZkProofData as ZkProofDataV5,
    },
};

/// Derive the v5 ElGamal keypair for a given signer + token account seed.
///
/// v4 and v5 implement `new_from_signer` with the byte-identical algorithm
/// (`signer.try_sign_message([b"ElGamalSecretKey", public_seed])` →
/// SHA-512 → curve25519 scalar reduction). So this produces the same
/// keypair an `spl-token-client` v4 call would derive, but routed through
/// the v5 types whose `PubkeyValidityProofData::new` uses the correct
/// 5.x transcript.
pub fn derive_elgamal_v5(signer: &dyn Signer, public_seed: &[u8]) -> Result<ElGamalKeypairV5> {
    ElGamalKeypairV5::new_from_signer(signer, public_seed)
        .map_err(|e| anyhow::anyhow!("ElGamalKeypair (v5) new_from_signer: {e}"))
}

/// Derive the v5 AES key for a given signer + token account seed. Used to
/// produce the `decryptable_zero_balance` ciphertext that `ConfigureAccount`
/// requires. v4 and v5 derive identically.
pub fn derive_ae_v5(signer: &dyn Signer, public_seed: &[u8]) -> Result<AeKeyV5> {
    AeKeyV5::new_from_signer(signer, public_seed)
        .map_err(|e| anyhow::anyhow!("AeKey (v5) new_from_signer: {e}"))
}

/// Build a `PubkeyValidityProofData` whose proof bytes were generated with
/// zk-sdk 5.0.1's transcript, but returned in the v4 type so it satisfies
/// the `Pod + ZkProofData<...>` bound of `spl_token_2022`'s instruction
/// builders.
///
/// Self-verifies under v5 before returning, so a malformed keypair fails
/// fast client-side rather than as an opaque `AlgebraicRelation` on-chain.
pub fn pubkey_validity_proof(keypair: &ElGamalKeypairV5) -> Result<PubkeyValidityProofDataV4> {
    use solana_zk_sdk_v5::zk_elgamal_proof_program::proof_data::ZkProofData as ZkProofDataV5;

    let v5_proof = PubkeyValidityProofDataV5::new(keypair)
        .map_err(|e| anyhow::anyhow!("PubkeyValidityProofData (v5) new: {e:?}"))?;

    // Self-verify under v5 (matches on-chain semantics).
    v5_proof
        .verify_proof()
        .map_err(|e| anyhow::anyhow!("PubkeyValidityProofData (v5) self-verify: {e:?}"))?;

    // Sanity-check layout: PubkeyValidityProofContext (32B PodElGamalPubkey)
    // + PodPubkeyValidityProof (64B) = 96 bytes in both versions.
    pod_cast(&v5_proof).context("Pod-cast v5 PubkeyValidityProofData → v4")
}

/// Pod-to-Pod reinterpret. Returns a copy of `src`'s bytes interpreted as
/// `Dst`. Both types must be `Pod` and have identical size; we assert both
/// at runtime since the Pod trait alone does not guarantee cross-crate
/// layout compatibility.
fn pod_cast<Src: Pod, Dst: Pod>(src: &Src) -> Result<Dst> {
    let src_bytes = bytemuck::bytes_of(src);
    if src_bytes.len() != std::mem::size_of::<Dst>() {
        anyhow::bail!(
            "pod_cast size mismatch: src={} bytes, dst={} bytes",
            src_bytes.len(),
            std::mem::size_of::<Dst>()
        );
    }
    // `pod_read_unaligned` copies bytes and reinterprets — no aliasing.
    Ok(bytemuck::pod_read_unaligned::<Dst>(src_bytes))
}

/// Encrypt the literal balance `0` under the supplied v5 AES key,
/// returning the 36-byte `DecryptableBalance` payload that
/// `ConfigureAccount` expects.
///
/// `DecryptableBalance` in spl-token-2022 v4 is a transparent newtype over
/// `PodAeCiphertext` (36 bytes: 16-byte nonce + 16-byte ciphertext + 4-byte
/// trailing tag depending on layout). We Pod-cast the v5 ciphertext bytes
/// into the v4 type the instruction builder expects.
pub fn encrypt_zero_balance_v4(
    ae: &AeKeyV5,
) -> Result<spl_token_2022::extension::confidential_transfer::DecryptableBalance> {
    // AeCiphertext is NOT Pod, so we go through its stable to_bytes()
    // encoding rather than a Pod-cast. The wire format (nonce || ct||tag)
    // is unchanged across v4/v5.
    let bytes = ae.encrypt(0_u64).to_bytes();
    let v4: spl_token_2022::extension::confidential_transfer::DecryptableBalance =
        bytemuck::pod_read_unaligned(&bytes);
    Ok(v4)
}

// ---------------------------------------------------------------------------
// Transfer / Withdraw proof generation
// ---------------------------------------------------------------------------
//
// These functions are direct ports of
// `spl_token_confidential_transfer_proof_generation::{transfer,withdraw}`
// (which pin to zk-sdk 4.x via `solana-zk-sdk` re-export) to zk-sdk 5.0.1.
// The arithmetic and protocol are identical — only the internal transcript
// construction inside `*::new` differs, and that's what we need: the v5
// transcript matches the on-chain validator's verifier on agave 4.x.
//
// Outputs are Pod-cast to v4 types so they can be fed directly to
// `spl-token-client`'s `confidential_transfer_create_context_state_account`
// (generic over `ZK: Pod + ZkProofData<U>`) without further conversion.

const TRANSFER_AMOUNT_LO_BITS: usize = 16;
const TRANSFER_AMOUNT_HI_BITS: usize = 32;
const REMAINING_BALANCE_BIT_LENGTH: usize = 64;
const RANGE_PROOF_PADDING_BIT_LENGTH: usize = 16;

/// V5 proof data for a confidential transfer, returned in v4 Pod-cast form
/// for direct consumption by spl-token-client APIs.
pub struct TransferProofDataV5 {
    pub equality_proof_data: CiphertextCommitmentEqualityProofDataV4,
    pub ciphertext_validity_proof_data: BatchedGroupedCiphertext3HandlesValidityProofDataV4,
    pub range_proof_data: BatchedRangeProofU128DataV4,
    /// Auditor decrypt handle's ciphertext for the low half of the transfer
    /// amount — used by the on-chain `Transfer` instruction.
    pub auditor_ciphertext_lo: PodElGamalCiphertextV4,
    /// Auditor decrypt handle's ciphertext for the high half.
    pub auditor_ciphertext_hi: PodElGamalCiphertextV4,
    #[allow(dead_code)]
    /// New decryptable available balance (post-transfer) under the sender's
    /// AES key — required by the on-chain `Transfer` instruction.
    pub new_decryptable_available_balance:
        spl_token_2022::extension::confidential_transfer::DecryptableBalance,
}

/// V5 proof data for a confidential withdraw, returned in v4 Pod-cast form.
pub struct WithdrawProofDataV5 {
    pub equality_proof_data: CiphertextCommitmentEqualityProofDataV4,
    pub range_proof_data: BatchedRangeProofU64DataV4,
    #[allow(dead_code)]
    /// New decryptable available balance (post-withdraw) under the AES key.
    pub new_decryptable_available_balance:
        spl_token_2022::extension::confidential_transfer::DecryptableBalance,
}

/// Decrypt the on-chain encrypted "decryptable available balance" (AES
/// ciphertext) to a `u64` plaintext using the supplied v5 AES key.
///
/// The wire format of `AeCiphertext` is identical between v4 and v5, so we
/// reinterpret the v4 bytes through v5's parser.
pub fn decrypt_decryptable_balance_v5(
    ae: &AeKeyV5,
    decryptable_balance_v4: &spl_token_2022::extension::confidential_transfer::DecryptableBalance,
) -> Result<u64> {
    let bytes_slice: &[u8] = bytemuck::bytes_of(decryptable_balance_v4);
    let mut bytes = [0u8; 36];
    bytes.copy_from_slice(bytes_slice);
    let ct = AeCiphertextV5::from_bytes(&bytes)
        .ok_or_else(|| anyhow::anyhow!("malformed AeCiphertext bytes"))?;
    ct.decrypt(ae)
        .ok_or_else(|| anyhow::anyhow!("AES decrypt failed"))
}

/// Reinterpret a v4 `PodElGamalCiphertext` (the on-chain available-balance
/// ciphertext) as a v5 `ElGamalCiphertext` for proof generation.
pub fn pod_v4_to_v5_ciphertext(pod: &PodElGamalCiphertextV4) -> Result<ElGamalCiphertextV5> {
    // Wire format: 64 bytes (32-byte commitment || 32-byte handle).
    let bytes_slice: &[u8] = bytemuck::bytes_of(pod);
    let mut bytes = [0u8; 64];
    bytes.copy_from_slice(bytes_slice);
    ElGamalCiphertextV5::from_bytes(&bytes)
        .ok_or_else(|| anyhow::anyhow!("decode v4→v5 ElGamalCiphertext bytes"))
}

/// Build a v5 `ElGamalPubkey` from a 32-byte payload (matching v4's
/// `PodElGamalPubkey` wire layout).
pub fn elgamal_pubkey_from_bytes_v5(bytes: &[u8; 32]) -> Result<ElGamalPubkeyV5> {
    use std::convert::TryFrom;
    ElGamalPubkeyV5::try_from(&bytes[..])
        .map_err(|e| anyhow::anyhow!("decode v5 ElGamalPubkey: {e:?}"))
}

/// Generate the three proofs required for a confidential transfer using
/// zk-sdk 5.0.1, then Pod-cast each to its v4 type.
///
/// Inputs:
/// - `current_available_balance` — the encrypted available balance pulled
///   from `ConfidentialTransferAccount::available_balance` (v4 wire format,
///   reinterpreted into v5).
/// - `current_balance_plaintext` — decrypted available balance under the
///   AES key (obtained via `decrypt_decryptable_balance_v5`).
/// - `transfer_amount`
/// - `source_elgamal` / `source_aes` / `destination_elgamal_pubkey` /
///   `auditor_elgamal_pubkey` — all v5 keys.
pub fn transfer_proof_data_v5(
    current_available_balance: &ElGamalCiphertextV5,
    current_balance_plaintext: u64,
    transfer_amount: u64,
    source_elgamal: &ElGamalKeypairV5,
    source_aes: &AeKeyV5,
    destination_elgamal_pubkey: &ElGamalPubkeyV5,
    auditor_elgamal_pubkey: &ElGamalPubkeyV5,
) -> Result<TransferProofDataV5> {
    let (transfer_amount_lo, transfer_amount_hi) =
        split_u64(transfer_amount, TRANSFER_AMOUNT_LO_BITS).context("split transfer amount")?;

    // Encrypt lo/hi as grouped 3-handle ciphertexts under (src, dst, auditor).
    let opening_lo = solana_zk_sdk_v5::encryption::pedersen::PedersenOpening::new_rand();
    let grouped_lo = GroupedElGamalV5::<3>::encrypt_with(
        [
            source_elgamal.pubkey(),
            destination_elgamal_pubkey,
            auditor_elgamal_pubkey,
        ],
        transfer_amount_lo,
        &opening_lo,
    );
    let opening_hi = solana_zk_sdk_v5::encryption::pedersen::PedersenOpening::new_rand();
    let grouped_hi = GroupedElGamalV5::<3>::encrypt_with(
        [
            source_elgamal.pubkey(),
            destination_elgamal_pubkey,
            auditor_elgamal_pubkey,
        ],
        transfer_amount_hi,
        &opening_hi,
    );

    // New plaintext balance and a fresh Pedersen commitment to it.
    let new_balance_plaintext = current_balance_plaintext
        .checked_sub(transfer_amount)
        .ok_or_else(|| anyhow::anyhow!("not enough confidential balance"))?;
    let (new_balance_commitment, new_balance_opening) = PedersenV5::new(new_balance_plaintext);

    // Homomorphically derive the post-transfer ciphertext at the source.
    let src_ct_lo = grouped_lo
        .to_elgamal_ciphertext(0)
        .map_err(|e| anyhow::anyhow!("extract src lo: {e:?}"))?;
    let src_ct_hi = grouped_hi
        .to_elgamal_ciphertext(0)
        .map_err(|e| anyhow::anyhow!("extract src hi: {e:?}"))?;
    let two_power_lo = ScalarV5::from(1u64 << TRANSFER_AMOUNT_LO_BITS);
    let combined_src_ct = src_ct_lo + src_ct_hi * two_power_lo;
    let new_available_balance_ct = current_available_balance - &combined_src_ct;

    // Equality proof.
    let equality_v5 = CiphertextCommitmentEqualityProofDataV5::new(
        source_elgamal,
        &new_available_balance_ct,
        &new_balance_commitment,
        &new_balance_opening,
        new_balance_plaintext,
    )
    .map_err(|e| anyhow::anyhow!("equality proof (v5): {e:?}"))?;
    equality_v5
        .verify_proof()
        .map_err(|e| anyhow::anyhow!("equality proof self-verify (v5): {e:?}"))?;

    // Ciphertext-validity proof.
    let validity_v5 = BatchedGroupedCiphertext3HandlesValidityProofDataV5::new(
        source_elgamal.pubkey(),
        destination_elgamal_pubkey,
        auditor_elgamal_pubkey,
        &grouped_lo,
        &grouped_hi,
        transfer_amount_lo,
        transfer_amount_hi,
        &opening_lo,
        &opening_hi,
    )
    .map_err(|e| anyhow::anyhow!("validity proof (v5): {e:?}"))?;
    validity_v5
        .verify_proof()
        .map_err(|e| anyhow::anyhow!("validity proof self-verify (v5): {e:?}"))?;

    // Range proof.
    let (padding_commitment, padding_opening) = PedersenV5::new(0_u64);
    let range_v5 = BatchedRangeProofU128DataV5::new(
        vec![
            &new_balance_commitment,
            &grouped_lo.commitment,
            &grouped_hi.commitment,
            &padding_commitment,
        ],
        vec![
            new_balance_plaintext,
            transfer_amount_lo,
            transfer_amount_hi,
            0,
        ],
        vec![
            REMAINING_BALANCE_BIT_LENGTH,
            TRANSFER_AMOUNT_LO_BITS,
            TRANSFER_AMOUNT_HI_BITS,
            RANGE_PROOF_PADDING_BIT_LENGTH,
        ],
        vec![
            &new_balance_opening,
            &opening_lo,
            &opening_hi,
            &padding_opening,
        ],
    )
    .map_err(|e| anyhow::anyhow!("range proof (v5): {e:?}"))?;
    range_v5
        .verify_proof()
        .map_err(|e| anyhow::anyhow!("range proof self-verify (v5): {e:?}"))?;

    // Extract auditor ciphertexts (index 2 = auditor handle).
    let auditor_lo_v5 = grouped_lo
        .to_elgamal_ciphertext(2)
        .map_err(|e| anyhow::anyhow!("extract auditor lo: {e:?}"))?;
    let auditor_hi_v5 = grouped_hi
        .to_elgamal_ciphertext(2)
        .map_err(|e| anyhow::anyhow!("extract auditor hi: {e:?}"))?;

    // New decryptable available balance under v5 AES (wire-compatible with v4).
    let new_decryptable_bytes = source_aes.encrypt(new_balance_plaintext).to_bytes();
    let new_decryptable_available_balance: spl_token_2022::extension::confidential_transfer::DecryptableBalance =
        bytemuck::pod_read_unaligned(&new_decryptable_bytes);

    Ok(TransferProofDataV5 {
        equality_proof_data: pod_cast(&equality_v5)?,
        ciphertext_validity_proof_data: pod_cast(&validity_v5)?,
        range_proof_data: pod_cast(&range_v5)?,
        auditor_ciphertext_lo: elgamal_ciphertext_v5_to_pod_v4(&auditor_lo_v5),
        auditor_ciphertext_hi: elgamal_ciphertext_v5_to_pod_v4(&auditor_hi_v5),
        new_decryptable_available_balance,
    })
}

/// Generate the two proofs required for a confidential withdraw using
/// zk-sdk 5.0.1, then Pod-cast each to its v4 type.
pub fn withdraw_proof_data_v5(
    current_available_balance: &ElGamalCiphertextV5,
    current_balance_plaintext: u64,
    withdraw_amount: u64,
    elgamal: &ElGamalKeypairV5,
    aes: &AeKeyV5,
) -> Result<WithdrawProofDataV5> {
    let new_balance_plaintext = current_balance_plaintext
        .checked_sub(withdraw_amount)
        .ok_or_else(|| anyhow::anyhow!("not enough confidential balance for withdraw"))?;

    let (remaining_commitment, remaining_opening) = PedersenV5::new(new_balance_plaintext);
    #[allow(deprecated)]
    let remaining_ct = current_available_balance - &ElGamalV5::encode(withdraw_amount);

    let equality_v5 = CiphertextCommitmentEqualityProofDataV5::new(
        elgamal,
        &remaining_ct,
        &remaining_commitment,
        &remaining_opening,
        new_balance_plaintext,
    )
    .map_err(|e| anyhow::anyhow!("withdraw equality proof (v5): {e:?}"))?;
    equality_v5
        .verify_proof()
        .map_err(|e| anyhow::anyhow!("withdraw equality self-verify (v5): {e:?}"))?;

    let range_v5 = BatchedRangeProofU64DataV5::new(
        vec![&remaining_commitment],
        vec![new_balance_plaintext],
        vec![REMAINING_BALANCE_BIT_LENGTH],
        vec![&remaining_opening],
    )
    .map_err(|e| anyhow::anyhow!("withdraw range proof (v5): {e:?}"))?;
    range_v5
        .verify_proof()
        .map_err(|e| anyhow::anyhow!("withdraw range self-verify (v5): {e:?}"))?;

    let new_decryptable_bytes = aes.encrypt(new_balance_plaintext).to_bytes();
    let new_decryptable_available_balance: spl_token_2022::extension::confidential_transfer::DecryptableBalance =
        bytemuck::pod_read_unaligned(&new_decryptable_bytes);

    Ok(WithdrawProofDataV5 {
        equality_proof_data: pod_cast(&equality_v5)?,
        range_proof_data: pod_cast(&range_v5)?,
        new_decryptable_available_balance,
    })
}

fn split_u64(amount: u64, lo_bits: usize) -> Result<(u64, u64)> {
    match lo_bits {
        0 => Ok((0, amount)),
        1..=63 => {
            let complement = (u64::BITS as usize) - lo_bits;
            let lo = (amount << complement) >> complement;
            let hi = amount >> lo_bits;
            Ok((lo, hi))
        }
        64 => Ok((amount, 0)),
        _ => anyhow::bail!("invalid lo bit length"),
    }
}

fn elgamal_ciphertext_v5_to_pod_v4(ct: &ElGamalCiphertextV5) -> PodElGamalCiphertextV4 {
    let bytes = ct.to_bytes();
    bytemuck::pod_read_unaligned::<PodElGamalCiphertextV4>(&bytes)
}
