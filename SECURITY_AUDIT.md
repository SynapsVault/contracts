# Security Review — SynapsVault Contracts

**Scope:** `vault-registry`, `access-lease`, `subscription` — initialisation,
access control, expiry arithmetic, revocation, cancellation and the upgrade
entrypoints.
**Status:** All High and Medium findings below are resolved in the current code
**except SV-12** (unauthenticated `init`), whose fix was lost in a later merge —
see [SV-12](#1-findings) and [Residual risk](#4-residual-risk).

Each finding lists its severity, the issue, and how it is resolved. Every
resolution is covered by a unit test in the contract's test module.

---

## 1. Findings

| ID | Contract | Severity | Finding | Resolution | Status |
|---|---|---|---|---|---|
| SV-1 | access-lease, subscription | **Critical** | `init` could be called again by anyone after deployment, overwriting the admin and taking over every admin-only entrypoint (grant/revoke leases, subscribe/renew, `upgrade`). | `init` fails with `AlreadyInitialised` once an admin is set. Test: `init_twice_is_rejected`. | Resolved |
| SV-2 | subscription | **Critical** | `create_plan` overwrote an existing plan, letting any address re-create someone else's `plan_id` with itself as publisher and change the price. | `create_plan` fails with `PlanExists` for an existing id. Test: `existing_plan_cannot_be_hijacked`. | Resolved |
| SV-3 | vault-registry | Medium | Re-calling `init` returned the misleading `AlreadyRegistered` error. | New `AlreadyInitialised` error code. | Resolved |
| SV-4 | access-lease | High | `expires_at = now + duration` could overflow `u32`. With `overflow-checks` it aborted the call; without it would wrap to a past ledger. | `checked_add`; overflow returns `InvalidDuration`. Test: `overflowing_duration_rejected`. | Resolved |
| SV-5 | subscription | Medium | Period-end arithmetic (`+ CYCLE`) was unchecked. | `checked_add`; overflow returns `Overflow`. | Resolved |
| SV-6 | access-lease | Medium | `grant_lease` accepted `duration_ledgers = 0`; `extend_lease` accepted `0`. | Both reject zero with `InvalidDuration`. Test: `zero_duration_rejected`. | Resolved |
| SV-7 | access-lease, subscription | Medium | No admin rotation — a compromised admin key required a redeploy. | Admin-gated `set_admin`. Test: `set_admin_rotates_admin`. | Resolved |
| SV-8 | subscription | Medium | `is_active` returned `false` immediately on `cancel`, contradicting the documented "access until period end" behaviour and cutting off paid access. | `is_active` depends only on `current_period_end`; `cancelled` blocks renewals. Re-subscribing carries over remaining paid time. Tests: `cancel_keeps_access_until_period_end_and_blocks_renewal`, `resubscribe_after_cancel_carries_over_paid_time`. | Resolved |
| SV-9 | subscription | Low | `deactivate_plan` and `cancel` wrote entries without bumping their TTL, so entries could be archived. | TTL bumped on every write. | Resolved |
| SV-10 | all | Low | access-lease and subscription emitted no events, so there was no on-chain audit trail for grants, revocations, subscriptions or upgrades. | Events on every state change and on `upgrade` (see `docs/CONTRACTS.md`). | Resolved |
| SV-11 | access-lease, subscription | Low | Instance storage (holding the admin) was only bumped at `init` and could expire. | Instance TTL bumped on every admin-gated call. | Resolved |
| SV-12 | access-lease, subscription, vault-registry | High | `init` does not require the admin's signature, so a front-runner can call `init` first after deployment and install themselves as admin, taking over every admin-only entrypoint (`upgrade` and `migrate` everywhere; `set_admin` and granting leases/subscriptions on access-lease and subscription). | Fixed for access-lease by #43 (`c7eda2e`: `admin.require_auth()` in `init`, test `init_requires_admin_auth`), but the check and its test were dropped when PR #141 was merged (`eee2a5d`). `subscription` and `vault-registry` never had it. Fix: call `admin.require_auth()` in every `init` before writing state and restore the test. Until then, mitigated operationally (see §4). | **Open (regressed)** |

## 2. Access-control matrix (verified by tests)

| Entrypoint | Required signer | Non-signer test |
|---|---|---|
| `vault-registry::register` | `creator` | `mutations_require_creator_auth` |
| `vault-registry::set_price` / `update_metadata` / `set_tags` | stored creator | `set_price_by_non_creator_is_rejected` |
| `vault-registry::set_listed` / `delist` | stored creator | `mutations_require_creator_auth` |
| `vault-registry::propose_transfer` | stored creator | `mutations_require_creator_auth` |
| `vault-registry::accept_transfer` | proposed new creator | `accept_transfer_by_non_pending_owner_is_rejected` |
| `vault-registry::cancel_transfer` | stored creator | `mutations_require_creator_auth` |
| `vault-registry::upgrade` | admin | `upgrade_rejects_non_admin` |
| `access-lease::init`, `subscription::init`, `vault-registry::init` | **none** — see SV-12 | — (test removed; see SV-12) |
| `access-lease::grant_lease` / `extend_lease` / `revoke_lease` | admin | `grant_by_non_admin_is_rejected` |
| `access-lease::set_admin` | current admin (one-step rotation) | `set_admin_by_non_admin_is_rejected` |
| `access-lease::propose_admin` / `accept_admin` | current admin for `propose_admin`; pending admin for `accept_admin` | `set_admin_by_non_admin_is_rejected` |
| `subscription::set_admin` | current admin | `set_admin_by_non_admin_is_rejected` |
| `subscription::subscribe` / `renew` | admin | `subscribe_by_non_admin_is_rejected` |
| `subscription::deactivate_plan` | publisher | `deactivate_requires_publisher_auth` |
| `subscription::cancel` | subscriber | `cancel_keeps_access_until_period_end_and_blocks_renewal` |

## 3. Expiry semantics

- Time is measured in ledger sequence numbers, which are monotonic, so clock
  skew does not apply.
- A lease / subscription is valid while `end > ledger.sequence()`; on the
  ledger equal to `end` it is expired (tests: `lease_expires_exactly_at_expiry`
  and the cancellation test above).
- Soroban has no re-entrancy into the same contract, and none of these
  contracts make cross-contract calls.

## 4. Residual risk

- **Initialisation front-running (SV-12, open).** `init` on all three
  contracts accepts any `admin` address without that address's signature, and
  the first successful call wins (`AlreadyInitialised` blocks every later one).
  Between deployment and `init`, anyone watching the network can call `init`
  with their own address and take over `upgrade`, `migrate` and every other
  admin-only entrypoint. Until `admin.require_auth()` is restored in
  `init`:
  - deploy and initialise in the same pipeline run with no gap, submitting
    `init` from the deployer account immediately after the deploy confirms;
  - immediately call `admin()` and confirm it returns the intended address;
  - if it does not, abandon that contract ID and redeploy — the admin cannot
    be recovered, because `set_admin` and `upgrade` are gated by the
    (attacker's) admin.

  Once the fix is restored this item can be removed and SV-12 marked Resolved
  again.

- **Admin key compromise.** The admin can upgrade code and grant arbitrary
  leases. Use a hardware-backed or multisig account on mainnet and rotate with
  `set_admin` if needed.
- **Upgrades do not migrate data.** Storage layout changes require an explicit
  migration step (see README → Upgrades).
