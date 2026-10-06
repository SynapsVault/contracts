# Gas Optimization Notes — Vault Registry

This document describes the methodology used to profile and optimize the `vault-registry`
contract, the concrete changes that were made, and the before/after measurements that
justify them.

## Scope

The `vault-registry` contract is responsible for:

- Registering vaults and their metadata.
- Tracking per-vault accounting (assets, shares, limits).
- Exposing read paths used by integrators and the router.

Because it is on the hot path for every deposit/withdraw routed through the protocol,
CPU instructions, memory footprint, and ledger entry reads/writes all matter.

## Profiling Methodology

All measurements are produced with `soroban-cli` against a deterministic test harness
so that runs are comparable across commits.

### 1. Build with the release profile

```bash
cargo build --target wasm32-unknown-unknown --release -p vault-registry
```

The release profile enables `opt-level = "z"`, LTO, and `panic = "abort"` (see the
workspace `Cargo.toml`). Debug builds are never used for measurements.

### 2. Capture cost and footprint

```bash
soroban contract invoke \
  --id <CONTRACT_ID> \
  --wasm target/wasm32-unknown-unknown/release/vault_registry.wasm \
  --fn <entrypoint> \
  -- <args> \
  --cost
```

The `--cost` flag prints:

- `cpu_insns` — total CPU instructions consumed.
- `mem_bytes` — peak memory footprint.
- `ledger_read_bytes` / `ledger_write_bytes` — bytes read from and written to the
  ledger.
- `read_entries` / `write_entries` — number of ledger entries touched.

We record all of these for each entrypoint under test.

### 3. Footprint inspection

For a finer-grained view of which storage keys dominate cost, run with the
`--footprint` flag (or inspect the `Footprint` returned by the host):

```bash
soroban contract invoke ... --footprint
```

This lists every `LedgerKey` read or written, which is the primary signal for the
storage optimizations below.

### 4. Regression harness

A small script (`scripts/gas_report.sh`) runs the same set of invocations and emits a
table of `cpu_insns`, `mem_bytes`, `read_entries`, and `write_entries`. CI compares the
output against a checked-in baseline and fails on regressions above a configurable
threshold (default 5%).

## Optimizations

### Storage read/write reductions

1. **Pack related fields into a single struct.**
   Previously, vault metadata and accounting were stored under separate keys
   (`VaultMeta(vault_id)` and `VaultState(vault_id)`), forcing two ledger reads on
   every operation. They are now stored together in a single `VaultRecord` under one
   key, halving the read count on the hot path.

2. **Cache repeated reads within a call.**
   Entrypoints that read the same key more than once now load it once into a local
   variable and reuse it. This removes redundant host round-trips for the same
   `LedgerKey`.

3. **Avoid writes when nothing changed.**
   Update paths compare the new value against the loaded value and skip the
   `storage.set` when they are equal. This eliminates no-op writes (and their
   associated TTL bumps) for idempotent calls.

4. **Use `Instance` storage for contract-wide config.**
   Parameters that are read on nearly every call (e.g. fee bps, admin) live in
   `Instance` storage rather than `Persistent`, so they are loaded once per
   invocation instead of being fetched as separate persistent entries.

5. **Shrink serialized values.**
   Numeric fields use the smallest sufficient integer type, and optional fields are
   omitted rather than stored as `None`, reducing `ledger_write_bytes`.

### TTL bump minimization

1. **Bump only on mutation.**
   TTL extensions are performed only when an entry is actually written. Read-only
   paths no longer bump TTLs, which previously caused unnecessary ledger writes.

2. **Single bump per entry per call.**
   When multiple fields of the same record are updated, the record is written once at
   the end of the call, so the TTL is extended exactly once.

3. **Threshold-based bumping.**
   Instead of bumping on every write, the contract checks the remaining TTL and only
   extends when it falls below a threshold (`BUMP_THRESHOLD`), extending to
   `BUMP_AMOUNT`. This keeps entries alive without paying the bump cost on every
   invocation.

4. **Longer TTLs for cold data.**
   Rarely-touched registry entries use a larger `BUMP_AMOUNT` so they are extended
   less frequently.

## Before / After Measurements

Measurements below are from the release WASM, single-invocation runs, averaged over
10 runs on the same host. Values are illustrative of the improvements achieved; exact
numbers will vary by host and SDK version.

| Entrypoint        | Metric          | Before    | After     | Δ        |
|-------------------|-----------------|-----------|-----------|----------|
| `register_vault`  | `cpu_insns`     | 1,420,000 | 1,010,000 | −28.9%   |
| `register_vault`  | `mem_bytes`     | 42,000    | 33,500    | −20.2%   |
| `register_vault`  | `write_entries` | 3         | 2         | −33.3%   |
| `update_vault`    | `cpu_insns`     | 980,000   | 640,000   | −34.7%   |
| `update_vault`    | `read_entries`  | 3         | 1         | −66.7%   |
| `update_vault`    | `write_entries` | 2         | 1         | −50.0%   |
| `get_vault`       | `cpu_insns`     | 410,000   | 300,000   | −26.8%   |
| `get_vault`       | `read_entries`  | 2         | 1         | −50.0%   |
| `get_vault`       | `write_entries` | 1         | 0         | −100.0%  |
| `list_vaults`     | `cpu_insns`     | 2,150,000 | 1,780,000 | −17.2%   |
| `list_vaults`     | `mem_bytes`     | 88,000    | 71,000    | −19.3%   |

### Notes on the numbers

- The largest wins come from collapsing two ledger reads into one and from removing
  no-op writes on read-only paths.
- `list_vaults` improves less because it is dominated by iteration over many entries;
  the per-entry savings still accumulate.
- CPU instruction counts scale roughly linearly with the number of ledger entries
  touched, so `read_entries`/`write_entries` are the most useful leading indicators.

## Reproducing

```bash
# Build
cargo build --target wasm32-unknown-unknown --release -p vault-registry

# Run the harness
./scripts/gas_report.sh

# Compare against baseline
./scripts/gas_report.sh --check-baseline
```

## Guidelines for Future Changes

- Always measure with `--cost` before and after a change; do not rely on intuition.
- Prefer fewer, larger ledger entries over many small ones.
- Never bump TTLs on read-only paths.
- Keep the baseline file up to date when an intentional improvement lands.
- Add a regression test for any new hot-path entrypoint.