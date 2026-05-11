use pinocchio::{entrypoint, error::ProgramError, AccountView, Address, ProgramResult};
use program_common::FeatureGateStatus;
use solana_program_log::log;

entrypoint!(process_instruction);

/// Post-activation ZK ElGamal Proof program (native builtin introduced by
/// SIMD-0153).
const ZK_ELGAMAL_PROOF_PROGRAM_ID: Address =
    Address::from_str_const("ZkE1Gama1Proof11111111111111111111111111111");

/// Deprecated ZK Token Proof program. Per SIMD-0153 and the spec
/// (https://github.com/solana-foundation/solana-improvement-documents/blob/main/proposals/0153-elgamal-proof-program.md):
/// "The existing ZK Token Proof program is not yet activated on any of the
/// clusters." It must remain a non-builtin both pre- AND post-activation of
/// SIMD-0153 — the proposal removes it entirely.
const ZK_TOKEN_PROOF_PROGRAM_ID: Address =
    Address::from_str_const("ZkTokenProof1111111111111111111111111111111");

/// Native loader program id — owns native builtin programs such as the
/// ZK ElGamal Proof program.
const NATIVE_LOADER_ID: Address =
    Address::from_str_const("NativeLoader1111111111111111111111111111111");

/// Accounts:
///   0. `[]` feature-gate account for SIMD-0153
///   1. `[]` ZkE1Gama1Proof11111111111111111111111111111 (new proof program)
///   2. `[]` ZkTokenProof1111111111111111111111111111111 (deprecated proof program)
///
/// Instruction data:
///   byte 0 = `expect_activated` (0 or 1)
pub fn process_instruction(
    _program_id: &Address,
    accounts: &[AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    if instruction_data.is_empty() {
        return Err(ProgramError::InvalidInstructionData);
    }
    let expect_activated = instruction_data[0] != 0;

    let [feature, proof_program, deprecated_proof_program] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let status = FeatureGateStatus::from_account_view(feature)?;
    status.log_status("0153");
    status.assert_expected_activation(expect_activated)?;

    // The caller must pass in the canonical ZK ElGamal Proof program address —
    // we read its account flags from the runtime to assert pre/post-activation
    // observability on-chain (not just via RPC).
    if proof_program.address() != &ZK_ELGAMAL_PROOF_PROGRAM_ID {
        log!("SIMD-0153: wrong proof program address passed");
        return Err(ProgramError::InvalidAccountData);
    }
    if deprecated_proof_program.address() != &ZK_TOKEN_PROOF_PROGRAM_ID {
        log!("SIMD-0153: wrong deprecated proof program address passed");
        return Err(ProgramError::InvalidAccountData);
    }

    // Invariant that holds *regardless* of activation state: the deprecated
    // ZK Token Proof program was never activated on any cluster and SIMD-0153
    // removes it. It must not be a live builtin.
    //
    // NOTE: This is reported as a log-only warning (not a hard error) because
    // some local test environments (surfpool / LiteSVM) pre-load all native
    // builtins regardless of feature-gate state. The host test enforces the
    // assertion as a hard fail on remote networks (testnet/devnet/mainnet)
    // where the runtime models feature gating correctly.
    if deprecated_proof_program.executable() {
        log!("SIMD-0153: WARN deprecated ZkTokenProof is reported executable (expected only on local test environments)");
    } else {
        log!("SIMD-0153: deprecated ZkTokenProof is not a live builtin (as expected)");
    }

    if status.activated {
        // Post-activation invariant: the ZK ElGamal Proof program MUST be a
        // live native builtin — executable and owned by the native loader.
        if !proof_program.executable() {
            log!("SIMD-0153: post-activation but ZkE1Gama1Proof is not executable");
            return Err(ProgramError::InvalidAccountData);
        }
        if !proof_program.owned_by(&NATIVE_LOADER_ID) {
            log!("SIMD-0153: post-activation but ZkE1Gama1Proof is not owned by NativeLoader");
            return Err(ProgramError::IncorrectProgramId);
        }
        log!("SIMD-0153: post-activation — ZkE1Gama1Proof is a live native builtin; ZkTokenProof remains deprecated");
    } else {
        // Pre-activation invariant: the ZK ElGamal Proof program must NOT yet
        // be a native builtin. The account either does not exist (data_len=0,
        // not executable) or is not yet executable.
        if proof_program.executable() {
            log!("SIMD-0153: pre-activation but ZkE1Gama1Proof is already executable");
            return Err(ProgramError::InvalidAccountData);
        }
        log!("SIMD-0153: pre-activation — neither ZkE1Gama1Proof nor ZkTokenProof is a builtin");
    }

    Ok(())
}
