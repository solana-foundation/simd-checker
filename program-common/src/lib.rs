use solana_address::Address;
pub struct FeatureGateAccount {
    pub activated: bool,
    pub epoch: u64,
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
