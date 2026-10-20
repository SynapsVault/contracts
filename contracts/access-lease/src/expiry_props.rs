//! Property tests for lease expiry arithmetic (`grant_lease`, `extend_lease`,
//! `is_valid`). Each property is checked against a plain-integer model of the
//! documented rules instead of hand-picked examples:
//!
//! - grant:  `expires_at = now + duration`, or `InvalidDuration` on overflow
//!   or when `duration` exceeds `MAX_LEASE_DURATION`.
//! - extend: `expires_at = max(expires_at, now) + extra`, or `InvalidDuration`
//!   on overflow or when `extra` exceeds `MAX_LEASE_DURATION`, leaving the
//!   stored lease untouched. `duration_ledgers` is the originally requested
//!   duration and is never rewritten by an extension.
//! - valid:  a lease is valid while `expires_at > ledger.sequence()`.
//! - ttl:    the persistent entry TTL always covers the full lease lifetime
//!   (plus a safety margin), so a valid lease never archives before
//!   `expires_at`.
use super::*;
use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    Address, Env, String,
};

/// Ledgers entries stay live for in these tests, so advancing time never
/// archives the lease, plan or instance entry under test.
const TEST_ENTRY_TTL: u32 = 20_000_000;
/// Largest start ledger used; keeps `now + TEST_ENTRY_TTL` inside `u32`.
const MAX_START: u32 = u32::MAX - 2 * TEST_ENTRY_TTL;
/// Largest amount of time advanced in one step.
const MAX_STEP: u32 = 10_000_000;

fn setup<'a>(start: u32) -> (Env, AccessLeaseClient<'a>, String, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| {
        l.sequence_number = start;
        l.min_persistent_entry_ttl = TEST_ENTRY_TTL;
        l.min_temp_entry_ttl = TEST_ENTRY_TTL;
        l.max_entry_ttl = 2 * TEST_ENTRY_TTL;
    });
    let id = env.register(AccessLease, ());
    let client = AccessLeaseClient::new(&env, &id);
    client.init(&Address::generate(&env));
    let rid = String::from_str(&env, "res");
    let buyer = Address::generate(&env);
    (env, client, rid, buyer)
}

