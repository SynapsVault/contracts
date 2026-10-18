#![no_std]
//! SynapsVault — Subscription Manager (Soroban / Stellar)
//!
//! Manages recurring 30-day subscription plans for SynapsVault publishers.
//! Publishers create plans; the backend subscribes buyers after payment confirmation
//! and renews each billing cycle. Subscribers can self-cancel with access until period end.

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, Address, BytesN,
    ContractExecutable, Env, String,
};

/// Ledgers per day (~5 s per ledger).
const DAY: u32 = 17_280;

/// Length of one billing cycle in ledgers (30 days).
pub const CYCLE: u32 = 30 * DAY;

/// Maximum length of a plan identifier in bytes.
pub const MAX_PLAN_ID_LEN: u32 = 64;

/// Shared persistent-storage TTL policy (see `docs/CONTRACTS.md`).
const BUMP: u32 = 365 * DAY; // 1-year TTL
const BUMP_THRESH: u32 = BUMP - DAY;

/// Contract version, sourced from Cargo.toml at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A recurring subscription plan offered by a publisher.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub plan_id: String,
    pub publisher: Address,
    pub price_per_cycle: i128,
    pub active: bool,
}

/// A subscriber's enrolment in a [`Plan`].
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Subscription {
    pub plan_id: String,
    pub subscriber: Address,
    pub started_at: u32,
    pub current_period_end: u32,
    pub cancelled: bool,
    pub total_renewals: u32,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Plan(String),
    Sub(String, Address),
    Version,
}

/// Subscription-manager error codes.
///
/// Numbering is **per-contract** and intentionally not aligned with the
/// `vault-registry` or `access-lease` enums: the same numeric value can mean
/// different things in different contracts (e.g. `AlreadyInitialised` is 9
/// here, 8 in the registry and 7 in the lease). The cross-contract mapping is
/// documented in `docs/CONTRACTS.md`; the discriminants are pinned by tests so
/// any renumbering is deliberate.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    // 1 was NotAdmin, removed as it is never returned (admin auth traps on failure)
    PlanNotFound = 2,
    PlanInactive = 3,
    AlreadySubbed = 4,
    SubNotFound = 5,
    Cancelled = 6,
    InvalidPrice = 7,
    NotInitialised = 8,
    AlreadyInitialised = 9,
    PlanExists = 10,
    Overflow = 11,
    /// `plan_id` is empty or exceeds the maximum allowed length.
    InvalidPlanId = 12,
    /// The new admin address is the same as the current admin address.
    SameAdmin = 13,
}

#[contractevent(topics = ["init"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitEvent {
    pub admin: Address,
}

#[contractevent(topics = ["setadmin"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetAdminEvent {
    pub new_admin: Address,
}

#[contractevent(topics = ["plan"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanCreatedEvent {
    #[topic]
    pub plan_id: String,
    #[topic]
    pub publisher: Address,
    pub price_per_cycle: i128,
}

#[contractevent(topics = ["setactive"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanActiveEvent {
    #[topic]
    pub plan_id: String,
    pub active: bool,
}

#[contractevent(topics = ["subscribe"], data_format = "vec")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscribedEvent {
    #[topic]
    pub plan_id: String,
    #[topic]
    pub subscriber: Address,
    pub current_period_end: u32,
    pub price: i128,
    pub payment_reference: String,
}

#[contractevent(topics = ["renew"], data_format = "vec")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenewedEvent {
    #[topic]
    pub plan_id: String,
    #[topic]
    pub subscriber: Address,
    pub current_period_end: u32,
    pub price: i128,
    pub payment_reference: String,
}

#[contractevent(topics = ["cancel"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancelledEvent {
    #[topic]
    pub plan_id: String,
    #[topic]
    pub subscriber: Address,
}

#[contractevent(topics = ["upgrade"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradeEvent {
    pub new_wasm_hash: BytesN<32>,
}

#[contract]
pub struct SubscriptionManager;

