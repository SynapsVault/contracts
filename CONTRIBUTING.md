# Contributing to SynapsVault Contracts

## Prerequisites

```bash
# Rust + wasm target
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add wasm32-unknown-unknown

# Soroban CLI
cargo install --locked soroban-cli
```

## Development workflow

```bash
# Run all tests
cargo test --workspace

# Format
cargo fmt

# Lint
cargo clippy --workspace -- -D warnings

# Build WASM (production)
cargo build --target wasm32-unknown-unknown --release --workspace
```

## Adding a new contract

1. `mkdir -p contracts/my-contract/src`
2. Add `Cargo.toml` (see `access-lease` as template)
3. Write `src/lib.rs`
4. Add `"contracts/my-contract"` to workspace `Cargo.toml`
5. Write tests in `src/test.rs`
6. Document all public functions in README

## Commit format

```
feat(access-lease): add batch revoke function
fix(subscription): handle expired renewal correctly  
test(vault-registry): add max-tag edge case
```
