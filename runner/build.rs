use std::fs;
use std::path::Path;

fn main() {
    let tests_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let dest = Path::new(&out_dir).join("test_registry.rs");

    let mut crate_names = Vec::new();

    if let Ok(entries) = fs::read_dir(&tests_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("Cargo.toml").exists() {
                let dir_name = path.file_name().unwrap().to_string_lossy().to_string();
                // Convert directory name (snake_case with hyphens) to a valid Rust identifier
                let crate_ident = dir_name.replace('-', "_");
                crate_names.push(crate_ident);
            }
        }
    }

    crate_names.sort();

    let mut code = String::new();
    code.push_str("pub fn all_tests() -> Vec<Box<dyn test_common::SimdTest>> {\n");
    code.push_str("    vec![\n");
    for name in &crate_names {
        code.push_str(&format!("        {}::register(),\n", name));
    }
    code.push_str("    ]\n");
    code.push_str("}\n");

    fs::write(&dest, code).unwrap();

    // Re-run if tests directory changes
    println!("cargo:rerun-if-changed=../tests");
}