#[contractimpl]
impl SubscriptionManager {
    pub fn init(env: Env, admin: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialised);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Version, &String::from_str(&env, VERSION));
        env.storage().instance().extend_ttl(BUMP_THRESH, BUMP);
        InitEvent { admin }.publish(&env);
        Ok(())
    }

    pub fn admin(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialised)
    }

    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        Self::require_admin(&env)?;
        let current_admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if current_admin == new_admin {
            return Err(Error::SameAdmin);
        }
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        SetAdminEvent { new_admin }.publish(&env);
        Ok(())
    }

    pub fn get_version(env: Env) -> String {
        env.storage()
            .instance()
            .get(&DataKey::Version)
            .unwrap_or_else(|| String::from_str(&env, VERSION))
    }

    pub fn is_compatible(env: Env) -> bool {
        match env.storage().instance().get::<_, String>(&DataKey::Version) {
            Some(stored) => stored == String::from_str(&env, VERSION),
            None => false,
        }
    }

    pub fn create_plan(
        env: Env,
        publisher: Address,
        plan_id: String,
        price_per_cycle: i128,
    ) -> Result<Plan, Error> {
        publisher.require_auth();
        if plan_id.is_empty() || plan_id.len() > MAX_PLAN_ID_LEN {
            return Err(Error::InvalidPlanId);
        }
        if price_per_cycle <= 0 {
            return Err(Error::InvalidPrice);
        }
        let key = DataKey::Plan(plan_id.clone());
        if env.storage().persistent().has(&key) {
            return Err(Error::PlanExists);
        }
        let plan = Plan {
            plan_id: plan_id.clone(),
            publisher: publisher.clone(),
            price_per_cycle,
            active: true,
        };
        env.storage().persistent().set(&key, &plan);
        env.storage()
            .persistent()
            .extend_ttl(&key, BUMP_THRESH, BUMP);
        PlanCreatedEvent {
            plan_id,
            publisher,
            price_per_cycle,
        }
        .publish(&env);
        Ok(plan)
    }

    pub fn get_plan(env: Env, plan_id: String) -> Result<Plan, Error> {
        let key = DataKey::Plan(plan_id);
        let plan: Plan = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::PlanNotFound)?;
        env.storage()
            .persistent()
            .extend_ttl(&key, BUMP_THRESH, BUMP);
        Ok(plan)
    }

    pub fn set_plan_active(env: Env, plan_id: String, active: bool) -> Result<(), Error> {
        let key = DataKey::Plan(plan_id.clone());
        let mut plan: Plan = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::PlanNotFound)?;
        env.storage()
            .persistent()
            .extend_ttl(&key, BUMP_THRESH, BUMP);

        plan.publisher.require_auth();
        plan.active = active;
        env.storage().persistent().set(&key, &plan);
        env.storage()
            .persistent()
            .extend_ttl(&key, BUMP_THRESH, BUMP);

        PlanActiveEvent { plan_id, active }.publish(&env);
        Ok(())
    }

    pub fn subscribe(
        env: Env,
        plan_id: String,
        subscriber: Address,
        payment_reference: String,
    ) -> Result<Subscription, Error> {
        Self::require_admin(&env)?;

        let plan_key = DataKey::Plan(plan_id.clone());
        let plan: Plan = env
            .storage()
            .persistent()
            .get(&plan_key)
            .ok_or(Error::PlanNotFound)?;
        env.storage()
            .persistent()
            .extend_ttl(&plan_key, BUMP_THRESH, BUMP);

        if !plan.active {
            return Err(Error::PlanInactive);
        }

        let key = DataKey::Sub(plan_id.clone(), subscriber.clone());
        let now = env.ledger().sequence();
        let mut base = now;
        let mut started_at = now;
        let mut total_renewals = 0;

        if let Some(s) = env.storage().persistent().get::<_, Subscription>(&key) {
            started_at = s.started_at;
            total_renewals = s.total_renewals;

            if s.current_period_end > now {
                if !s.cancelled {
                    return Err(Error::AlreadySubbed);
                }
                base = s.current_period_end;
            }
        }

        let sub = Subscription {
            plan_id: plan_id.clone(),
            subscriber: subscriber.clone(),
            started_at,
            current_period_end: base.checked_add(CYCLE).ok_or(Error::Overflow)?,
            cancelled: false,
            total_renewals,
        };

        Self::save_sub(&env, &key, &sub);

        SubscribedEvent {
            plan_id,
            subscriber,
            current_period_end: sub.current_period_end,
            price: plan.price_per_cycle,
            payment_reference,
        }
        .publish(&env);

        Ok(sub)
    }

    pub fn renew(
        env: Env,
        plan_id: String,
        subscriber: Address,
        payment_reference: String,
    ) -> Result<Subscription, Error> {
        Self::require_admin(&env)?;

        let plan_key = DataKey::Plan(plan_id.clone());
        let plan: Plan = env
            .storage()
            .persistent()
            .get(&plan_key)
            .ok_or(Error::PlanNotFound)?;
        env.storage()
            .persistent()
            .extend_ttl(&plan_key, BUMP_THRESH, BUMP);

        let key = DataKey::Sub(plan_id.clone(), subscriber.clone());
        let mut sub: Subscription = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::SubNotFound)?;
        env.storage()
            .persistent()
            .extend_ttl(&key, BUMP_THRESH, BUMP);

        if sub.cancelled {
            return Err(Error::Cancelled);
        }

        let base = sub.current_period_end.max(env.ledger().sequence());
        sub.current_period_end = base.checked_add(CYCLE).ok_or(Error::Overflow)?;
        sub.total_renewals = sub.total_renewals.saturating_add(1);

        Self::save_sub(&env, &key, &sub);

        RenewedEvent {
            plan_id,
            subscriber,
            current_period_end: sub.current_period_end,
            price: plan.price_per_cycle,
            payment_reference,
        }
        .publish(&env);

        Ok(sub)
    }

    pub fn cancel(env: Env, plan_id: String, subscriber: Address) -> Result<(), Error> {
        subscriber.require_auth();
        let key = DataKey::Sub(plan_id.clone(), subscriber.clone());
        let mut sub: Subscription = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::SubNotFound)?;
        env.storage()
            .persistent()
            .extend_ttl(&key, BUMP_THRESH, BUMP);
        if sub.cancelled {
            return Ok(());
        }
        sub.cancelled = true;
        Self::save_sub(&env, &key, &sub);
        CancelledEvent {
            plan_id,
            subscriber,
        }
        .publish(&env);
        Ok(())
    }

    pub fn is_active(env: Env, plan_id: String, subscriber: Address) -> bool {
        let key = DataKey::Sub(plan_id, subscriber);
        match env.storage().persistent().get::<_, Subscription>(&key) {
            Some(s) => {
                env.storage()
                    .persistent()
                    .extend_ttl(&key, BUMP_THRESH, BUMP);
                s.current_period_end > env.ledger().sequence()
            }
            None => false,
        }
    }

    pub fn get_subscription(
        env: Env,
        plan_id: String,
        subscriber: Address,
    ) -> Result<Subscription, Error> {
        let key = DataKey::Sub(plan_id, subscriber);
        let sub: Subscription = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::SubNotFound)?;
        env.storage()
            .persistent()
            .extend_ttl(&key, BUMP_THRESH, BUMP);
        Ok(sub)
    }

    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.deployer()
            .update_current_contract(ContractExecutable::Wasm(new_wasm_hash.clone()));
        UpgradeEvent { new_wasm_hash }.publish(&env);
        Ok(())
    }

    pub fn migrate(env: Env) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.storage()
            .instance()
            .set(&DataKey::Version, &String::from_str(&env, VERSION));
        env.storage().instance().extend_ttl(BUMP_THRESH, BUMP);
        Ok(())
    }
}

