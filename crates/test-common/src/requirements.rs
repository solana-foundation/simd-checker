use anyhow::{Context, Result};
use log::debug;
use serde::Serialize;
use sha2::{Digest, Sha256};
use solana_client::rpc_client::RpcClient;
use solana_pubkey::Pubkey;

use crate::manifest::{E2eCheckRequirements, Manifest};

/// `BPFLoaderUpgradeab1e11111111111111111111111`
const BPF_LOADER_UPGRADEABLE_ID: Pubkey =
    Pubkey::from_str_const("BPFLoaderUpgradeab1e11111111111111111111111");

/// Size of the `UpgradeableLoaderState::ProgramData` header that precedes the
/// ELF bytes inside a programdata account.
///
/// Layout (bincode):
///   4 (variant tag) + 8 (slot) + 1 (Option tag) + 32 (Pubkey) = 45
const PROGRAMDATA_HEADER_LEN: usize = 45;

/// Size of `UpgradeableLoaderState::Program { programdata_address }`:
///   4 (variant tag) + 32 (Pubkey) = 36
const PROGRAM_ACCOUNT_DATA_LEN: usize = 36;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum UnmetRequirement {
    FeatureGate {
        label: String,
        address: Pubkey,
    },
    ProgramMissing {
        address: Pubkey,
        description: Option<String>,
        reason: String,
    },
    ProgramHashMismatch {
        address: Pubkey,
        expected: String,
        actual: String,
        description: Option<String>,
    },
}

impl std::fmt::Display for UnmetRequirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnmetRequirement::FeatureGate { label, address } => {
                write!(f, "feature gate inactive: {label} ({address})")
            }
            UnmetRequirement::ProgramMissing {
                address,
                description,
                reason,
            } => {
                let desc = description
                    .as_deref()
                    .map(|d| format!(" [{d}]"))
                    .unwrap_or_default();
                write!(f, "program missing: {address}{desc} - {reason}")
            }
            UnmetRequirement::ProgramHashMismatch {
                address,
                expected,
                actual,
                description,
            } => {
                let desc = description
                    .as_deref()
                    .map(|d| format!(" [{d}]"))
                    .unwrap_or_default();
                write!(
                    f,
                    "program hash mismatch: {address}{desc} expected {expected}, got {actual}"
                )
            }
        }
    }
}

pub struct RequirementChecker<'a> {
    pub rpc: &'a RpcClient,
}

impl<'a> RequirementChecker<'a> {
    pub fn new(rpc: &'a RpcClient) -> Self {
        Self { rpc }
    }

    /// Walk the requirements, return every unmet one. Does not short-circuit.
    pub fn check(
        &self,
        req: &E2eCheckRequirements,
        manifest: &Manifest,
    ) -> Result<Vec<UnmetRequirement>> {
        let mut unmet = Vec::new();

        // Feature gates
        for (label, address) in req.resolved_feature_gates(manifest)? {
            match self.is_feature_active(&address) {
                Ok(true) => {}
                Ok(false) => unmet.push(UnmetRequirement::FeatureGate { label, address }),
                Err(e) => {
                    debug!("error checking feature gate {address}: {e}");
                    unmet.push(UnmetRequirement::FeatureGate { label, address });
                }
            }
        }

        // Programs
        for prog in &req.programs {
            match self.fetch_program_elf_hash(&prog.address) {
                Ok(Some(actual_hash)) => {
                    if let Some(expected) = &prog.expected_hash {
                        if !expected.eq_ignore_ascii_case(&actual_hash) {
                            unmet.push(UnmetRequirement::ProgramHashMismatch {
                                address: prog.address,
                                expected: expected.clone(),
                                actual: actual_hash,
                                description: prog.description.clone(),
                            });
                        }
                    }
                }
                Ok(None) => unmet.push(UnmetRequirement::ProgramMissing {
                    address: prog.address,
                    description: prog.description.clone(),
                    reason: "program account not found or not an upgradeable program".to_string(),
                }),
                Err(e) => unmet.push(UnmetRequirement::ProgramMissing {
                    address: prog.address,
                    description: prog.description.clone(),
                    reason: format!("rpc error: {e}"),
                }),
            }
        }

        Ok(unmet)
    }

    /// True iff the feature gate account exists and looks activated.
    /// Mirrors `SimdTest::detect_feature_activated`.
    fn is_feature_active(&self, addr: &Pubkey) -> Result<bool> {
        match self.rpc.get_account(addr) {
            Ok(account) => Ok(account.data.len() >= 9 && account.data[0] != 0),
            Err(_) => Ok(false),
        }
    }

    /// Replicates `solana-verify get-executable-hash` / `get-program-hash`.
    /// Returns `None` if the program is missing or not in a recognized loader.
    ///
    /// Reference implementation in `solana-foundation/solana-verifiable-build`:
    /// - `get_program_hash` — loader dispatch + programdata fetch:
    ///   <https://github.com/solana-foundation/solana-verifiable-build/blob/master/src/main.rs>
    ///   (search for `pub fn get_program_hash`)
    /// - `get_binary_hash` — trims trailing zero padding then sha256:
    ///   <https://github.com/solana-foundation/solana-verifiable-build/blob/master/src/main.rs>
    ///   (search for `pub fn get_binary_hash`)
    ///
    /// The header offset (`PROGRAMDATA_HEADER_LEN = 45`) corresponds to
    /// `UpgradeableLoaderState::size_of_programdata_metadata()` in
    /// `solana-loader-v3-interface`.
    fn fetch_program_elf_hash(&self, program_id: &Pubkey) -> Result<Option<String>> {
        let program_account = match self.rpc.get_account(program_id) {
            Ok(a) => a,
            Err(_) => return Ok(None),
        };

        if !program_account.executable {
            return Ok(None);
        }

        if program_account.owner != BPF_LOADER_UPGRADEABLE_ID {
            // Non-upgradeable programs: hash the account data directly.
            return Ok(Some(hex_sha256(&program_account.data)));
        }

        if program_account.data.len() < PROGRAM_ACCOUNT_DATA_LEN {
            return Ok(None);
        }

        // UpgradeableLoaderState::Program { programdata_address: Pubkey }
        // bincode layout: 4-byte little-endian variant tag (= 2), then 32-byte pubkey.
        let mut programdata_bytes = [0u8; 32];
        programdata_bytes.copy_from_slice(&program_account.data[4..36]);
        let programdata_address = Pubkey::new_from_array(programdata_bytes);

        let programdata_account = self
            .rpc
            .get_account(&programdata_address)
            .with_context(|| format!("failed to fetch programdata {programdata_address}"))?;

        if programdata_account.data.len() < PROGRAMDATA_HEADER_LEN {
            return Ok(None);
        }

        let elf = &programdata_account.data[PROGRAMDATA_HEADER_LEN..];
        // Trim trailing zero padding (programdata is over-allocated for upgrades).
        let trimmed_end = elf
            .iter()
            .rposition(|b| *b != 0)
            .map(|i| i + 1)
            .unwrap_or(0);
        let elf = &elf[..trimmed_end];

        Ok(Some(hex_sha256(elf)))
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        s.push_str(&format!("{:02x}", b));
    }
    s
}
