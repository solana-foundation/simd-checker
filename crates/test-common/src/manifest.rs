use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use solana_pubkey::Pubkey;

pub type SimdId = String;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest(HashMap<SimdId, FeatureConfig>);

impl Manifest {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let manifest: Manifest = serde_yaml::from_str(&contents)?;
        Ok(manifest)
    }

    pub fn get(&self, id: &str) -> Option<&FeatureConfig> {
        self.0.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&SimdId, &FeatureConfig)> {
        self.0.iter()
    }
}

fn deserialize_pubkey<'de, D>(deserializer: D) -> Result<Pubkey, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Ok(Pubkey::from_str_const(&s))
}

fn deserialize_option_pubkey<'de, D>(deserializer: D) -> Result<Option<Pubkey>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(deserializer)?;
    Ok(s.map(|s| Pubkey::from_str_const(&s)))
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

#[derive(Debug, Clone, Deserialize)]
pub struct NetworkDeploymentConfig {
    #[serde(deserialize_with = "deserialize_option_pubkey", default)]
    pub address: Option<Pubkey>,
    #[serde(deserialize_with = "deserialize_option_pubkey", default)]
    pub authority: Option<Pubkey>,
}
