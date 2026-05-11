use std::collections::HashMap;
use test_common::{E2eTest, SimdTest};

mod e2e;
mod simd_0153;
mod simd_0194;
mod simd_0266;

pub fn all_simd_tests() -> HashMap<String, Box<dyn SimdTest>> {
    let mut map = HashMap::new();
    map.insert("simd_0153".to_string(), simd_0153::register());
    map.insert("simd_0194".to_string(), simd_0194::register());
    map.insert("simd_0266".to_string(), simd_0266::register());
    map
}

pub fn all_e2e_tests() -> HashMap<String, Box<dyn E2eTest>> {
    let mut map = HashMap::new();
    map.insert(
        "e2e_confidential_transfers".to_string(),
        e2e::confidential_transfers::register(),
    );
    map
}