fn set_ledger(env: &Env, seq: u32) {
    env.ledger().set_sequence_number(seq);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// `grant_lease` sets `expires_at = now + duration` exactly, or rejects an
    /// overflowing or over-cap duration without storing anything.
    #[test]
    fn grant_sets_expiry_or_rejects_overflow(now in 0..=MAX_START, duration in 1..=u32::MAX) {
        let (_env, client, rid, buyer) = setup(now);
        match now.checked_add(duration) {
            Some(expected) if duration <= MAX_LEASE_DURATION => {
                let lease = client.grant_lease(&rid, &buyer, &duration);
                prop_assert_eq!(lease.expires_at, expected);
                prop_assert_eq!(lease.granted_at, now);
                prop_assert_eq!(lease.duration_ledgers, duration);
                prop_assert!(client.is_valid(&rid, &buyer));
            }
            _ => {
                prop_assert_eq!(
                    client.try_grant_lease(&rid, &buyer, &duration),
                    Err(Ok(Error::InvalidDuration))
                );
                prop_assert!(client.try_get_lease(&rid, &buyer).is_err());
                prop_assert!(!client.is_valid(&rid, &buyer));
            }
        }
    }

    /// A lease is valid on every ledger before `expires_at` and invalid from
    /// `expires_at` onwards.
    #[test]
    fn lease_valid_strictly_before_expiry(now in 0..=MAX_START, duration in 1..=MAX_STEP) {
        let (env, client, rid, buyer) = setup(now);
        let lease = client.grant_lease(&rid, &buyer, &duration);

        set_ledger(&env, lease.expires_at - 1);
        prop_assert!(client.is_valid(&rid, &buyer));
        set_ledger(&env, lease.expires_at);
        prop_assert!(!client.is_valid(&rid, &buyer));
        set_ledger(&env, lease.expires_at.saturating_add(1));
        prop_assert!(!client.is_valid(&rid, &buyer));
    }

    /// The persistent TTL of a granted lease always covers the full lease
    /// lifetime plus a safety margin, so a valid lease never archives before
    /// `expires_at`.
    #[test]
    fn grant_ttl_covers_full_lease(now in 0..=MAX_START, duration in 1..=MAX_STEP) {
        let (env, client, rid, buyer) = setup(now);
        let lease = client.grant_lease(&rid, &buyer, &duration);
        let remaining = lease.expires_at.saturating_sub(now);
        let ttl = env.storage().persistent().get_ttl(&DataKey::Lease(rid.clone(), buyer.clone()));
        prop_assert!(ttl >= remaining, "ttl {} shorter than lease remaining {}", ttl, remaining);
    }

    /// `extend_lease` adds to `max(expires_at, now)`: an active lease keeps its
    /// remaining time, an expired one restarts from now. The result is always
    /// later than both the old expiry and now. Overflow or an over-cap `extra`
    /// is rejected and leaves the stored lease unchanged. `duration_ledgers`
    /// records the originally requested duration and is never rewritten.
    #[test]
    fn extend_adds_to_later_of_expiry_and_now(
        now in 0..=MAX_START,
        duration in 1..=MAX_STEP,
        elapsed in 0..=MAX_STEP,
        extra in 1..=u32::MAX,
    ) {
        let (env, client, rid, buyer) = setup(now);
        let lease = client.grant_lease(&rid, &buyer, &duration);
        let t = now + elapsed;
        set_ledger(&env, t);

        match lease.expires_at.max(t).checked_add(extra) {
            Some(expected) if extra <= MAX_LEASE_DURATION => {
                let extended = client.extend_lease(&rid, &buyer, &extra);
                prop_assert_eq!(extended.expires_at, expected);
                prop_assert!(extended.expires_at > lease.expires_at);
                prop_assert!(extended.expires_at > t);
                prop_assert!(client.is_valid(&rid, &buyer));
                // Extending never rewrites when the lease was granted.
                prop_assert_eq!(extended.granted_at, lease.granted_at);
                // `duration_ledgers` is the originally requested duration and
                // stays immutable across extensions.
                prop_assert_eq!(extended.duration_ledgers, lease.duration_ledgers);
            }
            _ => {
                prop_assert_eq!(
                    client.try_extend_lease(&rid, &buyer, &extra),
                    Err(Ok(Error::InvalidDuration))
                );
                prop_assert_eq!(client.get_lease(&rid, &buyer), lease);
            }
        }
    }

    /// Repeated extensions of an active lease accumulate exactly.
    #[test]
    fn consecutive_extensions_accumulate(
        now in 0..=MAX_START,
        duration in 1..=MAX_STEP,
        extras in proptest::collection::vec(1u32..=1_000_000, 1..6),
    ) {
        let (_env, client, rid, buyer) = setup(now);
        let mut expected = client.grant_lease(&rid, &buyer, &duration).expires_at;
        for extra in extras {
            expected += extra;
            prop_assert_eq!(client.extend_lease(&rid, &buyer, &extra).expires_at, expected);
        }
    }

    /// Re-granting is refused while the lease is active and, once it has
    /// expired, starts a fresh lease from the current ledger.
    #[test]
    fn regrant_only_after_expiry_and_restarts_from_now(
        now in 0..=MAX_START,
        first in 1..=MAX_STEP,
        elapsed in 0..=MAX_STEP,
        second in 1..=MAX_STEP,
    ) {
        let (env, client, rid, buyer) = setup(now);
        let lease = client.grant_lease(&rid, &buyer, &first);
        let t = now + elapsed;
        set_ledger(&env, t);

        if lease.expires_at > t {
            prop_assert_eq!(
                client.try_grant_lease(&rid, &buyer, &second),
                Err(Ok(Error::AlreadyActive))
            );
        } else {
            let regranted = client.grant_lease(&rid, &buyer, &second);
            prop_assert_eq!(regranted.granted_at, t);
            prop_assert_eq!(regranted.expires_at, t + second);
        }
    }

    /// Zero-length grants and extensions are always rejected.
    #[test]
    fn zero_duration_always_rejected(now in 0..=MAX_START, duration in 1..=MAX_STEP) {
        let (_env, client, rid, buyer) = setup(now);
        prop_assert_eq!(client.try_grant_lease(&rid, &buyer, &0), Err(Ok(Error::InvalidDuration)));
        let lease = client.grant_lease(&rid, &buyer, &duration);
        prop_assert_eq!(client.try_extend_lease(&rid, &buyer, &0), Err(Ok(Error::InvalidDuration)));
        prop_assert_eq!(client.get_lease(&rid, &buyer), lease);
    }
}
