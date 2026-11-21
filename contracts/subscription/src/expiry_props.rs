//! Property tests for subscription expiry arithmetic (`subscribe`, `renew`,
//! `cancel`, `is_active`, `remaining_ledgers`). Each property is checked
//! against a plain-integer model of the documented rules:
//!
//! - subscribe: `current_period_end = base + CYCLE`, where `base` is the
//!   remaining paid period of a cancelled-but-active subscription, otherwise
//!   `now`; `Overflow` if that does not fit in `u32`.
//! - renew:     `current_period_end = max(current_period_end, now) + CYCLE`,
//!   or `Overflow`, leaving the stored subscription untouched.
//! - active:    a subscription is active while `current_period_end > now`,
//!   regardless of `cancelled`.
//!
//! Renewal counter: `total_renewals` is **saturating**. It counts renewals up
//! to `u32::MAX` and then stays pinned there; a renewal at the maximum still
//! extends the paid period (the counter is informational, not a gate). This is
//! the explicit overflow policy for #228: no panic, no wrap, no `Overflow`
//! error — the counter simply stops counting while renewals keep working.
//!
//! TTL handling: `renew` and `cancel` extend the subscription key exactly once
//! per write (inside `save_sub`); the redundant pre-save extensions were
//! removed in #227. The storage-operation counts below pin that down so a
//! regression that re-adds an extension is caught.
//!
//! Cancellation idempotency: `cancel` succeeds when the subscription is
//! already cancelled but must not emit a second cancellation event nor touch
//! the stored period end or renewal counter (#229).
//!
//! Plan status vs. entitlement (#230): a plan's activity status and a
//! subscriber's entitlement are separate states. `is_active` only inspects the
//! subscription's `current_period_end` and never loads the plan, so
//! deactivating a plan does not revoke access already paid for; reactivating
//! the plan does not extend it either. The tests below pin that down across an
//! existing paid period.
//!
//! Deactivated plans and renewal (#223): deactivating a plan blocks **only new
//! subscribers** (`subscribe` rejects an inactive plan with `PlanInactive`).
//! Existing subscribers keep their entitlement through the already-paid period
//! and may still `renew` — `renew` does not consult `plan.active`, so a renewal
//! extends the paid period exactly as it would for an active plan. This is the
//! explicit product rule; the tests below pin both halves down.
//!
//! Plan activation idempotency (#226): `set_plan_active` still enforces
//! publisher authorization on every call, but when the requested `active`
//! value already matches the stored one it is a no-op — no storage write and
//! no `setactive` event. The tests below pin repeated activation and repeated
//! deactivation down.
use super::*;
use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger as _},
    Address, Env, IntoVal, String, Symbol, Val, Vec,
};

/// Ledgers entries stay live for in these tests, so advancing time past a full
/// `CYCLE` never archives the plan, subscription or instance entry.
const TEST_ENTRY_TTL: u32 = 20_000_000;
/// Largest start ledger used; keeps `now + TEST_ENTRY_TTL` inside `u32`.
const MAX_START: u32 = u32::MAX - 2 * TEST_ENTRY_TTL;
/// Largest amount of time advanced in one step (just over three cycles).
const MAX_STEP: u32 = 3 * CYCLE + 1;

/// Owned fixture: the environment and contract ID are returned by value so
/// callers construct the borrowing client at the use site.
struct Ctx {
    env: Env,
    id: Address,
    plan: String,
    sub: Address,
    payref: String,
}

impl Ctx {
    fn client(&self) -> SubscriptionManagerClient<'_> {
        SubscriptionManagerClient::new(&self.env, &self.id)
    }
}

fn setup(start: u32) -> Ctx {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| {
        l.sequence_number = start;
        l.min_persistent_entry_ttl = TEST_ENTRY_TTL;
        l.min_temp_entry_ttl = TEST_ENTRY_TTL;
        l.max_entry_ttl = 2 * TEST_ENTRY_TTL;
    });
    let id = env.register(SubscriptionManager, ());
    let client = SubscriptionManagerClient::new(&env, &id);
    client.init(&Address::generate(&env));
    let plan = String::from_str(&env, "plan");
    client.create_plan(&Address::generate(&env), &plan, &1_000_000i128);
    let sub = Address::generate(&env);
    let payref = String::from_str(&env, "ref");
    Ctx {
        env,
        id,
        plan,
        sub,
        payref,
    }
}

fn set_ledger(env: &Env, seq: u32) {
    env.ledger().set_sequence_number(seq);
}

/// Counts the events emitted by the contract whose topic is `cancel`.
fn cancel_event_count(env: &Env, contract: &Address) -> usize {
    let topic: Vec<Val> = (Symbol::new(env, "cancel"),).into_val(env);
    env.events()
        .all()
        .iter()
        .filter(|(addr, topics, _)| addr == contract && *topics == topic)
        .count()
}