impl SubscriptionManager {
    fn require_admin(env: &Env) -> Result<(), Error> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialised)?;
        common::require_admin(env, &admin);
        env.storage().instance().extend_ttl(BUMP_THRESH, BUMP);
        Ok(())
    }

    /// Persist a [`Subscription`] and bump its TTL in one call.
    ///
    /// Every mutating path that touches a subscription key (`subscribe`,
    /// `renew`, `cancel`) calls this helper instead of duplicating the
    /// `persistent().set` + `extend_ttl` pair inline (issue #122).
    fn save_sub(env: &Env, key: &DataKey, sub: &Subscription) {
        env.storage().persistent().set(key, sub);
        env.storage()
            .persistent()
            .extend_ttl(key, BUMP_THRESH, BUMP);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        symbol_short,
        testutils::{Address as _, Events as _, Ledger as _, MockAuth, MockAuthInvoke},
        vec, Address, BytesN, Env, IntoVal, String,
    };

    struct Ctx<'a> {
        env: Env,
        client: SubscriptionManagerClient<'a>,
        admin: Address,
        publisher: Address,
        plan_id: String,
    }

    /// Real WASM for success-path `upgrade` tests: the host rejects a hash
    /// that was never uploaded. Reuses the vault-registry upgrade fixture.
    const UPGRADE_TARGET_WASM: &[u8] =
        include_bytes!("../../vault-registry/fixtures/vault_registry_upgrade_target.wasm");

    fn uploaded_wasm_hash(env: &Env) -> BytesN<32> {
        env.deployer().upload_contract_wasm(UPGRADE_TARGET_WASM)
    }

    fn setup<'a>() -> Ctx<'a> {
        let env = Env::default();
        env.mock_all_auths();
        let id = env.register(SubscriptionManager, ());
        let client = SubscriptionManagerClient::new(&env, &id);
        let admin = Address::generate(&env);
        let publisher = Address::generate(&env);
        let plan_id = String::from_str(&env, "plan");
        client.init(&admin);
        client.create_plan(&publisher, &plan_id, &1_000_000i128);
        Ctx {
            env,
            client,
            admin,
            publisher,
            plan_id,
        }
    }

    fn advance(env: &Env, ledgers: u32) {
        env.ledger()
            .set_sequence_number(env.ledger().sequence() + ledgers);
    }

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
        assert_eq!(client.get_plan(&plan_id), plan);
    }

    #[test]
    fn test_subscribe_and_active() {
        let c = setup();
        let subscriber = Address::generate(&c.env);
        let payment_ref = String::from_str(&c.env, "txn123");

        assert!(!c.client.is_active(&c.plan_id, &subscriber));
        c.client.subscribe(&c.plan_id, &subscriber, &payment_ref);
        assert!(c.client.is_active(&c.plan_id, &subscriber));
    }

    #[test]
    fn set_admin_by_non_admin_is_rejected() {
        let c = setup();
        let mallory = Address::generate(&c.env);
        let new_admin = Address::generate(&c.env);
        c.env.mock_auths(&[MockAuth {
            address: &mallory,
            invoke: &MockAuthInvoke {
                contract: &c.client.address,
                fn_name: "set_admin",
                args: (new_admin.clone(),).into_val(&c.env),
                sub_invokes: &[],
            },
        }]);
        assert!(c.client.try_set_admin(&new_admin).is_err());
    }

    #[test]
    fn set_admin_to_same_admin_fails() {
        let c = setup();
        assert_eq!(c.client.try_set_admin(&c.admin), Err(Ok(Error::SameAdmin)));
    }

    #[test]
    fn upgrade_success() {
        let c = setup();
        let hash = uploaded_wasm_hash(&c.env);
        c.client.upgrade(&hash);
        // Validating the call succeeded through auth checks
    }

    #[test]
    fn upgrade_by_non_admin_is_rejected() {
        let c = setup();
        let mallory = Address::generate(&c.env);
        let hash = BytesN::from_array(&c.env, &[1; 32]);
        c.env.mock_auths(&[MockAuth {
            address: &mallory,
            invoke: &MockAuthInvoke {
                contract: &c.client.address,
                fn_name: "upgrade",
                args: (hash.clone(),).into_val(&c.env),
                sub_invokes: &[],
            },
        }]);
        assert!(c.client.try_upgrade(&hash).is_err());
    }

    #[test]
    fn upgrade_fails_before_init() {
        let env = Env::default();
        env.mock_all_auths();
        let client = SubscriptionManagerClient::new(&env, &env.register(SubscriptionManager, ()));
        let hash = BytesN::from_array(&env, &[1; 32]);
        assert_eq!(client.try_upgrade(&hash), Err(Ok(Error::NotInitialised)));
    }

    #[test]
    fn events_have_documented_shape() {
        let c = setup();
        let sub = Address::generate(&c.env);
        let new_admin = Address::generate(&c.env);
        let plan_id2 = String::from_str(&c.env, "p2");
        let payment_ref = String::from_str(&c.env, "t1");
        let payment_ref2 = String::from_str(&c.env, "t2");

        // `env.events().all()` only holds the events of the most recent
        // invocation, so each call's event is checked straight after it.
        let assert_last_event = |topics: soroban_sdk::Vec<soroban_sdk::Val>,
                                 data: soroban_sdk::Val| {
            assert_eq!(
                c.env.events().all().filter_by_contract(&c.client.address),
                vec![&c.env, (c.client.address.clone(), topics, data)]
            );
        };

        // 1. set_admin
        c.client.set_admin(&new_admin);
        assert_last_event(
            (symbol_short!("setadmin"),).into_val(&c.env),
            new_admin.into_val(&c.env),
        );

        // 2. create_plan
        c.client.create_plan(&c.publisher, &plan_id2, &1000);
        assert_last_event(
            (symbol_short!("plan"), plan_id2.clone(), c.publisher.clone()).into_val(&c.env),
            1000i128.into_val(&c.env),
        );

        // 3. subscribe
        let s = c.client.subscribe(&plan_id2, &sub, &payment_ref);
        assert_last_event(
            (symbol_short!("subscribe"), plan_id2.clone(), sub.clone()).into_val(&c.env),
            (s.current_period_end, 1000i128, payment_ref.clone()).into_val(&c.env),
        );

        // 4. renew
        advance(&c.env, 10);
        let r = c.client.renew(&plan_id2, &sub, &payment_ref2);
        assert_last_event(
            (symbol_short!("renew"), plan_id2.clone(), sub.clone()).into_val(&c.env),
            (r.current_period_end, 1000i128, payment_ref2.clone()).into_val(&c.env),
        );

        // 5. cancel
        c.client.cancel(&plan_id2, &sub);
        assert_last_event(
            (symbol_short!("cancel"), plan_id2.clone(), sub.clone()).into_val(&c.env),
            ().into_val(&c.env),
        );

        // 6. set_plan_active
        c.client.set_plan_active(&plan_id2, &false);
        assert_last_event(
            (symbol_short!("setactive"), plan_id2.clone()).into_val(&c.env),
            false.into_val(&c.env),
        );
    }
}
#[cfg(test)]
mod expiry_props;

