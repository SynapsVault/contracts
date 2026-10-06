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

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub plan_id:         String,
    pub publisher:       Address,
    pub price_per_cycle: i128,  // USDC stroops (7 decimal places)
    pub active:          bool,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Subscription {
    pub plan_id:             String,
    pub subscriber:          Address,
    pub started_at:          u32,
    pub current_period_end:  u32,
    pub cancelled:           bool,
    pub total_renewals:      u32,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Plan(String),
    Sub(String, Address),
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    NotAdmin       = 1,
    PlanNotFound   = 2,
    PlanInactive   = 3,
    AlreadySubbed  = 4,
    SubNotFound    = 5,
    Cancelled      = 6,
    InvalidPrice   = 7,
    NotInitialised = 8,
}

#[contract]
pub struct SubscriptionManager;

#[contractimpl]
impl SubscriptionManager {
    /// Initialise — set the admin wallet. Must be called once after deployment.
    pub fn init(env: Env, admin: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().extend_ttl(BUMP_THRESH, BUMP);
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

    /// Publisher deactivates a plan — no new subscribers allowed.
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

    /// Backend subscribes a buyer after payment is confirmed (admin only).
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

    /// Admin renews a subscription by one billing cycle after payment.
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

    /// Subscriber cancels their own subscription.
    /// Access continues until `current_period_end`.
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

    /// Returns `true` if subscriber has active, non-expired access. Permissionless.
    pub fn is_active(env: Env, plan_id: String, subscriber: Address) -> bool {
        let key = DataKey::Sub(plan_id, subscriber);
        match env.storage().persistent().get::<_, Subscription>(&key) {
            Some(s) => !s.cancelled && s.current_period_end > env.ledger().sequence(),
            None    => false,
        }
    }

    /// Return the full Subscription struct. Permissionless.
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