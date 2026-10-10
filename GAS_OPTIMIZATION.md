# Gas Optimization Notes

Soroban fees are driven by CPU instructions, memory, ledger entries
read/written, and WASM size. This document describes how the contracts keep
those low and how to measure them.

## Design choices

**Storage**

- **One entry per record.** A `Resource`, `Lease`, `Plan` or `Subscription` is a
  single `#[contracttype]` struct under one key, so each hot-path call touches
  exactly one persistent entry (plus the instance entry for admin calls).
- **Instance storage for contract-wide values.** `Admin`, `Version` and the
  registry `Count` live in instance storage, which is loaded once per invocation.
- **Single read for access checks.** `is_valid` / `is_active` do one `get` and
  a comparison — no auth, no writes, no TTL bumps.
- **Validation before storage access.** Price / tag / duration checks run
  before any ledger reads, so invalid calls fail cheaply.
- **No-op cancel.** Cancelling an already-cancelled subscription returns
  without writing.

**TTL**

- **Bump on write only.** Read paths never extend TTLs.
- **Threshold-based bumping.** `extend_ttl(threshold, amount)` is a no-op
  unless the remaining TTL is below `amount - 1 day`, so repeated writes in the
  same day pay no extension cost.

**Bounded work**

- `vault-registry::list` caps page size at `MAX_PAGE_SIZE` (20), so one call
  can never exceed the per-transaction budget regardless of registry size.
- Tags are bounded (`MAX_TAGS` = 8, `MAX_TAG_LEN` = 32) and metadata pointers
  are bounded (`MAX_METADATA_POINTER_LEN` = 512 bytes), which bounds entry size.

**WASM size**

The release profile (workspace `Cargo.toml`) uses `opt-level = "z"`, LTO,
`codegen-units = 1`, `panic = "abort"` and stripped symbols, and
`stellar contract build` strips unused contract-spec entries. CI fails the build
if any contract exceeds Soroban's 64 KiB limit and reports sizes in the job
summary.

| Contract | Release WASM (`stellar contract build`, soroban-sdk 28) |
|---|---|
| `subscription` | ~16 KB |
| `vault-registry` | ~15 KB |
| `access-lease` | ~11 KB |

## Measuring

The `gas_register_and_list_budget` test in `vault-registry` prints the CPU and
memory cost of the hot entrypoints and asserts upper bounds, so large
regressions fail CI:

```bash
cargo test -p vault-registry gas -- --nocapture
```

Current numbers (host test environment, native execution — on-chain WASM
execution costs more, but relative changes track):

| Entrypoint | CPU instructions | Memory bytes |
|---|---|---|
| `register` (2 tags) | ~129,000 | ~59,000 |
| `set_price` | ~98,000 | ~45,000 |
| `get` | ~51,000 | ~32,000 |
| `list(0, 20)` | ~1,515,000 | ~652,000 |

To measure real on-chain cost, simulate against testnet:

```bash
stellar contract invoke --id <CONTRACT_ID> --source-account <KEY> \
  --network testnet --send=no -- get --id <RESOURCE_ID>
```

## Guidelines for future changes

- Measure before and after; update the table above when an intentional change lands.
- Prefer fewer, larger ledger entries over many small ones.
- Never bump TTLs or write on read-only paths.
- Keep every loop bounded by a constant.
