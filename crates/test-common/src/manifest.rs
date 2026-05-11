use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

use serde::Deserialize;
use solana_pubkey::Pubkey;

pub type SimdId = String;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub simds: HashMap<SimdId, FeatureConfig>,
    #[serde(default)]
    pub e2e_checks: HashMap<String, E2eCheckConfig>,
}

impl Manifest {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let manifest: Manifest = serde_yaml::from_str(&contents)?;
        Ok(manifest)
    }

    pub fn get(&self, id: &str) -> Option<&FeatureConfig> {
        self.simds.get(id)
    }

    pub fn iter_simds(&self) -> impl Iterator<Item = (&SimdId, &FeatureConfig)> {
        self.simds.iter()
    }

    pub fn iter_e2e_checks(&self) -> impl Iterator<Item = (&String, &E2eCheckConfig)> {
        self.e2e_checks.iter()
    }

    /// Resolve a feature-gate label that is either a SIMD id (e.g. `simd_0153`)
    /// or a raw base58 pubkey to a `Pubkey`.
    pub fn resolve_feature_gate(&self, key: &str) -> Option<Pubkey> {
        if let Some(config) = self.simds.get(key) {
            return Some(config.feature_activation.address);
        }
        Pubkey::from_str(key).ok()
    }
}

fn deserialize_pubkey<'de, D>(deserializer: D) -> Result<Pubkey, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Pubkey::from_str(&s).map_err(serde::de::Error::custom)
}

fn deserialize_option_pubkey<'de, D>(deserializer: D) -> Result<Option<Pubkey>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(deserializer)?;
    s.map(|s| Pubkey::from_str(&s).map_err(serde::de::Error::custom))
        .transpose()
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeatureConfig {
    pub description: String,
    #[serde(default)]
    pub url: Option<String>,
    pub number: u32,
    pub feature_activation: FeatureActivationConfig,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub deprecated_by: Option<String>,
    #[serde(default)]
    pub test: Option<TestConfig>,
}

