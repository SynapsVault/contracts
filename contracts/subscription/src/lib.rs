#![no_std]
//! SynapsVault — Subscription Manager (Soroban / Stellar)
//!
//! Manages recurring 30-day subscription plans for SynapsVault publishers.
//! Publishers create plans; the backend subscribes buyers after payment confirmation
//! and renews each billing cycle. Subscribers can self-cancel with access until period end.

use soroban_sdk::{contract, contracterror, contractimpl, contracttype, Address, BytesN, Env, String};

const DAY:         u32 = 17_280;           // ~5s/ledger
const CYCLE:       u32 = 30 * DAY;         // 30-day billing cycle
const BUMP:        u32 = 365 * DAY;        // 1-year TTL
const BUMP_THRESH: u32 = BUMP - DAY;

/// A recurring subscription plan offered by a publisher.
///
/// Plans are keyed by `plan_id` and can be deactivated by their publisher to
/// prevent new subscriptions. Existing subscribers are unaffected by
/// deactivation and continue to renew until they cancel.
/// Contract version, sourced from Cargo.toml at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// Unique, publisher-chosen identifier for the plan.
    pub plan_id:         String,
    /// Wallet that created the plan and is the only party allowed to deactivate it.
    pub publisher:       Address,
    /// Price charged per 30-day billing cycle, denominated in USDC stroops
    /// (7 decimal places). Must be strictly positive.
    pub price_per_cycle: i128,  // USDC stroops (7 decimal places)
    /// Whether new subscriptions are currently allowed. Set to `false` by
    /// [`SubscriptionManager::deactivate_plan`].
    pub active:          bool,
}

/// A subscriber's enrolment in a [`Plan`].
///
/// Access is granted while `!cancelled && current_period_end > ledger.sequence()`.
/// Each billing cycle is exactly 30 days (`CYCLE` ledgers). Cancellation is
/// non-destructive: it only flips `cancelled`, leaving access intact until the
/// end of the already-paid period.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Subscription {
    /// Identifier of the plan this subscription belongs to.
    pub plan_id:             String,
    /// Wallet receiving access to the plan's content.
    pub subscriber:          Address,
    /// Ledger sequence at which the subscription was first created.
    pub started_at:          u32,
    /// Ledger sequence at which the current paid period expires. Renewals
    /// extend this by one `CYCLE` (30 days).
    pub current_period_end:  u32,
    /// Set to `true` by [`SubscriptionManager::cancel`]. Cancelled
    /// subscriptions cannot be renewed and are not considered active.
    pub cancelled:           bool,
    /// Number of successful renewals applied to this subscription.
    pub total_renewals:      u32,
}

/// Storage keys used by the contract.
///
/// `Admin` lives in instance storage; `Plan` and `Sub` entries live in
/// persistent storage and are TTL-bumped on every write.
#[contracttype]
pub enum DataKey {
    /// Address authorised to call [`SubscriptionManager::subscribe`] and
    /// [`SubscriptionManager::renew`]. Set once by [`SubscriptionManager::init`].
    Admin,
    /// A [`Plan`] keyed by its `plan_id`.
    Plan(String),
    /// A [`Subscription`] keyed by `(plan_id, subscriber)`.
    Sub(String, Address),
    Version,
}

/// Error codes returned by the subscription manager.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    /// Caller failed the admin `require_auth` check.
    NotAdmin       = 1,
    /// No plan exists for the supplied `plan_id`.
    PlanNotFound   = 2,
    /// The plan exists but has been deactivated; new subscriptions are rejected.
    PlanInactive   = 3,
    /// The subscriber already has an active, non-expired subscription to this plan.
    AlreadySubbed  = 4,
    /// No subscription exists for the supplied `(plan_id, subscriber)` pair.
    SubNotFound    = 5,
    /// The subscription has been cancelled and can no longer be renewed.
    Cancelled      = 6,
    /// `price_per_cycle` was not strictly positive.
    InvalidPrice   = 7,
    /// `init` has not been called, so no admin is configured.
    NotInitialised = 8,
}

#[contract]
pub struct SubscriptionManager;

