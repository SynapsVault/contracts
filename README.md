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

## Repo siblings

| Repo | Description |
|---|---|
| [SynapsVault-frontend](https://github.com/SynapsVault/frontend) | React UI |
| [SynapsVault-backend](https://github.com/SynapsVault/backend) | Express API |

## License

MIT © 2025 bolu26
# SynapsVault Contracts — deployed on Stellar testnet
