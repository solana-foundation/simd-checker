use pinocchio::{entrypoint, error::ProgramError, AccountView, Address, ProgramResult};
use program_common::FeatureGateStatus;
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

    let status = FeatureGateStatus::from_account_view(feature)?;

    status.log_status("0194");
    status.assert_expected_activation(expect_activated)?;

    let mut rent_bytes = [0u8; 24]; // Allocate a buffer to hold the Rent sysvar data
    let sysvar_id = solana_sysvar::rent::id(); // Get the ID of the Rent sysvar
    let offset = 0; // Offset to read from the sysvar data
    get_sysvar(&mut rent_bytes[..17], &sysvar_id, offset, 17)?;

    let rent: Rent = unsafe { core::mem::transmute(rent_bytes) };
    let exemption = rent.exemption_threshold.to_string();
    log!("Lamports Per Byte: {}", rent.lamports_per_byte);
    log!("Exemption Threshold: {}", exemption.as_str());
    log!("Burn Percent: {}", rent.burn_percent);
    if status.activated {
        if rent.exemption_threshold != 1.0 {
            log!("Feature is activated but exemption threshold is not set to 1.0");
            return Err(ProgramError::InvalidAccountData);
        }
    } else {
        if rent.exemption_threshold != 2.0 {
            log!("Feature is not activated, but exemption threshold is not set to 2.0");
            return Err(ProgramError::InvalidAccountData);
        }
    }
    Ok(())
}