#[contractimpl]
impl SubscriptionManager {
    /// Initialise the contract by setting the admin wallet.
    ///
    /// Must be called exactly once after deployment. The admin is the only
    /// address permitted to call [`Self::subscribe`] and [`Self::renew`].
    ///
    /// # Parameters
    /// - `admin`: wallet that will act as the backend operator.
    pub fn init(env: Env, admin: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Version, &String::from_str(&env, VERSION));
        env.storage().instance().extend_ttl(BUMP_THRESH, BUMP);
    }

    /// Create a new subscription plan.
    ///
    /// Requires authorisation from `publisher`. The plan becomes immediately
    /// active and can be subscribed to by the admin.
    ///
    /// # Parameters
    /// - `publisher`: wallet owning the plan; must sign the transaction.
    /// - `plan_id`: unique identifier for the plan.
    /// - `price_per_cycle`: price per 30-day cycle in USDC stroops; must be `> 0`.
    ///
    /// # Errors
    /// - [`Error::InvalidPrice`] if `price_per_cycle <= 0`.
    /// Return the contract version string. Permissionless.
    pub fn get_version(env: Env) -> String {
        env.storage()
            .instance()
            .get(&DataKey::Version)
            .unwrap_or_else(|| String::from_str(&env, VERSION))
    }

    /// Returns `true` if the stored version matches the compiled version.
    /// Useful for detecting a contract that needs migration after an upgrade.
    pub fn is_compatible(env: Env) -> bool {
        let stored: Option<String> = env.storage().instance().get(&DataKey::Version);
        match stored {
            Some(v) => v == String::from_str(&env, VERSION),
            None    => false,
        }
    }

    /// Publisher creates a subscription plan. `price_per_cycle` is in USDC stroops.
    pub fn create_plan(
        env: Env,
        publisher: Address,
        plan_id: String,
        price_per_cycle: i128,
    ) -> Result<Plan, Error> {
        publisher.require_auth();
        if price_per_cycle <= 0 {
            return Err(Error::InvalidPrice);
        }
        let plan = Plan {
            plan_id: plan_id.clone(),
            publisher,
            price_per_cycle,
            active: true,
        };
        let key = DataKey::Plan(plan_id);
        env.storage().persistent().set(&key, &plan);
        env.storage().persistent().extend_ttl(&key, BUMP_THRESH, BUMP);
        Ok(plan)
    }

    /// Deactivate a plan so no new subscribers can join.
    ///
    /// Requires authorisation from the plan's publisher. Existing subscriptions
    /// remain valid and can still be renewed; only new subscriptions are blocked.
    ///
    /// # Errors
    /// - [`Error::PlanNotFound`] if no plan exists for `plan_id`.
    pub fn deactivate_plan(env: Env, plan_id: String) -> Result<(), Error> {
        let key = DataKey::Plan(plan_id);
        let mut plan: Plan = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::PlanNotFound)?;
        plan.publisher.require_auth();
        plan.active = false;
        env.storage().persistent().set(&key, &plan);
        Ok(())
    }

    /// Subscribe a buyer to a plan after off-chain payment confirmation.
    ///
    /// Admin-only. Creates a fresh subscription with a 30-day period starting
    /// at the current ledger sequence. If a previous subscription exists but is
    /// cancelled or expired, it is overwritten.
    ///
    /// # Parameters
    /// - `plan_id`: plan to subscribe to; must exist and be active.
    /// - `subscriber`: wallet receiving access.
    ///
    /// # Errors
    /// - [`Error::NotAdmin`] if the caller is not the configured admin.
    /// - [`Error::PlanNotFound`] if the plan does not exist.
    /// - [`Error::PlanInactive`] if the plan has been deactivated.
    /// - [`Error::AlreadySubbed`] if an active, non-expired subscription exists.
    pub fn subscribe(
        env: Env,
        plan_id: String,
        subscriber: Address,
    ) -> Result<Subscription, Error> {
        Self::require_admin(&env)?;

        let plan: Plan = env
            .storage()
            .persistent()
            .get(&DataKey::Plan(plan_id.clone()))
            .ok_or(Error::PlanNotFound)?;

        if !plan.active {
            return Err(Error::PlanInactive);
        }

        let key = DataKey::Sub(plan_id.clone(), subscriber.clone());

        // Block double-subscribe if still active.
        if let Some(s) = env.storage().persistent().get::<_, Subscription>(&key) {
            if !s.cancelled && s.current_period_end > env.ledger().sequence() {
                return Err(Error::AlreadySubbed);
            }
        }

        let now = env.ledger().sequence();
        let sub = Subscription {
            plan_id,
            subscriber,
            started_at:          now,
            current_period_end:  now + CYCLE,
            cancelled:           false,
            total_renewals:      0,
        };
        env.storage().persistent().set(&key, &sub);
        env.storage().persistent().extend_ttl(&key, BUMP_THRESH, BUMP);
        Ok(sub)
    }

    /// Renew a subscription by one 30-day billing cycle after payment.
    ///
    /// Admin-only. The new period end is computed as
    /// `max(current_period_end, now) + CYCLE`, so renewals stack from the end
    /// of the current period when it is still in the future, or from `now`
    /// when the subscription has already lapsed.
    ///
    /// # Errors
    /// - [`Error::NotAdmin`] if the caller is not the configured admin.
    /// - [`Error::SubNotFound`] if no subscription exists for the pair.
    /// - [`Error::Cancelled`] if the subscription has been cancelled.
    pub fn renew(
        env: Env,
        plan_id: String,
        subscriber: Address,
    ) -> Result<Subscription, Error> {
        Self::require_admin(&env)?;
        let key = DataKey::Sub(plan_id.clone(), subscriber.clone());
        let mut sub: Subscription = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::SubNotFound)?;

        if sub.cancelled {
            return Err(Error::Cancelled);
        }

        // Extend from end of current period, or from now if expired.
        let base = sub.current_period_end.max(env.ledger().sequence());
        sub.current_period_end = base + CYCLE;
        sub.total_renewals += 1;

        env.storage().persistent().set(&key, &sub);
        env.storage().persistent().extend_ttl(&key, BUMP_THRESH, BUMP);
        Ok(sub)
    }

    /// Cancel the caller's own subscription.
    ///
    /// Requires authorisation from `subscriber`. Cancellation is idempotent in
    /// effect: access continues until `current_period_end`, after which the
    /// subscription is no longer active and cannot be renewed.
    ///
    /// # Errors
    /// - [`Error::SubNotFound`] if no subscription exists for the pair.
    pub fn cancel(env: Env, plan_id: String, subscriber: Address) -> Result<(), Error> {
        subscriber.require_auth();
        let key = DataKey::Sub(plan_id, subscriber);
        let mut sub: Subscription = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::SubNotFound)?;
        sub.cancelled = true;
        env.storage().persistent().set(&key, &sub);
        Ok(())
    }

    /// Return `true` if the subscriber currently has active access.
    ///
    /// Access is active when the subscription exists, is not cancelled, and
    /// `current_period_end` is strictly greater than the current ledger
    /// sequence. Permissionless — safe to call from any context.
    pub fn is_active(env: Env, plan_id: String, subscriber: Address) -> bool {
        let key = DataKey::Sub(plan_id, subscriber);
        match env.storage().persistent().get::<_, Subscription>(&key) {
            Some(s) => !s.cancelled && s.current_period_end > env.ledger().sequence(),
            None    => false,
        }
    }

    /// Return the full [`Subscription`] record for a `(plan_id, subscriber)` pair.
    ///
    /// Permissionless. Returns the stored record regardless of whether it is
    /// cancelled or expired.
    ///
    /// # Errors
    /// - [`Error::SubNotFound`] if no subscription exists for the pair.
    pub fn get_subscription(
        env: Env,
        plan_id: String,
        subscriber: Address,
    ) -> Result<Subscription, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Sub(plan_id, subscriber))
            .ok_or(Error::SubNotFound)
    }

    /// Admin upgrades the contract WASM. Admin only.
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.deployer().update_current_contract_wasm(new_wasm_hash);
        Ok(())
    }

    // ─── Internal helpers ────────────────────────────────────────────────────

    fn require_admin(env: &Env) -> Result<(), Error> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialised)?;
        admin.require_auth();
        Ok(())
    }
}






#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Address, Env, String};

    #[test]
    fn test_create_plan() {
        let env = Env::default();
        env.mock_all_auths();

        let id = env.register(SubscriptionManager, ());
        let client = SubscriptionManagerClient::new(&env, &id);

        let admin = Address::generate(&env);
        let publisher = Address::generate(&env);
        let plan_id = String::from_str(&env, "plan_001");

        client.init(&admin);

        let plan = client.create_plan(&publisher, &plan_id, &10_000_000i128);
        assert_eq!(plan.price_per_cycle, 10_000_000i128);
        assert!(plan.active);
    }

    #[test]
    fn test_subscribe_and_active() {
        let env = Env::default();
        env.mock_all_auths();

        let id = env.register(SubscriptionManager, ());
        let client = SubscriptionManagerClient::new(&env, &id);

        let admin = Address::generate(&env);
        let publisher = Address::generate(&env);
        let subscriber = Address::generate(&env);
        let plan_id = String::from_str(&env, "plan_002");

        client.init(&admin);
        client.create_plan(&publisher, &plan_id, &1_000_000i128);

        assert!(!client.is_active(&plan_id, &subscriber));
        client.subscribe(&plan_id, &subscriber);
        assert!(client.is_active(&plan_id, &subscriber));
    }
}