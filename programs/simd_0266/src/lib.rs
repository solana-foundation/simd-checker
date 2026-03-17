use pinocchio::{
    cpi::invoke,
    entrypoint,
    error::ProgramError,
    instruction::{InstructionAccount, InstructionView},
    AccountView, Address, ProgramResult,
};
use program_common::FeatureGateStatus;
use solana_program_log::log;

entrypoint!(process_instruction);

pub fn process_instruction(
    _program_id: &Address,
    accounts: &[AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    // Accounts: [feature_gate, token_program, source, dest, authority]
    // Instruction data: [expect_activated(1 byte), amount(8 bytes LE)]
    if accounts.len() < 5 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    if instruction_data.len() < 9 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let expect_activated = instruction_data[0] != 0;
    let feature = &accounts[0];

    let status = FeatureGateStatus::from_account_view(feature)?;

    status.log_status("0266");
    status.assert_expected_activation(expect_activated)?;

    let token_program = &accounts[1];
    let source = &accounts[2];
    let dest = &accounts[3];
    let authority = &accounts[4];

    // Build batch instruction data containing 2 transfers of `amount` each.
    // Batch format: [255, {num_accounts, data_len, transfer_discriminator, amount_bytes}...]
    let mut batch_data = [0u8; 23]; // 1 + 2 * (2 + 1 + 8)
    batch_data[0] = 255; // Batch discriminator

    // Transfer 1
    batch_data[1] = 3; // num_accounts (source, dest, authority)
    batch_data[2] = 9; // data_len (1 discriminator + 8 amount)
    batch_data[3] = 3; // Transfer discriminator
    batch_data[4..12].copy_from_slice(&instruction_data[1..9]);

    // Transfer 2
    batch_data[12] = 3;
    batch_data[13] = 9;
    batch_data[14] = 3;
    batch_data[15..23].copy_from_slice(&instruction_data[1..9]);

    let cpi_accounts = [
        InstructionAccount::writable(source.address()),
        InstructionAccount::writable(dest.address()),
        InstructionAccount::readonly_signer(authority.address()),
        InstructionAccount::writable(source.address()),
        InstructionAccount::writable(dest.address()),
        InstructionAccount::readonly_signer(authority.address()),
    ];

    let instruction = InstructionView {
        program_id: token_program.address(),
        data: &batch_data,
        accounts: &cpi_accounts,
    };

    invoke::<6>(
        &instruction,
        &[source, dest, authority, source, dest, authority],
    )?;

    log!("SIMD-0266: Batch transfer completed");
    Ok(())
}