#[cfg(test)]
mod encoding {
    use super::*;
    use soroban_sdk::{testutils::Address as _, xdr::ToXdr, Address, Env, String};

    /// Golden encoded size (in bytes) of a representative [`Subscription`].
    ///
    /// This pins the XDR layout of the `#[contracttype]` struct so that any
    /// change to its fields, their order or their sizes is caught by CI (the
    /// test suite runs on every PR). If a layout change is intended, update
    /// this value **deliberately** in the same commit and note it in the PR
    /// description — never adjust it just to make CI green.
    const SUBSCRIPTION_ENCODED_LEN: u32 = 0;

    fn sample_subscription(env: &Env) -> Subscription {
        Subscription {
            plan_id: String::from_str(env, "plan"),
            subscriber: Address::generate(env),
            started_at: 1,
            current_period_end: 2,
            cancelled: false,
            total_renewals: 3,
        }
    }

    #[test]
    fn subscription_encoding_size_is_pinned() {
        let env = Env::default();
        let sub = sample_subscription(&env);
        let encoded = sub.to_xdr(&env);
        assert_eq!(
            encoded.len(),
            SUBSCRIPTION_ENCODED_LEN,
            "Subscription encoding changed; update SUBSCRIPTION_ENCODED_LEN deliberately"
        );
    }

