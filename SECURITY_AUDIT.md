# Security Audit: Access-Lease Time-Lock Logic

**Scope:** Access-lease issuance, expiry arithmetic, revocation, and the upgrade entrypoint.
**Status:** Draft
**Auditor:** Senior Security Review

---

## 1. Summary

This document records a security review of the access-lease time-lock logic. The review covers four areas:

1. Integer overflow in `expires_at` arithmetic.
2. Unauthorized revocation of active leases.
3. Lease-expiry edge cases (boundary conditions, clock skew, re-entrancy).
4. The newly introduced upgrade entrypoint.

Each section lists findings, severity, and mitigations.

---

## 2. Findings

### 2.1 Overflow in `expires_at` Arithmetic

**Severity:** High

**Description:**
Lease expiry is computed as `expires_at = now + duration`. If `duration` is attacker-controlled or unbounded, the addition can overflow the underlying integer type. On wrap-around, `expires_at` may become a small value in the past, causing the lease to be treated as already expired, or a value that bypasses expiry checks entirely.

**Impact:**
- Premature expiry: denial of service for legitimate lease holders.
- Wrapped expiry: leases that never expire, granting indefinite access.

**Mitigation:**
- Use checked arithmetic (`checked_add`) and reject on overflow.
- Enforce an upper bound (`MAX_LEASE_DURATION`) on `duration`.
- Validate that `expires_at > now` after computation.
- Prefer a wide integer type (e.g., `u64`/`i64`) with explicit range checks.

---

### 2.2 Unauthorized Revocation

**Severity:** High

**Description:**
The revocation path does not consistently verify that the caller is the lease owner or an authorized administrator. Any caller able to reach the entrypoint may revoke another party's lease.

**Impact:**
- Denial of service against other lease holders.
- Disruption of dependent workflows relying on active leases.

**Mitigation:**
- Require authentication and authorization checks before revocation.
- Verify caller identity against the lease owner or an explicit admin role.
- Emit an audit event on every revocation attempt (success and failure).
- Consider a two-step revocation for high-value leases.

---

### 2.3 Lease-Expiry Edge Cases

**Severity:** Medium

**Description:**
Several boundary conditions are not handled:

- **Exact-boundary expiry:** behavior when `now == expires_at` is ambiguous (inclusive vs. exclusive).
- **Clock skew / non-monotonic time:** wall-clock adjustments can cause leases to expire early or late.
- **Zero or negative duration:** a `duration` of `0` or negative may create an immediately-expired or never-expired lease.
- **Re-entrancy:** expiry checks that call back into lease state may be re-entered during revocation or renewal.
- **Concurrent renewal/expiry:** a race between renewal and expiry can leave inconsistent state.

**Impact:**
- Inconsistent access decisions.
- Potential bypass of expiry under race conditions.

**Mitigation:**
- Define and document boundary semantics (recommend `now >= expires_at` means expired).
- Use a monotonic clock source where available.
- Reject `duration <= 0` and enforce a minimum.
- Guard state transitions with a lock or atomic compare-and-swap.
- Ensure expiry checks are idempotent and free of external calls (no re-entrancy).

---

### 2.4 Upgrade Entrypoint

**Severity:** High

**Description:**
The new upgrade entrypoint changes lease-related state or logic. Without access control and validation, it can be invoked by unauthorized parties to alter lease behavior, migrate state incorrectly, or bypass existing invariants.

**Impact:**
- Privilege escalation via unauthorized upgrade.
- Corrupted or inconsistent lease state after migration.
- Loss of active leases or indefinite extension of expired ones.

**Mitigation:**
- Restrict the entrypoint to a trusted admin/owner role.
- Make the operation idempotent and one-shot where possible.
- Validate pre- and post-conditions on migrated state (e.g., `expires_at` bounds).
- Emit audit events and require explicit confirmation for destructive changes.
- Provide a rollback or pause mechanism.

---

## 3. Mitigation Checklist

| Area | Mitigation | Status |
|------|------------|--------|
| Overflow | Checked arithmetic + `MAX_LEASE_DURATION` | Required |
| Overflow | Post-computation `expires_at > now` check | Required |
| Revocation | Caller authorization check | Required |
| Revocation | Audit logging | Required |
| Expiry | Documented boundary semantics | Required |
| Expiry | Monotonic clock | Recommended |
| Expiry | Reject `duration <= 0` | Required |
| Expiry | Atomic state transitions | Required |
| Upgrade | Admin-only access control | Required |
| Upgrade | State validation + idempotency | Required |
| Upgrade | Audit logging + rollback | Recommended |

---

## 4. Residual Risk

After the mitigations above, residual risk is limited to:

- Clock-source limitations on platforms without a monotonic clock.
- Administrative key compromise, which is out of scope for this review.

---

## 5. References

- Access-lease issuance and expiry logic.
- Revocation entrypoint.
- Upgrade entrypoint.
- Related audit logging and authorization modules.