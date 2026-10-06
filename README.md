<div align="center">
  <h1>⬡ SynapsVault — Contracts</h1>
  <p><strong>Soroban smart contracts on the Stellar network</strong></p>
  <p>
    <a href="https://github.com/SynapsVault/contracts/actions"><img src="https://github.com/SynapsVault/contracts/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
    <img src="https://img.shields.io/badge/Soroban-v21-7D00FF" alt="Soroban v21">
    <img src="https://img.shields.io/badge/Rust-1.78%2B-orange" alt="Rust">
    <img src="https://img.shields.io/badge/network-Stellar-blue" alt="Stellar">
    <img src="https://img.shields.io/badge/license-MIT-green" alt="MIT">
  </p>
</div>

---

## Contracts

### `vault-registry`

The on-chain registry for SynapsVault resources. Stores creator address, price (in USDC stroops), metadata pointer (IPFS CID / content hash), tags, and listing status.

Only the registered creator can mutate their resource (`require_auth`). Ownership can be transferred. Supports paginated listing and metadata updates.

### `access-lease` ⭐

Time-limited on-chain access grants. The backend issues a `Lease` struct with a specific `expires_at` ledger sequence. Any party can verify access with a single read — no need to trust the backend.

**Functions**

| Function | Auth | Description |
|---|---|---|
| `init(admin)` | — | Set admin at deploy |
| `grant_lease(resource_id, buyer, duration_ledgers)` | Admin | Issue a timed lease |
| `extend_lease(resource_id, buyer, extra_ledgers)` | Admin | Extend lease |
| `is_valid(resource_id, buyer)` | None | Check active status |
| `get_lease(resource_id, buyer)` | None | Full lease struct |
| `revoke_lease(resource_id, buyer)` | Admin | Revoke on refund / ToS |

### `subscription` ⭐

Recurring 30-day subscription plans. Publishers define plans; the backend subscribes buyers and renews each cycle. Subscribers can self-cancel with access through the period end.

**Functions**

| Function | Auth | Description |
|---|---|---|
| `init(admin)` | — | Set admin at deploy |
| `create_plan(publisher, plan_id, price_per_cycle)` | Publisher | Define a plan |
| `deactivate_plan(plan_id)` | Publisher | Close plan to new subs |
| `subscribe(plan_id, subscriber)` | Admin | Start subscription |
| `renew(plan_id, subscriber)` | Admin | Add one billing cycle |
| `cancel(plan_id, subscriber)` | Subscriber | Self-cancel |
| `is_active(plan_id, subscriber)` | None | Check active status |
| `get_subscription(plan_id, subscriber)` | None | Full sub struct |

---

## Quick start

```bash
# Prerequisites
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add wasm32-unknown-unknown
cargo install --locked soroban-cli

# Clone
git clone https://github.com/SynapsVault/contracts SynapsVault-contracts
cd SynapsVault-contracts

# Run tests
cargo test --workspace

# Build WASM
cargo build --target wasm32-unknown-unknown --release --workspace
# Output: target/wasm32-unknown-unknown/release/*.wasm
```

## Deploy to Stellar testnet

```bash
# Ensure Soroban CLI is configured for testnet
soroban network add testnet \
  --rpc-url https://soroban-testnet.stellar.org \
  --network-passphrase "Test SDF Network ; September 2015"

# Fund your account
soroban keys generate --global deployer
soroban keys fund deployer --network testnet

# Deploy vault-registry
soroban contract deploy \
  --wasm target/wasm32-unknown-unknown/release/vault_registry.wasm \
  --source deployer \
  --network testnet

# Deploy access-lease
soroban contract deploy \
  --wasm target/wasm32-unknown-unknown/release/access_lease.wasm \
  --source deployer \
  --network testnet

# Deploy subscription
soroban contract deploy \
  --wasm target/wasm32-unknown-unknown/release/subscription.wasm \
  --source deployer \
  --network testnet
```

## Deploy to Stellar mainnet

Mainnet deploys follow the same flow as testnet with a different network
configuration and a hardware-backed signing key. **Never** reuse the testnet
deployer key on mainnet.

```bash
# Configure mainnet
soroban network add mainnet \
  --rpc-url https://soroban-mainnet.stellar.org \
  --network-passphrase "Public Global Stellar Network ; September 2015"

# Use a dedicated, hardware-backed mainnet key
soroban keys generate --global mainnet-deployer
# Fund it with real XLM before proceeding.

# Build a reproducible, optimized WASM artifact
cargo build --target wasm32-unknown-unknown --release --workspace
soroban contract optimize \
  --wasm target/wasm32-unknown-unknown/release/vault_registry.wasm
# ...repeat optimize for access_lease.wasm and subscription.wasm

# Deploy each contract (record the returned contract IDs)
soroban contract deploy \
  --wasm target/wasm32-unknown-unknown/release/vault_registry.wasm \
  --source mainnet-deployer \
  --network mainnet

# Initialize each contract with the platform admin wallet
soroban contract invoke \
  --id <C_REGISTRY_ID> \
  --source mainnet-deployer \
  --network mainnet \
  -- init --admin <PLATFORM_ADMIN_ADDRESS>
```

**Mainnet checklist**

1. All tests green on `main` and the release tag is signed.
2. WASM artifacts are optimized and their hashes recorded (see *Upgrades*).
3. Contract IDs and admin address are written to the backend's mainnet config.
4. `init` is called exactly once per contract — it is not idempotent.
5. A post-deploy smoke test verifies `is_valid` / `is_active` reads.

## Upgrades