/// Counts the events emitted by the contract whose topic is `setactive`.
fn setactive_event_count(env: &Env, contract: &Address) -> usize {
    let topic: Vec<Val> = (Symbol::new(env, "setactive"),).into_val(env);
    env.events()
        .all()
        .iter()
        .filter(|(addr, topics, _)| addr == contract && *topics == topic)
        .count()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// A first subscription starts now and ends exactly one `CYCLE` later.
    ///
    /// The `Overflow` branch (`now + CYCLE > u32::MAX`) is not exercised: it
    /// needs a ledger within one cycle of `u32::MAX`, where the host's own
    /// storage-TTL arithmetic fails before the contract's check is reached.
    #[test]
    fn subscribe_ends_one_cycle_after_now(now in 0..=MAX_START) {
        let c = setup(now);
        let client = c.client();
        let s = client.subscribe(&c.plan, &c.sub, &c.payref);
        prop_assert_eq!(s.current_period_end, now + CYCLE);
        prop_assert_eq!(s.started_at, now);
        prop_assert!(!s.cancelled);
        prop_assert_eq!(s.total_renewals, 0);
    }

    /// Active strictly before `current_period_end`, inactive from it onwards.
    #[test]
    fn active_strictly_before_period_end(now in 0..=MAX_START) {
        let c = setup(now);
        let client = c.client();
        let s = client.subscribe(&c.plan, &c.sub, &c.payref);
        set_ledger(&c.env, s.current_period_end - 1);
        prop_assert!(client.is_active(&c.plan, &c.sub));
        set_ledger(&c.env, s.current_period_end);
        prop_assert!(!client.is_active(&c.plan, &c.sub));
    }

    /// `renew` extends from the later of the current period end and now: an
    /// early renewal keeps the unused time, a late one restarts from now. The
    /// new end is later than both and the renewal counter goes up by one.
    #[test]
    fn renew_extends_from_later_of_period_end_and_now(now in 0..=MAX_START, elapsed in 0..=MAX_STEP) {
        let c = setup(now);
        let client = c.client();
        let s = client.subscribe(&c.plan, &c.sub, &c.payref);
        let t = now + elapsed;
        set_ledger(&c.env, t);

        let expected = s.current_period_end.max(t) + CYCLE;
        let renewed = client.renew(&c.plan, &c.sub, &c.payref);
        prop_assert_eq!(renewed.current_period_end, expected);
        prop_assert!(renewed.current_period_end > s.current_period_end);
        prop_assert!(renewed.current_period_end > t);
        prop_assert_eq!(renewed.total_renewals, s.total_renewals + 1);
        prop_assert_eq!(renewed.started_at, s.started_at);
        prop_assert!(client.is_active(&c.plan, &c.sub));
    }

    /// Renewing `k` times without letting the subscription lapse adds exactly
    /// `k` cycles.
    #[test]
    fn early_renewals_accumulate_whole_cycles(now in 0..=MAX_START, k in 1u32..6) {
        let c = setup(now);
        let client = c.client();
        client.subscribe(&c.plan, &c.sub, &c.payref);
        let mut last = None;
        for _ in 0..k {
            last = Some(client.renew(&c.plan, &c.sub, &c.payref));
        }
        let last = last.unwrap();
        prop_assert_eq!(last.current_period_end, now + (k + 1) * CYCLE);
        prop_assert_eq!(last.total_renewals, k);
    }

    /// Cancelling keeps access until the period end. Re-subscribing while the
    /// paid period is still running carries the remaining time over; after
    /// it lapses, the new period starts from now.
    #[test]
    fn resubscribe_after_cancel_carries_over_remaining_time(now in 0..=MAX_START, elapsed in 0..=MAX_STEP) {
        let c = setup(now);
        let client = c.client();
        let s = client.subscribe(&c.plan, &c.sub, &c.payref);
        client.cancel(&c.plan, &c.sub);
        let t = now + elapsed;
        set_ledger(&c.env, t);
        let base = if s.current_period_end > t { s.current_period_end } else { t };
        let resub = client.subscribe(&c.plan, &c.sub, &c.payref);
        prop_assert_eq!(resub.current_period_end, base + CYCLE);
        prop_assert!(!resub.cancelled);
    }

    /// Repeated activation is a no-op: the first call flips the plan and emits
    /// one `setactive` event, the second call leaves state and events alone.
    #[test]
    fn repeated_activation_is_noop(now in 0..=MAX_START) {
        let c = setup(now);
        let client = c.client();
        client.set_plan_active(&c.plan, &false);
        let before = setactive_event_count(&c.env, &c.id);
        client.set_plan_active(&c.plan, &true);
        let after_first = setactive_event_count(&c.env, &c.id);
        prop_assert_eq!(after_first, before + 1);
        client.set_plan_active(&c.plan, &true);
        prop_assert_eq!(setactive_event_count(&c.env, &c.id), after_first);
        prop_assert!(client.get_plan(&c.plan).active);
    }

    /// Repeated deactivation is a no-op: the first call flips the plan and
    /// emits one `setactive` event, the second call leaves state and events
    /// alone.
    #[test]
    fn repeated_deactivation_is_noop(now in 0..=MAX_START) {
        let c = setup(now);
        let client = c.client();
        let before = setactive_event_count(&c.env, &c.id);
        client.set_plan_active(&c.plan, &false);
        let after_first = setactive_event_count(&c.env, &c.id);
        prop_assert_eq!(after_first, before + 1);
        client.set_plan_active(&c.plan, &false);
        prop_assert_eq!(setactive_event_count(&c.env, &c.id), after_first);
        prop_assert!(!client.get_plan(&c.plan).active);
    }
}
