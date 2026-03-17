use pinocchio::{error::ProgramError, AccountView, ProgramResult};
use solana_address::Address;
use solana_program_log::log;

pub struct FeatureGateStatus {
    pub activated: bool,
    pub epoch: u64,
    pub is_owned: bool,
    pub status: String,
}

impl FeatureGateStatus {
    pub fn from_account_view(feature: &AccountView) -> Result<Self, ProgramError> {
        let is_owned = feature.owned_by(&FEATURE_GATE_PROGRAM);
        let feature_bytes = feature.try_borrow()?.to_vec();
        let (activated, epoch) = if feature_bytes.len() < 9 {
            (false, 0)
        } else {
            (
                feature_bytes[0] != 0,
                u64::from_le_bytes(feature_bytes[1..9].try_into().unwrap()),
            )
        };

        let status = if activated {
            format!("Activated in slot {}", epoch)
        } else if is_owned {
            "Pending activation".to_string()
        } else {
            "Not Activated".to_string()
        };

        Ok(FeatureGateStatus {
            activated,
            epoch,
            is_owned,
            status,
        })
    }

    pub fn assert_expected_activation(&self, expect_activated: bool) -> ProgramResult {
        if self.activated && !expect_activated {
            return Err(ProgramError::Custom(
                TestFailure::ActivatedWhenNotExpected as u32,
            ));
        } else if !self.activated && expect_activated {
            return Err(ProgramError::Custom(
                TestFailure::NotActivatedWhenExpected as u32,
            ));
        }
        Ok(())
    }

    pub fn log_status(&self, simd_number: &str) {
        log!("SIMD-{} Feature gate", simd_number);
        log!("----------------------------");
        log!("Status: {}\n", self.status.as_str());
    }
}

pub const FEATURE_GATE_PROGRAM: Address =
    Address::from_str_const("Feature111111111111111111111111111111111111");

#[repr(u32)]
pub enum TestFailure {
    ActivatedWhenNotExpected = 1,
    NotActivatedWhenExpected = 2,
}

impl TryFrom<u32> for TestFailure {
    type Error = u32;
    fn try_from(code: u32) -> Result<Self, Self::Error> {
        match code {
            1 => Ok(TestFailure::ActivatedWhenNotExpected),
            2 => Ok(TestFailure::NotActivatedWhenExpected),
            _ => Err(code),
        }
    }
}
