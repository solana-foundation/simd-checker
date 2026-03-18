# simd-checker

Verify SIMD feature activations on Solana networks.

## Usage

```
# Build programs and run all tests on localnet
just run

# Run a specific test
just run --filter 0194

# Run on devnet/testnet/mainnet
just run --network testnet

# Run with debug logging
just debug

# Output results as JSON to stdout
just run --output json

# Write results as YAML to a file
just run --output yaml --output-file results.yaml
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

## Status

| SIMD | Description                                   | Tested                                                                            |
| ---- | --------------------------------------------- | --------------------------------------------------------------------------------- |
| 0033 | Timely Vote Credits                           | -                                                                                 |
| 0047 | Syscall and Sysvar for last restart slot      | -                                                                                 |
| 0049 | Syscall for remaining compute units           | -                                                                                 |
| 0075 | Precompile for verifying secp256r1 sig.       | -                                                                                 |
| 0079 | Allow Commission Decrease at Any Time         | -                                                                                 |
| 0083 | Relax Entry Constraints                       | -                                                                                 |
| 0084 | Disable rent fees collection                  | -                                                                                 |
| 0085 | Additional Fee-Collector Constraints          | -                                                                                 |
| 0089 | Programify Feature Gate Program               | -                                                                                 |
| 0093 | Disable Bpf loader V2 program deployment      | -                                                                                 |
| 0096 | Reward full priority fee to validator         | -                                                                                 |
| 0105 | Maintain Dynamic Set of Reserved Account Keys | -                                                                                 |
| 0118 | Partitioned Epoch Rewards Distribution        | -                                                                                 |
| 0127 | Get-Sysvar Syscall                            | -                                                                                 |
| 0128 | Migrate Address Lookup Table to Core BPF      | -                                                                                 |
| 0129 | Alt_BN128 Syscalls - Simplified Error Code    | -                                                                                 |
| 0133 | Syscall Get-Epoch-Stake                       | -                                                                                 |
| 0137 | EC Syscalls - Abort on Unsupported Curve/Ops  | -                                                                                 |
| 0138 | Deprecate legacy vote instructions            | -                                                                                 |
| 0140 | Migrate Config to Core BPF                    | -                                                                                 |
| 0148 | MoveStake and MoveLamports Instructions       | -                                                                                 |
| 0152 | Precompiles                                   | -                                                                                 |
| 0153 | ZK ElGamal Proof Program                      | -                                                                                 |
| 0159 | Relax Precompile Failure Constraint           | -                                                                                 |
| 0162 | Remove Accounts `is_executable` Flag Checks   | -                                                                                 |
| 0163 | Lift the CPI caller restriction               | -                                                                                 |
| 0166 | SBPF Dynamic stack frames                     | -                                                                                 |
| 0170 | Reserve minimal CUs for builtins              | -                                                                                 |
| 0173 | SBPF instruction encoding improvements        | -                                                                                 |
| 0174 | SBPF arithmetics improvements                 | -                                                                                 |
| 0175 | Disable Partitioned Rent Updates              | -                                                                                 |
| 0178 | SBPF Static Syscalls                          | -                                                                                 |
| 0182 | Consume requested CUs for sBPF failures       | -                                                                                 |
| 0183 | Skip Rent Rewrites                            | -                                                                                 |
| 0185 | Vote Account v4                               | -                                                                                 |
| 0189 | SBPF stricter ELF headers                     | -                                                                                 |
| 0194 | Deprecate Rent Exemption Threshold            | yes [Needs LiteSVM fix for Surfpool](https://github.com/LiteSVM/litesvm/pull/307) |
| 0196 | Migrate Stake to Core BPF                     | -                                                                                 |
| 0207 | Raise Block Limits to 50M CUs                 | -                                                                                 |
| 0215 | Homomorphic Hashing of Account State          | -                                                                                 |
| 0219 | Stricter ABI and Runtime Constraints          | -                                                                                 |
| 0220 | Snapshots use Accounts Lattice Hash           | -                                                                                 |
| 0222 | Fix alt_bn128_pairing syscall length          | -                                                                                 |
| 0223 | Removes Accounts Delta Hash                   | -                                                                                 |
| 0242 | Static Nonce Account Only                     | -                                                                                 |
| 0256 | Increase Block Limits to 60M CUs              | -                                                                                 |
| 0266 | p-token: Efficient Token program              | yes [Needs LiteSVM fix for Surfpool](https://github.com/LiteSVM/litesvm/pull/310) |
| 0267 | Sets rent_epoch to a constant in the VM       | -                                                                                 |
| 0268 | Raise CPI Nesting Limit                       | -                                                                                 |
| 0321 | VM Register 2 Instruction Data Pointer        | -                                                                                 |
| 0334 | Fix alt_bn128_pairing syscall length check    | -                                                                                 |
| 0384 | Alpenglow migration                           | -                                                                                 |
| 0387 | BLS Pubkey management in vote account         | -                                                                                 |
| 0406 | Maximum instruction accounts                  | -                                                                                 |
| 0444 | Relax program data account check in migration | -                                                                                 |