    #[test]
    fn subscription_encoding_is_stable_across_values() {
        // Field sizes are fixed-width (String/Address are length-prefixed but
        // the struct itself has no variable padding), so two values with the
        // same field types must encode to the same length.
        let env = Env::default();
        let a = sample_subscription(&env);
        let mut b = sample_subscription(&env);
        b.plan_id = String::from_str(&env, "a-much-longer-plan-identifier");
        assert_eq!(a.to_xdr(&env).len(), b.to_xdr(&env).len());
    }

    #[test]
    fn error_discriminants_are_pinned() {
        // Numbering is per-contract; see the doc comment on `Error` and
        // docs/CONTRACTS.md for the cross-contract mapping.
        assert_eq!(Error::PlanNotFound as u32, 2);
        assert_eq!(Error::PlanInactive as u32, 3);
        assert_eq!(Error::AlreadySubbed as u32, 4);
        assert_eq!(Error::SubNotFound as u32, 5);
        assert_eq!(Error::Cancelled as u32, 6);
        assert_eq!(Error::InvalidPrice as u32, 7);
        assert_eq!(Error::NotInitialised as u32, 8);
        assert_eq!(Error::AlreadyInitialised as u32, 9);
        assert_eq!(Error::PlanExists as u32, 10);
        assert_eq!(Error::Overflow as u32, 11);
        assert_eq!(Error::InvalidPlanId as u32, 12);
        assert_eq!(Error::SameAdmin as u32, 13);
    }
}
