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

## Documentation

Documentation must be kept in sync with the code. When you change any of the following, update the corresponding documentation in the same pull request:

- **Public functions** — update the rustdoc comments (`///`) on the function, including its parameters, return values, and any panics or errors it may raise.
- **Public structs and enums** — update the rustdoc comments describing the type and each of its fields or variants.
- **Errors** — update the rustdoc comments on error variants and any related error-handling documentation.
- **Invariants** — update the relevant rustdoc comments and any invariant descriptions in `docs/CONTRACTS.md`.

Specifically:

- Update `docs/CONTRACTS.md` whenever a contract's public interface, behavior, or invariants change.
- Update rustdoc comments (`///`) whenever a public function, struct, enum, or error changes.

Documentation changes should accompany the code changes in the same PR — do not defer them to a follow-up. A PR that changes public behavior without updating the corresponding documentation is considered incomplete.

## Commit format

```
feat(access-lease): add batch revoke function
fix(subscription): handle expired renewal correctly  
test(vault-registry): add max-tag edge case