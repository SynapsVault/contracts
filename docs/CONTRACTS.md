# Contracts

This document is the authoritative reference for the three Soroban smart contracts that make up the platform:

| Contract | Crate | Responsibility |
| --- | --- | --- |
| **Vault Registry** | `vault-registry` | Registers resources and their metadata; issues and tracks leases. |
| **Access Lease** | `access-lease` | Manages time-bounded access leases against registered resources. |
| **Subscription** | `subscription` | Manages recurring plans and subscriber lifecycle. |

---

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Access Control Model](#access-control-model)
3. [Admin Role](#admin-role)
4. [Vault Registry Contract](#vault-registry-contract)
5. [Access Lease Contract](#access-lease-contract)
6. [Subscription Contract](#subscription-contract)
7. [Cross-Contract Interactions](#cross-contract-interactions)
8. [Usage Examples](#usage-examples)

---

## Architecture Overview

The three contracts are designed to be composed. The **Vault Registry** is the source of truth for what resources exist and who owns them. The **Access Lease** contract consumes registry data to grant time-bounded access. The **Subscription** contract consumes registry data to grant recurring access tied to a billing plan.

```
                 +----------------------+
                 |    Vault Registry    |
                 |  (resources, leases) |
                 +----------+-----------+
                            |
          +-----------------+-----------------+
          |                                   |
          v                                   v
  +---------------+                   +----------------+
  | Access Lease  |                   |  Subscription  |
  | (time-bound)  |                   |  (recurring)   |
  +---------------+                   +----------------+
```

Key design principles:

- **Single source of truth.** Resource metadata and ownership live only in the Vault Registry. The other contracts read from it and never duplicate it.
- **Explicit authorization.** Every state-mutating entry point requires an `Address` signature via `require_auth`.
- **Bounded storage.** All persistent entries carry a TTL that is bumped on access to avoid archival.
- **Monotonic identifiers.** Counters used for IDs only ever increase; IDs are never reused.

---

## Access Control Model

Authorization is enforced at two levels:

1. **Contract-level admin.** Each contract stores an `admin: Address` in instance storage. Admin-only functions call `admin.require_auth()` and compare the caller against the stored admin.
2. **Resource-level owner.** The Vault Registry stores an `owner: Address` per resource. Owner-only functions (e.g. updating metadata, revoking leases) call `owner.require_auth()` and verify the caller owns the resource.

Roles summary:

| Role | Capabilities |
| --- | --- |
| **Admin** | Initialize contracts, set/rotate admin, pause/unpause, upgrade. |
| **Resource Owner** | Register resources, update metadata, revoke leases, deactivate resources. |
| **Lessee / Subscriber** | Acquire leases, renew, cancel their own leases/subscriptions. |
| **Public** | Read-only queries (get resource, list leases, check status). |

---

## Admin Role

Each contract exposes the following admin surface:

| Function | Description |
| --- | --- |
| `initialize(admin)` | One-time setup. Stores the admin address. Panics if already initialized. |
| `set_admin(new_admin)` | Rotates the admin. Requires current admin auth. |
| `get_admin()` | Returns the current admin address. |
| `pause()` / `unpause()` | Toggles a global pause flag. Mutating entry points check this flag. |
| `is_paused()` | Returns the pause state. |

The admin is stored in **instance storage** under the key `DataKey::Admin` and is bumped with the instance TTL on every contract invocation.

---

## Vault Registry Contract

### Purpose

The Vault Registry is the canonical registry of resources. It stores resource metadata, ownership, and the set of leases associated with each resource. It is the only contract permitted to mint lease IDs.

### State Structures

#### `Resource`

```rust
pub struct Resource {
    pub id: u64,               // Monotonic identifier
    pub owner: Address,        // Owner authorized to mutate this resource
    pub name: String,          // Human-readable name
    pub uri: String,           // Off-chain metadata URI
    pub active: bool,          // Whether new leases may be issued
    pub created_at: u64,       // Ledger timestamp of creation
    pub updated_at: u64,       // Ledger timestamp of last update
}
```

#### `Lease`

```rust
pub struct Lease {
    pub id: u64,               // Monotonic identifier
    pub resource_id: u64,      // Reference to Resource.id
    pub lessee: Address,       // Holder of the lease
    pub start: u64,            // Ledger timestamp when the lease begins
    pub end: u64,              // Ledger timestamp when the lease expires
    pub revoked: bool,         // Set to true when the owner revokes
}
```

### Storage Keys

```rust
pub enum DataKey {
    Admin,                     // Instance: Address
    Paused,                    // Instance: bool
    ResourceCount,             // Instance: u64
    LeaseCount,                // Instance: u64
    Resource(u64),             // Persistent: Resource
    Lease(u64),                // Persistent: Lease
    ResourceLeases(u64),       // Persistent: Vec<u64>
}
```

### TTLs

| Storage | Constant | Value (ledgers) | Bump policy |
| --- | --- | --- | --- |
| Instance | `INSTANCE_TTL` | 30 days | Bumped on every invocation. |
| Persistent (Resource) | `RESOURCE_TTL` | 60 days | Bumped on read and write. |
| Persistent (Lease) | `LEASE_TTL` | 30 days | Bumped on read and write. |
| Persistent (ResourceLeases) | `RESOURCE_TTL` | 60 days | Bumped on read and write. |

### Function Reference

| Function | Parameters | Returns | Auth | Errors |
| --- | --- | --- | --- | --- |
| `initialize` | `admin: Address` | `()` | None (one-time) | `AlreadyInitialized` |
| `set_admin` | `new_admin: Address` | `()` | Admin | `NotAuthorized` |
| `get_admin` | — | `Address` | None | `NotInitialized` |
| `pause` | — | `()` | Admin | `NotAuthorized` |
| `unpause` | — | `()` | Admin | `NotAuthorized` |
| `is_paused` | — | `bool` | None | — |
| `register_resource` | `owner: Address, name: String, uri: String` | `u64` | Owner | `Paused`, `InvalidInput` |
| `update_resource` | `owner: Address, id: u64, name: String, uri: String` | `()` | Owner | `NotFound`, `NotAuthorized`, `Paused` |
| `deactivate_resource` | `owner: Address, id: u64` | `()` | Owner | `NotFound`, `NotAuthorized` |
| `get_resource` | `id: u64` | `Resource` | None | `NotFound` |
| `list_resources` | `start: u64, limit: u32` | `Vec<Resource>` | None | — |
| `issue_lease` | `owner: Address, resource_id: u64, lessee: Address, start: u64, end: u64` | `u64` | Owner | `NotFound`, `NotAuthorized`, `ResourceInactive`, `InvalidInput`, `Paused` |
| `revoke_lease` | `owner: Address, lease_id: u64` | `()` | Owner | `NotFound`, `NotAuthorized` |
| `get_lease` | `lease_id: u64` | `Lease` | None | `NotFound` |
| `list_leases` | `resource_id: u64` | `Vec<u64>` | None | `NotFound` |
| `is_lease_active` | `lease_id: u64` | `bool` | None | `NotFound` |

### Error Codes

| Code | Name | Meaning |
| --- | --- | --- |
| 1 | `AlreadyInitialized` | `initialize` called more than once. |
| 2 | `NotInitialized` | Contract used before `initialize`. |
| 3 | `NotAuthorized` | Caller failed `require_auth` or is not the owner/admin. |
| 4 | `NotFound` | Referenced resource or lease does not exist. |
| 5 | `ResourceInactive` | Attempted to issue a lease against an inactive resource. |
| 6 | `InvalidInput` | Empty name/URI, or `end <= start`. |
| 7 | `Paused` | Contract is paused. |

### Invariants

- **Monotonic count.** `ResourceCount` and `LeaseCount` only increase. IDs are never reused, even after revocation.
- **TTL bumping.** Every read or write of a persistent entry extends its TTL to the configured value.
- **Active-lease check.** `issue_lease` fails if the resource is inactive or if `end <= start`.
- **Owner consistency.** Only the stored `owner` of a resource may mutate it or issue/revoke its leases.
- **Lease linkage.** Every `Lease.resource_id` refers to an existing `Resource`, and the lease ID appears in `ResourceLeases(resource_id)`.

---

## Access Lease Contract

### Purpose

The Access Lease contract provides a thin, composable interface for acquiring and renewing time-bounded leases. It delegates resource validation to the Vault Registry and stores only the lease-to-holder mapping it needs.

### State Structures

#### `Lease`

```rust
pub struct Lease {
    pub id: u64,               // Local monotonic identifier
    pub registry_lease_id: u64,// Lease ID in the Vault Registry
    pub resource_id: u64,      // Reference to Resource.id
    pub lessee: Address,       // Holder
    pub start: u64,
    pub end: u64,
    pub renewed_at: u64,       // Last renewal timestamp
}
```

### Storage Keys

```rust
pub enum DataKey {
    Admin,                     // Instance: Address
    Paused,                    // Instance: bool
    LeaseCount,                // Instance: u64
    Lease(u64),                // Persistent: Lease
    LesseeLeases(Address),     // Persistent: Vec<u64>
}
```

### TTLs

| Storage | Constant | Value (ledgers) | Bump policy |
| --- | --- | --- | --- |
| Instance | `INSTANCE_TTL` | 30 days | Bumped on every invocation. |
| Persistent (Lease) | `LEASE_TTL` | 30 days | Bumped on read and write. |
| Persistent (LesseeLeases) | `LEASE_TTL` | 30 days | Bumped on read and write. |

### Function Reference

| Function | Parameters | Returns | Auth | Errors |
| --- | --- | --- | --- | --- |
| `initialize` | `admin: Address, registry: Address` | `()` | None (one-time) | `AlreadyInitialized` |
| `set_admin` | `new_admin: Address` | `()` | Admin | `NotAuthorized` |
| `get_admin` | — | `Address` | None | `NotInitialized` |
| `pause` / `unpause` | — | `()` | Admin | `NotAuthorized` |
| `is_paused` | — | `bool` | None | — |
| `acquire` | `lessee: Address, resource_id: u64, duration: u64` | `u64` | Lessee | `NotFound`, `ResourceInactive`, `InvalidInput`, `Paused` |
| `renew` | `lessee: Address, lease_id: u64, duration: u64` | `()` | Lessee | `NotFound`, `NotAuthorized`, `InvalidInput`, `Paused` |
| `cancel` | `lessee: Address, lease_id: u64` | `()` | Lessee | `NotFound`, `NotAuthorized` |
| `get_lease` | `lease_id: u64` | `Lease` | None | `NotFound` |
| `list_lessee_leases` | `lessee: Address` | `Vec<u64>` | None | — |
| `is_active` | `lease_id: u64` | `bool` | None | `NotFound` |

### Error Codes

| Code | Name | Meaning |
| --- | --- | --- |
| 1 | `AlreadyInitialized` | `initialize` called more than once. |
| 2 | `NotInitialized` | Contract used before `initialize`. |
| 3 | `NotAuthorized` | Caller is not the lessee or admin. |
| 4 | `NotFound` | Lease or resource does not exist. |
| 5 | `ResourceInactive` | Registry reports the resource as inactive. |
| 6 | `InvalidInput` | `duration == 0`. |
| 7 | `Paused` | Contract is paused. |

### Invariants

- **Monotonic count.** `LeaseCount` only increases.
- **TTL bumping.** Persistent entries are bumped on read and write.
- **Active-lease check.** `renew` fails if the lease has already expired; `is_active` returns `false` for expired or cancelled leases.
- **Registry linkage.** Every local lease references a valid `registry_lease_id`; the registry is the authority on resource state.
- **Lessee index consistency.** Every lease ID appears in `LesseeLeases(lessee)`.

---

## Subscription Contract

### Purpose

The Subscription contract manages recurring plans. A plan defines a resource, a price, and a billing period. Subscribers enroll in a plan and are billed per period; access is granted while the subscription is active.

### State Structures

#### `Plan`

```rust
pub struct Plan {
    pub id: u64,               // Monotonic identifier
    pub owner: Address,        // Plan owner (resource owner)
    pub resource_id: u64,      // Reference to Resource.id
    pub price: i128,           // Price per period, in stroops
    pub period: u64,           // Billing period in seconds
    pub active: bool,          // Whether new subscriptions may be created
    pub created_at: u64,
}
```

#### `Subscription`

```rust
pub struct Subscription {
    pub id: u64,               // Monotonic identifier
    pub plan_id: u64,          // Reference to Plan.id
    pub subscriber: Address,   // Subscriber
    pub started_at: u64,       // Enrollment timestamp
    pub paid_until: u64,       // Timestamp through which the subscription is paid
    pub cancelled: bool,       // Set when the subscriber cancels
}
```

### Storage Keys

```rust
pub enum DataKey {
    Admin,                     // Instance: Address
    Paused,                    // Instance: bool
    PlanCount,                 // Instance: u64
    SubscriptionCount,         // Instance: u64
    Plan(u64),                 // Persistent: Plan
    Subscription(u64),         // Persistent: Subscription
    SubscriberSubs(Address),   // Persistent: Vec<u64>
}
```

### TTLs

| Storage | Constant | Value (ledgers) | Bump policy |
| --- | --- | --- | --- |
| Instance | `INSTANCE_TTL` | 30 days | Bumped on every invocation. |
| Persistent (Plan) | `PLAN_TTL` | 60 days | Bumped on read and write. |
| Persistent (Subscription) | `SUBSCRIPTION_TTL` | 30 days | Bumped on read and write. |
| Persistent (SubscriberSubs) | `SUBSCRIPTION_TTL` | 30 days | Bumped on read and write. |

### Function Reference

| Function | Parameters | Returns | Auth | Errors |
| --- | --- | --- | --- | --- |
| `initialize` | `admin: Address` | `()` | None (one-time) | `AlreadyInitialized` |
| `set_admin` | `new_admin: Address` | `()` | Admin | `NotAuthorized` |
| `get_admin` | — | `Address` | None | `NotInitialized` |
| `pause` / `unpause` | — | `()` | Admin | `NotAuthorized` |
| `is_paused` | — | `bool` | None | — |
| `create_plan` | `owner: Address, resource_id: u64, price: i128, period: u64` | `u64` | Owner | `NotFound`, `InvalidInput`, `Paused` |
| `update_plan` | `owner: Address, plan_id: u64, price: i128, period: u64` | `()` | Owner | `NotFound`, `NotAuthorized`, `InvalidInput` |
| `deactivate_plan` | `owner: Address, plan_id: u64` | `()` | Owner | `NotFound`, `NotAuthorized` |
| `get_plan` | `plan_id: u64` | `Plan` | None | `NotFound` |
| `subscribe` | `subscriber: Address, plan_id: u64` | `u64` | Subscriber | `NotFound`, `PlanInactive`, `Paused` |
| `renew_subscription` | `subscriber: Address, subscription_id: u64` | `()` | Subscriber | `NotFound`, `NotAuthorized`, `Cancelled`, `Paused` |
| `cancel_subscription` | `subscriber: Address, subscription_id: u64` | `()` | Subscriber | `NotFound`, `NotAuthorized` |
| `get_subscription` | `subscription_id: u64` | `Subscription` | None | `NotFound` |
| `list_subscriber_subs` | `subscriber: Address` | `Vec<u64>` | None | — |
| `is_subscription_active` | `subscription_id: u64` | `bool` | None | `NotFound` |

### Error Codes

| Code | Name | Meaning |
| --- | --- | --- |
| 1 | `AlreadyInitialized` | `initialize` called more than once. |
| 2 | `NotInitialized` | Contract used before `initialize`. |
| 3 | `NotAuthorized` | Caller is not the owner, subscriber, or admin. |
| 4 | `NotFound` | Plan or subscription does not exist. |
| 5 | `PlanInactive` | Attempted to subscribe to an inactive plan. |
| 6 | `InvalidInput` | `price <= 0` or `period == 0`. |
| 7 | `Paused` | Contract is paused. |
| 8 | `Cancelled` | Operation attempted on a cancelled subscription. |

### Invariants

- **Monotonic count.** `PlanCount` and `SubscriptionCount` only increase.
- **TTL bumping.** Persistent entries are bumped on read and write.
- **Active-lease check.** `is_subscription_active` returns `true` only when `paid_until >= now` and `cancelled == false`.
- **Plan ownership.** Only the plan owner may update or deactivate a plan.
- **Subscriber index consistency.** Every subscription ID appears in `SubscriberSubs(subscriber)`.
- **Paid-until monotonicity.** `paid_until` never decreases; renewals extend it by exactly one period.

---

## Cross-Contract Interactions

| Caller | Callee | Purpose |
| --- | --- | --- |
| Access Lease | Vault Registry | Validate resource existence and active status before issuing a lease. |
| Subscription | Vault Registry | Validate resource existence before creating a plan. |
| Vault Registry | — | No outbound calls; it is the leaf dependency. |

Contracts are wired together at initialization time via the `registry` address passed to `Access Lease::initialize`. The Subscription contract reads the registry address from its own instance storage (set at initialization).

---

## Usage Examples

### Register a resource and issue a lease

```rust
// 1. Register a resource owned by `alice`.
let resource_id = registry.register_resource(&alice, &"Dataset A".into(), &"ipfs://...".into());

// 2. Issue a 7-day lease to `bob`.
let now = env.ledger().timestamp();
let lease_id = registry.issue_lease(
    &alice,
    &resource_id,
    &bob,
    &now,
    &(now + 7 * 24 * 60 * 60),
);

// 3. Verify the lease is active.
assert!(registry.is_lease_active(&lease_id));
```

### Acquire and renew an access lease

```rust
// Acquire a 1-day lease through the Access Lease contract.
let lease_id = access_lease.acquire(&bob, &resource_id, &(24 * 60 * 60));

// Renew for another day.
access_lease.renew(&bob, &lease_id, &(24 * 60 * 60));

// Cancel.
access_lease.cancel(&bob, &lease_id);
```

### Create a plan and subscribe

```rust
// Owner creates a monthly plan priced at 100 XLM (in stroops).
let plan_id = subscription.create_plan(
    &alice,
    &resource_id,
    &1_000_000_000,
    &(30 * 24 * 60 * 60),
);

// Bob subscribes.
let sub_id = subscription.subscribe(&bob, &plan_id);

// Renew for another period.
subscription.renew_subscription(&bob, &sub_id);

// Check status.
assert!(subscription.is_subscription_active(&sub_id));
```

### Admin operations

```rust
// Rotate the admin.
registry.set_admin(&new_admin);

// Pause all mutations during an incident.
registry.pause();
assert!(registry.is_paused());

// Resume.
registry.unpause();