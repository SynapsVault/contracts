# Contributing to SynapsVault Contracts

Thanks for contributing! Please read the [Code of Conduct](CODE_OF_CONDUCT.md).

## Picking up an issue

- Comment on the issue (or apply through Drips Wave) and wait to be assigned before starting.
- Issues labelled `complexity: trivial`, `complexity: medium` or `complexity: high`
  describe the expected scope; each lists acceptance criteria.
- One issue per PR. Reference it with `Closes #<n>`.
- Report security vulnerabilities privately — see [SECURITY.md](SECURITY.md).

## Prerequisites

```bash
# Rust (rust-toolchain.toml pins stable + the wasm32v1-none target)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Stellar CLI v25.2+ (needed to build WASM, deploy and invoke)
cargo install --locked stellar-cli
```

## Development workflow

Run the same checks as CI before pushing:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Build WASM (production)
stellar contract build
```

Notes:

- `test_snapshots/` directories are generated on every test run and are git-ignored.
- `contracts/vault-registry/fixtures/vault_registry_upgrade_target.wasm` is the
  prebuilt WASM the upgrade tests upgrade to. Refresh it with `make fixture`
  in `contracts/vault-registry` if the registry's interface changes.
- Error codes are part of the public ABI: append new variants, never renumber.

## Adding a new contract

1. `mkdir -p contracts/my-contract/src`
2. Add `Cargo.toml` (see `access-lease` as template)
3. Write `src/lib.rs` with a `#[cfg(test)] mod tests`
4. Add `"contracts/my-contract"` to workspace `Cargo.toml`
5. Add a test step to `.github/workflows/ci.yml` and the deploy workflows
6. Document the interface in `README.md` and `docs/CONTRACTS.md`

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
```
