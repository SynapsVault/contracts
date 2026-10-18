# Changelog

All notable changes to the SynapsVault contracts are documented here.

This file tracks **public-interface changes** (new / removed / renamed
entrypoints, changed argument types or return types), **storage-layout
changes** (new or removed `DataKey` variants, changed struct fields), and
**error-code changes** (new codes, renumbered codes, codes that change
meaning).  Internal refactors that do not affect the on-chain interface are
omitted.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/) (`MAJOR.MINOR.PATCH`):
- **MAJOR** — breaking change (storage layout, removed entrypoint, renumbered
  error code, changed event shape).
- **MINOR** — backwards-compatible addition (new entrypoint, new event, new
  error code with a previously-unused discriminant).
- **PATCH** — bug fix or internal refactor with no observable interface change.

> **Note for maintainers:** every pull request that changes a contract's public
> interface, storage layout, events, or error codes **must** add an entry here
> and update `docs/CONTRACTS.md` in the same PR.  The CI `check-events` job
> enforces that `docs/CONTRACTS.md` event tables stay in sync with the source.

---

## [Unreleased]

### vault-registry

#### Added
- `retire(id)` — admin-only entrypoint that permanently marks a resource as
  retired (`Resource.retired = true`).  Retired resources are excluded from
  `list` and can no longer be mutated by their creator.  Emits
  `("retire", id)`.
- `("retire", id)` event added to the Events table in `docs/CONTRACTS.md`.

### access-lease

#### Added
- `propose_admin(pending_admin)` — step 1 of the new two-step admin rotation
  flow (issue #42).  The current admin nominates a new address.  Stores a
  `PendingAdmin` entry in instance storage.  Emits `("proposeadmin",)`.
- `accept_admin(pending_admin)` — step 2.  The nominated address accepts,
  becomes the new admin, and the pending-admin slot is cleared.  Emits
  `("acceptadmin",)` followed by the canonical `("setadmin",)`.
- `pending_admin()` — read-only view of the currently nominated address, or
  `None` if no proposal is outstanding.
- `DataKey::PendingAdmin` — new instance-storage key for the pending address.
- `Error::NoPendingAdmin` (code 10) — `accept_admin` called with no pending
  proposal.
- `Error::NotPendingAdmin` (code 11) — `accept_admin` called by an address
  that is not the nominated pending admin.
- `("proposeadmin",)` and `("acceptadmin",)` events added to the Events table
  in `docs/CONTRACTS.md`.

#### Changed
- `set_admin` is now **deprecated** in favour of the two-step flow.  It
  remains callable for backwards compatibility but also clears any outstanding
  pending-admin proposal.

### subscription

_No interface changes in this release._

---

## [0.1.0] — 2025-01-01

> Initial release of all three SynapsVault contracts on Stellar testnet.

### vault-registry — 0.1.0

#### Added
- `init(admin)` — one-time initialiser; sets the upgrade admin.
- `register(creator, id, price, metadata, tags)` — register a new resource.
- `set_price(id, new_price)` — update the resource price.
- `update_metadata(id, metadata)` — update the off-chain content pointer.
- `set_tags(id, tags)` — replace the discovery tag list.
- `set_listed(id, listed)` / `delist(id)` — toggle discoverability.
- `propose_transfer(id, new_creator)` / `accept_transfer(id, new_creator)` /
  `cancel_transfer(id)` — two-step ownership transfer.
- `get(id)` / `exists(id)` / `get_owner(id)` / `count()` — read-only views.
- `list(start, limit, only_listed, tag)` — paginated resource listing with
  optional `only_listed` and `tag` filters; limit capped at 20.
- `upgrade(new_wasm_hash)` / `migrate()` — admin-gated WASM upgrade flow.
- `version()` / `get_version()` / `is_compatible(version)` — versioning.
- `admin()` — read current admin address.

**Storage keys:** `Resource(id)`, `Index(i)`, `PendingOwner(id)`, `Count`,
`Admin`, `Version`.

**Events:** `init`, `register`, `setprice`, `updmeta`, `settags`, `proptrans`,
`transfer`, `canceltrans`, `setlisted`, `delist`, `upgrade`.

**Errors:**

| Code | Name |
|---|---|
| 1 | `AlreadyRegistered` |
| 2 | `NotFound` |
| 3 | `InvalidPrice` |
| 4 | `MetadataTooLong` |
| 5 | `InvalidTag` |
| 6 | `NotAdmin` (reserved) |
| 7 | `NotInitialised` |
| 8 | `AlreadyInitialised` |
| 9 | `MetadataEmpty` |
| 10 | `InvalidResourceId` |
| 11 | `NoPendingTransfer` |
| 12 | `NotPendingOwner` |

---

### access-lease — 0.1.0

#### Added
- `init(admin)`, `admin()`, `set_admin(new_admin)`.
- `grant_lease(resource_id, buyer, duration_ledgers)` /
  `grant_leases(requests)` — single and batch lease grant.
- `extend_lease(resource_id, buyer, extra_ledgers)`.
- `revoke_lease(resource_id, buyer)`.
- `is_valid(resource_id, buyer)` / `get_lease(resource_id, buyer)`.
- `upgrade(new_wasm_hash)` / `migrate()` / `get_version()` /
  `is_compatible()` / `get_version()`.

**Storage keys:** `Admin`, `Version`, `Lease(resource_id, buyer)`.

**Events:** `init`, `setadmin`, `grant`, `extend`, `revoke`, `upgrade`,
`migrate`.

**Errors:**

| Code | Name |
|---|---|
| 1 | `NotAdmin` (reserved) |
| 2 | `LeaseNotFound` |
| 3 | `AlreadyActive` |
| 4 | `InvalidDuration` |
| 5 | `NotInitialised` |
| 7 | `AlreadyInitialised` |
| 8 | `LeaseExpired` (reserved) |
| 9 | `InvalidResourceId` |

---

### subscription — 0.1.0

#### Added
- `init(admin)`, `admin()`, `set_admin(new_admin)`.
- `create_plan(publisher, plan_id, price_per_cycle)` / `get_plan(plan_id)` /
  `set_plan_active(plan_id, active)`.
- `subscribe(plan_id, subscriber, payment_reference)` /
  `renew(plan_id, subscriber, payment_reference)` /
  `cancel(plan_id, subscriber)`.
- `is_active(plan_id, subscriber)` /
  `get_subscription(plan_id, subscriber)`.
- `upgrade(new_wasm_hash)` / `migrate()` / `get_version()` /
  `is_compatible()`.

**Storage keys:** `Admin`, `Version`, `Plan(plan_id)`,
`Sub(plan_id, subscriber)`.

**Events:** `init`, `setadmin`, `plan`, `setactive`, `subscribe`, `renew`,
`cancel`, `upgrade`.

**Errors:**

| Code | Name |
|---|---|
| 2 | `PlanNotFound` |
| 3 | `PlanInactive` |
| 4 | `AlreadySubbed` |
| 5 | `SubNotFound` |
| 6 | `Cancelled` |
| 7 | `InvalidPrice` |
| 8 | `NotInitialised` |
| 9 | `AlreadyInitialised` |
| 10 | `PlanExists` |
| 11 | `Overflow` |
| 12 | `InvalidPlanId` |
| 13 | `SameAdmin` |

[Unreleased]: https://github.com/SynapsVault/contracts/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/SynapsVault/contracts/releases/tag/v0.1.0
