use std::collections::HashMap;
use test_common::SimdTest;

mod simd_0194;
mod simd_0266;

pub fn all_tests() -> HashMap<String, Box<dyn SimdTest>> {
    let mut map = HashMap::new();
    map.insert("simd_0194".to_string(), simd_0194::register());
    map.insert("simd_0266".to_string(), simd_0266::register());
    map
}
