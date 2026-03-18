# simd-checker

Verify SIMD feature activations on Solana networks.

## Usage

```
# Build programs and run all tests on localnet
just run --network localnet

# Run a specific test
just run --network localnet --filter 0194

# Output results as JSON to stdout
just run --network localnet --output json

# Write results as YAML to a file
just run --network localnet --output yaml --output-file results.yaml
```

### CLI flags

| Flag            | Default                    | Description                                                           |
| --------------- | -------------------------- | --------------------------------------------------------------------- |
| `--network`     | `localnet`                 | Target network: `localnet`, `testnet`, `mainnet`, or a custom RPC URL |
| `--filter`      |                            | Filter tests by name or SIMD number                                   |
| `--keypair`     | `~/.config/solana/id.json` | Path to keypair file (required for testnet/mainnet)                   |
| `--manifest`    | `manifest.yaml`            | Path to the manifest YAML file                                        |
| `--output`      | `text`                     | Output format: `text`, `json`, or `yaml`                              |
| `--output-file` |                            | Write json/yaml output to a file instead of stdout                    |

## Adding a new test

1. **Create the on-chain program** under `programs/simd_XXXX/` with its own `Cargo.toml` (`cdylib` crate type) and entrypoint logic in `src/lib.rs`.

2. **Add the host-side test** at `crates/tests/src/simd_XXXX.rs`. Implement the `SimdTest` trait:

   ```rust
   use anyhow::Result;
   use async_trait::async_trait;
   use test_common::{RpcContext, SimdTest, TestOutcome};

   pub struct SimdXXXXTest;

   #[async_trait]
   impl SimdTest for SimdXXXXTest {
       fn program(&self) -> Option<test_common::ProgramDeployment> {
           Some(test_common::ProgramDeployment {
               keypair_path: "programs/simd_XXXX/program-keypair.json".to_string(),
               so_path: "target/deploy/simd_XXXX.so".to_string(),
           })
       }

       async fn run_rpc(&self, ctx: RpcContext) -> Result<TestOutcome> {
           // Build and send your transaction, return Pass or Fail
           todo!()
       }
   }

   pub fn register() -> Box<dyn SimdTest> {
       Box::new(SimdXXXXTest)
   }
   ```

3. **Register the module** in `crates/tests/src/lib.rs`:

   ```rust
   mod simd_XXXX;
   ```

   And add one line in [`all_tests()`](crates/tests/src/lib.rs):

   ```rust
   map.insert("simd_XXXX".to_string(), simd_XXXX::register());
   ```

4. **Add a manifest entry** in `manifest.yaml`:

   ```yaml
   simd_XXXX:
     description: Your feature description
     number: XXXX
     feature_activation:
       address: <feature-gate-pubkey>
     depends_on: []
     test:
       location: "crates/tests/src/simd_XXXX.rs"
   ```
