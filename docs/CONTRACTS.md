# SynapsVault Contracts — Interface Reference

This document is the source-of-truth reference for the public interface,
storage layout, events and error codes of each contract in this workspace.
Keep it in sync with the code: any change to a contract's public interface,
storage or error codes must update this file in the same pull request.

- [Common conventions](#common-conventions)
- [vault-registry](#vault-registry)
- [access-lease](#access-lease)
- [subscription](#subscription)

---

## Common conventions

| Topic | Convention |
|---|---|
| Amounts | `i128` USDC stroops (7 decimals). Always strictly positive. |
| Time | Ledger sequence numbers (`u32`). ~5 s per ledger, 17,280 ledgers ≈ 1 day. |
| Admin | A single admin address held in instance storage, set once by `init`. In production this is the backend platform wallet. |
| `init` | One-time. A second call fails with `AlreadyInitialised` — it can never overwrite the admin. `init` does not require the admin's signature, so deploy and init in the same pipeline run. |
| Admin rotation | `set_admin(new_admin)` on `access-lease` and `subscription`, gated by the current admin. |
| Upgrades | `upgrade(new_wasm_hash)` — admin-only; storage is preserved. |
| Versioning | `get_version()` returns the version recorded at `init` (crate version); `is_compatible(...)` detects stale deployments. |
| Errors | Every fallible entrypoint returns `Result<_, Error>`; codes are stable and never renumbered. |
| Events | Defined as `#[contractevent]` types (so they appear in the contract spec and generated bindings). Topics start with a fixed symbol; data is a single value. Unit tests (`events_have_documented_shape`) pin the wire format. |

### Roles

| Role | Capabilities |
|---|---|
| **Admin** | Initialise-time owner. Upgrades contracts, rotates itself, grants/extends/revokes leases, subscribes/renews subscribers. |
| **Creator** (`vault-registry`) | Registers resources and is the only one able to mutate them; may transfer ownership. |
| **Publisher** (`subscription`) | Creates and deactivates their own plans. |
| **Buyer / Subscriber** | Holds leases/subscriptions; a subscriber can cancel their own subscription. |
| **Anyone** | All read-only entrypoints (`get`, `list`, `is_valid`, `is_active`, …) are permissionless. |

---

## vault-registry

On-chain registry of vault resources: creator, price, metadata pointer, tags
and listing status. Payment itself happens off-contract (x402 + USDC SAC); the
registry is the transparent source of truth for *what* exists, *who* owns it
and *what it costs*.

### Data

```rust
pub struct Resource {
    pub id: String,         // unique, immutable
    pub creator: Address,   // only address allowed to mutate the resource
    pub price: i128,        // USDC stroops, > 0
    pub metadata: String,   // IPFS URI / content hash, <= 512 bytes
    pub listed: bool,       // true on register
    pub tags: Vec<String>,  // <= 8 tags, each 1..=32 bytes
}
```

| Constant | Value |
|---|---|
| `MAX_METADATA_POINTER_LEN` | 512 bytes |
| `MAX_TAGS` | 8 |
| `MAX_TAG_LEN` | 32 bytes |
| `MAX_PAGE_SIZE` | 20 |

### Functions

| Function | Auth | Returns | Errors |
|---|---|---|---|
| `init(admin)` | — (one-time) | `()` | `AlreadyInitialised` |
| `admin()` | — | `Address` | `NotInitialised` |
| `upgrade(new_wasm_hash)` | Admin | `()` | `NotInitialised` |
| `register(creator, id, price, metadata, tags)` | `creator` | `()` | `InvalidPrice`, `MetadataTooLong`, `InvalidTag`, `AlreadyRegistered` |
| `set_price(id, new_price)` | Creator | `()` | `InvalidPrice`, `NotFound` |
| `update_metadata(id, metadata)` | Creator | `()` | `NotFound`, `MetadataTooLong` |
| `set_tags(id, tags)` | Creator | `()` | `InvalidTag`, `NotFound` |
| `transfer_ownership(id, new_creator)` | Creator | `()` | `NotFound` |
| `set_listed(id, listed)` | Creator | `()` | `NotFound` |
| `delist(id)` | Creator | `()` | `NotFound` |
| `list(start, limit)` | — | `Vec<Resource>` | — (`limit` capped at 20) |
| `get(id)` | — | `Resource` | `NotFound` |
| `exists(id)` | — | `bool` | — |
| `get_owner(id)` | — | `Address` | `NotFound` |
| `count()` | — | `u32` | — |
| `version()` | — | `String` (compiled version) | — |
| `get_version()` | — | `String` (version stored at `init`) | — |
| `is_compatible(version)` | — | `bool` — major.minor matches | — |

The registry itself works without `init`; the admin is only needed for `upgrade`.

### Storage

| Key | Storage | Value | TTL |
|---|---|---|---|
| `Resource(id)` | persistent | `Resource` | bumped to 30 days on every write |
| `Index(i)` | persistent | resource id (insertion order) | bumped to 30 days on write |
| `Count` | instance | `u32`, monotonic | instance bumped on write |
| `Admin` | instance | `Address` | — |
| `Version` | instance | `String` | — |

### Events

| Topics | Data |
|---|---|
| `("init",)` | admin |
| `("register", creator)` | id |
| `("setprice", id)` | new price |
| `("updmeta", id)` | `()` |
| `("settags", id)` | tags |
| `("transfer", id)` | new creator |
| `("setlisted", id)` | listed |
| `("upgrade",)` | new wasm hash |

### Errors

| Code | Name | Meaning |
|---|---|---|
| 1 | `AlreadyRegistered` | `id` already exists. |
| 2 | `NotFound` | No resource with `id`. |
| 3 | `InvalidPrice` | Price ≤ 0. |
| 4 | `MetadataTooLong` | Metadata > 512 bytes. |
| 5 | `InvalidTag` | > 8 tags, or a tag is empty / > 32 bytes. |
| 6 | `NotAdmin` | Reserved. |
| 7 | `NotInitialised` | Admin-only call before `init`. |
| 8 | `AlreadyInitialised` | `init` called twice. |

---

## access-lease

Time-limited access grants. The backend issues a `Lease` with an `expires_at`
ledger; any party can verify access with a single permissionless read.

### Data

```rust
pub struct Lease {
    pub resource_id: String,
    pub buyer: Address,
    pub granted_at: u32,       // ledger of grant
    pub expires_at: u32,       // exclusive
    pub duration_ledgers: u32, // as originally requested
}
```

A lease is **valid** iff it exists and `expires_at > ledger.sequence()`.

### Functions

| Function | Auth | Returns | Errors |
|---|---|---|---|
| `init(admin)` | — (one-time) | `()` | `AlreadyInitialised` |
| `admin()` | — | `Address` | `NotInitialised` |
| `set_admin(new_admin)` | Admin | `()` | `NotInitialised` |
| `grant_lease(resource_id, buyer, duration_ledgers)` | Admin | `Lease` | `NotInitialised`, `InvalidDuration`, `AlreadyActive` |
| `extend_lease(resource_id, buyer, extra_ledgers)` | Admin | `Lease` | `NotInitialised`, `InvalidDuration`, `LeaseNotFound` |
| `revoke_lease(resource_id, buyer)` | Admin | `()` | `NotInitialised`, `LeaseNotFound` |
| `is_valid(resource_id, buyer)` | — | `bool` | — |
| `get_lease(resource_id, buyer)` | — | `Lease` (even if expired) | `LeaseNotFound` |
| `get_version()` | — | `String` | — |
| `is_compatible()` | — | `bool` — stored version == compiled | — |
| `upgrade(new_wasm_hash)` | Admin | `()` | `NotInitialised` |

**Semantics**

- `grant_lease` rejects a zero duration and any duration whose expiry would
  overflow `u32`. An expired lease may be replaced by a new grant.
- `extend_lease` computes `max(expires_at, now) + extra_ledgers`, so an expired
  lease restarts from the current ledger. Zero / overflowing extensions are rejected.
- `revoke_lease` deletes the record.

### Storage

| Key | Storage | Value | TTL |
|---|---|---|---|
| `Admin` | instance | `Address` | instance bumped to 90 days on admin calls |
| `Version` | instance | `String` | — |
| `Lease(resource_id, buyer)` | persistent | `Lease` | bumped to 90 days on write |

### Events

| Topics | Data |
|---|---|
| `("init",)` | admin |
| `("setadmin",)` | new admin |
| `("grant", resource_id, buyer)` | `expires_at` |
| `("extend", resource_id, buyer)` | new `expires_at` |
| `("revoke", resource_id, buyer)` | `()` |
| `("upgrade",)` | new wasm hash |

### Errors

| Code | Name | Meaning |
|---|---|---|
| 1 | `NotAdmin` | Reserved. |
| 2 | `LeaseNotFound` | No lease for the pair. |
| 3 | `AlreadyActive` | An unexpired lease already exists. |
| 4 | `InvalidDuration` | Zero duration, or expiry would overflow. |
| 5 | `NotInitialised` | Admin-only call before `init`. |
| 6 | `UpgradeNotAllowed` | Reserved. |
| 7 | `AlreadyInitialised` | `init` called twice. |

---

## subscription

Recurring 30-day subscription plans. Publishers define plans; the backend
subscribes buyers after payment and renews each cycle. Subscribers can
self-cancel and keep access through the end of the paid period.

### Data

```rust
pub struct Plan {
    pub plan_id: String,
    pub publisher: Address,
    pub price_per_cycle: i128, // USDC stroops, > 0
    pub active: bool,          // false => no new subscriptions
}

pub struct Subscription {
    pub plan_id: String,
    pub subscriber: Address,
    pub started_at: u32,
    pub current_period_end: u32,
    pub cancelled: bool,       // blocks renewals; access runs to period end
    pub total_renewals: u32,
}
```

`CYCLE` = 30 days = 518,400 ledgers. A subscription is **active** iff it exists
and `current_period_end > ledger.sequence()` — this includes cancelled
subscriptions whose paid period has not yet ended.

### Functions

| Function | Auth | Returns | Errors |
|---|---|---|---|
| `init(admin)` | — (one-time) | `()` | `AlreadyInitialised` |
| `admin()` | — | `Address` | `NotInitialised` |
| `set_admin(new_admin)` | Admin | `()` | `NotInitialised` |
| `create_plan(publisher, plan_id, price_per_cycle)` | `publisher` | `Plan` | `InvalidPrice`, `PlanExists` |
| `get_plan(plan_id)` | — | `Plan` | `PlanNotFound` |
| `deactivate_plan(plan_id)` | Publisher | `()` | `PlanNotFound` |
| `subscribe(plan_id, subscriber)` | Admin | `Subscription` | `NotInitialised`, `PlanNotFound`, `PlanInactive`, `AlreadySubbed`, `Overflow` |
| `renew(plan_id, subscriber)` | Admin | `Subscription` | `NotInitialised`, `SubNotFound`, `Cancelled`, `Overflow` |
| `cancel(plan_id, subscriber)` | `subscriber` | `()` (idempotent) | `SubNotFound` |
| `is_active(plan_id, subscriber)` | — | `bool` | — |
| `get_subscription(plan_id, subscriber)` | — | `Subscription` | `SubNotFound` |
| `get_version()` | — | `String` | — |
| `is_compatible()` | — | `bool` | — |
| `upgrade(new_wasm_hash)` | Admin | `()` | `NotInitialised` |

**Semantics**

- `subscribe` is rejected while a non-cancelled subscription is still in its
  paid period. Re-subscribing after a cancel carries over any remaining paid
  time (`max(current_period_end, now) + CYCLE`).
- `renew` stacks from `max(current_period_end, now)`, and keeps working after
  the plan is deactivated.
- Plan ids are first-come-first-served; an existing plan can never be overwritten.

### Storage

| Key | Storage | Value | TTL |
|---|---|---|---|
| `Admin` | instance | `Address` | instance bumped to 1 year on admin calls |
| `Version` | instance | `String` | — |
| `Plan(plan_id)` | persistent | `Plan` | bumped to 1 year on write |
| `Sub(plan_id, subscriber)` | persistent | `Subscription` | bumped to 1 year on write |

### Events

| Topics | Data |
|---|---|
| `("init",)` | admin |
| `("setadmin",)` | new admin |
| `("plan", plan_id, publisher)` | price per cycle |
| `("deactiv", plan_id)` | `()` |
| `("subscribe", plan_id, subscriber)` | `current_period_end` |
| `("renew", plan_id, subscriber)` | new `current_period_end` |
| `("cancel", plan_id, subscriber)` | `()` |
| `("upgrade",)` | new wasm hash |

### Errors

| Code | Name | Meaning |
|---|---|---|
| 1 | `NotAdmin` | Reserved. |
| 2 | `PlanNotFound` | No plan with `plan_id`. |
| 3 | `PlanInactive` | Plan deactivated; no new subscriptions. |
| 4 | `AlreadySubbed` | Active, non-cancelled subscription exists. |
| 5 | `SubNotFound` | No subscription for the pair. |
| 6 | `Cancelled` | Cannot renew a cancelled subscription. |
| 7 | `InvalidPrice` | Price ≤ 0. |
| 8 | `NotInitialised` | Admin-only call before `init`. |
| 9 | `AlreadyInitialised` | `init` called twice. |
| 10 | `PlanExists` | `plan_id` already taken. |
| 11 | `Overflow` | Period end would overflow `u32`. |

---

## Example (Rust test client)

```rust
let env = Env::default();
env.mock_all_auths();

let lease = AccessLeaseClient::new(&env, &env.register(AccessLease, ()));
lease.init(&admin);
lease.grant_lease(&String::from_str(&env, "res_1"), &buyer, &17_280); // 1 day
assert!(lease.is_valid(&String::from_str(&env, "res_1"), &buyer));
```