Contracts are upgradeable through an **admin-gated upgrade entrypoint**. The
admin uploads a new WASM blob to the ledger, then points the existing contract
at that blob's hash. Storage layout is preserved across upgrades, so existing
leases, plans, and subscriptions remain readable.

**Flow**

```bash
# 1. Build and optimize the new WASM
cargo build --target wasm32-unknown-unknown --release --workspace
soroban contract optimize \
  --wasm target/wasm32-unknown-unknown/release/access_lease.wasm

# 2. Upload the WASM blob and capture its hash
soroban contract upload \
  --wasm target/wasm32-unknown-unknown/release/access_lease.wasm \
  --source mainnet-deployer \
  --network mainnet
# -> returns the wasm hash, e.g. 0xabc123...

# 3. Point the live contract at the new hash (admin auth required)
soroban contract invoke \
  --id <C_LEASE_ID> \
  --source mainnet-deployer \
  --network mainnet \
  -- upgrade --new_wasm_hash 0xabc123...
```

The `upgrade` entrypoint calls `require_auth()` on the stored admin address, so
only the platform admin wallet can change a contract's code. Uploading a WASM
blob is permissionless, but it has no effect until the admin invokes `upgrade`.

**Migration notes**

- The upgrade entrypoint does **not** run data migrations. Any change to a
  stored struct's layout must be handled by a follow-up admin call that reads
  and rewrites entries in the new shape.
- Additive changes (new fields appended to a struct) are safe only if the
  contract tolerates missing fields on read; prefer a versioned key or an
  explicit migration entrypoint for anything else.
- Always upgrade on testnet first, verify reads against existing data, then
  repeat the exact same hash on mainnet.
- Keep the previous WASM hash recorded so a rollback is a single `upgrade`
  call.

## Gas optimization

Contract entrypoints are written to minimize ledger footprint and CPU
instructions, since both drive Soroban fees:

- **Batched storage reads.** `is_valid` / `is_active` fetch the lease or
  subscription in a single `get`, avoiding repeated instance lookups.
- **Packed structs.** Fields are ordered to avoid padding and use the smallest
  integer types that fit (`u32` ledger sequences, `i128` amounts only where
  required).
- **No unbounded loops.** Paginated listing caps page size so a single call
  cannot exceed the instruction budget.
- **TTL bumps on write only.** Entries are bumped once per mutation rather than
  on every read, keeping read paths cheap.
- **Optimized WASM.** Release builds run through `soroban contract optimize`
  before deploy, shrinking the blob and its upload cost.
=======

## CI/CD

```
Push to feat/* ──► Cargo test + fmt + clippy
                         │
Merge to dev   ──► Tests + WASM build
                         │
Merge to main  ──► Tests + WASM build + deploy to Stellar testnet
```

GitHub secrets required for auto-deploy:
- `STELLAR_TESTNET_SECRET_KEY` — Stellar keypair with testnet XLM

## Architecture

```
Stellar Ledger
│
├── vault-registry     (C_REGISTRY_ID)
│   └── Resource { id, creator, price, metadata, tags, listed }
│
├── access-lease       (C_LEASE_ID)
│   └── Lease { resource_id, buyer, granted_at, expires_at }
│
└── subscription       (C_SUB_ID)
    ├── Plan { plan_id, publisher, price_per_cycle, active }
    └── Subscription { plan_id, subscriber, period_end, renewals }
```

## Storage TTLs

All persistent entries are bumped **90 days** on every write — actively managed resources are never archived by the ledger's state expiry mechanism.

## Security

- Every mutating call is gated by `require_auth()` on the appropriate authority.
- The `admin` role is held by the backend's platform wallet — **not** a user wallet.
- `is_valid` and `is_active` are fully permissionless — any frontend or smart contract can verify access without relying on SynapsVault's API.
- Contract upgrades are admin-gated: uploading a WASM blob is permissionless,
  but only the admin can invoke `upgrade` to change live code.

### `access-lease` audit findings

An internal security review of `access-lease` covered lease issuance, expiry,
and revocation. Findings and their resolutions:

| ID | Severity | Finding | Resolution |
|---|---|---|---|
| AL-1 | High | `grant_lease` accepted `duration_ledgers = 0`, minting a lease that was never valid yet consumed storage. | Reject zero/negative durations; `grant_lease` now requires `duration_ledgers > 0`. |
| AL-2 | Medium | `extend_lease` on an expired lease silently restarted access from the current ledger, letting a lapsed buyer regain access without a new grant. | `extend_lease` now extends from `max(expires_at, current_ledger)` and requires an active lease. |
| AL-3 | Medium | `revoke_lease` left the lease entry in place, so a later `grant_lease` for the same `(resource_id, buyer)` could be shadowed by stale data. | `revoke_lease` removes the entry and bumps TTL on the tombstone-free path. |
| AL-4 | Low | `is_valid` compared `expires_at` with `<=`, treating a lease as invalid on its final ledger. | Boundary changed to `<` so the final ledger is inclusive. |
| AL-5 | Low | Admin rotation was impossible; a compromised admin key required redeploying the contract. | Added an admin-only `set_admin` entrypoint. |

All findings are resolved in the current release. No open High or Medium issues
remain as of this revision.

## Repo siblings

| Repo | Description |
|---|---|
| [SynapsVault-frontend](https://github.com/SynapsVault/frontend) | React UI |
| [SynapsVault-backend](https://github.com/SynapsVault/backend) | Express API |

## License

MIT © 2025 Busiii-adetiba
# SynapsVault Contracts — deployed on Stellar testnet