impl FeatureConfig {
    pub fn test_deployment_for(&self, network: &str) -> Option<&NetworkDeploymentConfig> {
        self.test
            .as_ref()
            .and_then(|test| test.deployment_for(network))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeatureActivationConfig {
    #[serde(deserialize_with = "deserialize_pubkey")]
    pub address: Pubkey,
    #[serde(default)]
    pub devnet: Option<NetworkActivationConfig>,
    #[serde(default)]
    pub testnet: Option<NetworkActivationConfig>,
    #[serde(default)]
    pub mainnet: Option<NetworkActivationConfig>,
}

impl FeatureActivationConfig {
    /// Returns whether the feature is expected to be activated on the given network,
    /// based on the presence of an `epoch` value in the manifest.
    pub fn is_activated_on(&self, network: &str) -> bool {
        let config = match network {
            "devnet" => self.devnet.as_ref(),
            "testnet" => self.testnet.as_ref(),
            "mainnet" => self.mainnet.as_ref(),
            _ => None,
        };
        config.and_then(|c| c.epoch).is_some()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct NetworkActivationConfig {
    pub epoch: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TestConfig {
    pub location: String,
    #[serde(default)]
    pub devnet: Option<NetworkDeploymentConfig>,
    #[serde(default)]
    pub testnet: Option<NetworkDeploymentConfig>,
    #[serde(default)]
    pub mainnet: Option<NetworkDeploymentConfig>,
}

impl TestConfig {
    pub fn deployment_for(&self, network: &str) -> Option<&NetworkDeploymentConfig> {
        match network {
            "devnet" => self.devnet.as_ref(),
            "testnet" => self.testnet.as_ref(),
            "mainnet" => self.mainnet.as_ref(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct NetworkDeploymentConfig {
    #[serde(deserialize_with = "deserialize_option_pubkey", default)]
    pub address: Option<Pubkey>,
    #[serde(deserialize_with = "deserialize_option_pubkey", default)]
    pub authority: Option<Pubkey>,
}

// --------------------------------------------------------------------------
// E2E check types
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct E2eCheckConfig {
    pub description: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub requires: E2eCheckRequirements,
    /// Logical test id, registered in `crates/tests/src/lib.rs::all_e2e_tests()`.
    pub test: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct E2eCheckRequirements {
    /// Feature gates that must be active. Each entry is either a SIMD id
    /// (e.g. "simd_0153"), in which case the pubkey is resolved via the
    /// `simds` map, or a raw base58 pubkey.
    #[serde(default)]
    pub feature_gates: Vec<String>,

    /// On-chain programs that must be deployed (and optionally pinned to a
    /// specific binary hash).
    #[serde(default)]
    pub programs: Vec<ProgramDeploymentRequirement>,
}

impl E2eCheckRequirements {
    /// Resolve every feature-gate label to a `(label, Pubkey)` pair.
    /// Errors if a label is neither a known SIMD id nor a parseable pubkey.
    pub fn resolved_feature_gates(
        &self,
        manifest: &Manifest,
    ) -> anyhow::Result<Vec<(String, Pubkey)>> {
        self.feature_gates
            .iter()
            .map(|label| {
                manifest
                    .resolve_feature_gate(label)
                    .map(|pk| (label.clone(), pk))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "feature gate '{}' is neither a known SIMD id nor a valid pubkey",
                            label
                        )
                    })
            })
            .collect()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProgramDeploymentRequirement {
    #[serde(deserialize_with = "deserialize_pubkey")]
    pub address: Pubkey,
    /// Hex hash from `solana-verify get-executable-hash` of the deployed ELF.
    /// `None` means "must exist and be executable" with no binary pinning.
    #[serde(default)]
    pub expected_hash: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_network_specific_test_deployment_address() {
        let manifest: Manifest = serde_yaml::from_str(
            r#"
simds:
  simd_0001:
    description: Example
    number: 1
    feature_activation:
      address: 11111111111111111111111111111111
    test:
      location: "crates/tests/src/simd_0001.rs"
      testnet:
        address: GQfx3D8zDArQVtaRXqiJiSVe8mG2dKNGeiKBWr9YKPS5
"#,
        )
        .unwrap();

        let config = manifest.get("simd_0001").unwrap();

        assert_eq!(
            config
                .test_deployment_for("testnet")
                .and_then(|deployment| deployment.address),
            Some(Pubkey::from_str_const(
                "GQfx3D8zDArQVtaRXqiJiSVe8mG2dKNGeiKBWr9YKPS5"
            ))
        );
        assert!(config.test_deployment_for("localnet").is_none());
    }

    #[test]
    fn parses_simds_only_document() {
        let m: Manifest = serde_yaml::from_str(
            r#"
simds:
  simd_0001:
    description: Example
    number: 1
    feature_activation:
      address: 11111111111111111111111111111111
"#,
        )
        .unwrap();
        assert_eq!(m.simds.len(), 1);
        assert!(m.e2e_checks.is_empty());
    }

    #[test]
    fn parses_e2e_checks_only_document() {
        let m: Manifest = serde_yaml::from_str(
            r#"
e2e_checks:
  example:
    description: Example
    test: e2e_example
    requires:
      feature_gates:
        - 11111111111111111111111111111111
"#,
        )
        .unwrap();
        assert!(m.simds.is_empty());
        assert_eq!(m.e2e_checks.len(), 1);
    }

    #[test]
    fn e2e_check_resolves_feature_gate_by_simd_id_and_raw_pubkey() {
        let m: Manifest = serde_yaml::from_str(
            r#"
simds:
  simd_0153:
    description: ZK ElGamal
    number: 153
    feature_activation:
      address: zkhiy5oLowR7HY4zogXjCjeMXyruLqBwSWH21qcFtnv
e2e_checks:
  example:
    description: Example
    test: e2e_example
    requires:
      feature_gates:
        - simd_0153
        - 11111111111111111111111111111111
"#,
        )
        .unwrap();
        let check = m.e2e_checks.get("example").unwrap();
        let resolved = check.requires.resolved_feature_gates(&m).unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].0, "simd_0153");
        assert_eq!(
            resolved[0].1,
            Pubkey::from_str_const("zkhiy5oLowR7HY4zogXjCjeMXyruLqBwSWH21qcFtnv")
        );
        assert_eq!(
            resolved[1].1,
            Pubkey::from_str_const("11111111111111111111111111111111")
        );
    }

    #[test]
    fn e2e_check_unknown_feature_gate_label_errors() {
        let m: Manifest = serde_yaml::from_str(
            r#"
e2e_checks:
  example:
    description: Example
    test: e2e_example
    requires:
      feature_gates:
        - not_a_simd_or_pubkey
"#,
        )
        .unwrap();
        let check = m.e2e_checks.get("example").unwrap();
        assert!(check.requires.resolved_feature_gates(&m).is_err());
    }
}
