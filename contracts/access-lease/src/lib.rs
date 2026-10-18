#![no_std]
//! SynapsVault — Access Lease Contract (Soroban / Stellar)
//!
//! Issues time-limited on-chain access grants to vault resources.
//! Any third party can verify a buyer's access via `is_valid` without
//! trusting the SynapsVault backend — the Stellar ledger is the source of truth.

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, Address, BytesN,
    ContractExecutable, Env, String,
};

const DAY: u32 = 17_280; // ~5s/ledger × 17280 = 1 day

/// Persistent-storage TTL policy for this contract.
const PERSISTENT_TTL: u32 = 90 * DAY;
/// Threshold at which a persistent entry's TTL is refreshed on access.
const PERSISTENT_TTL_THRESHOLD: u32 = PERSISTENT_TTL - DAY;

/// Instance-storage TTL policy. Instance storage holds the admin/version
/// metadata, which must stay alive for the whole life of the contract, so it
/// shares the persistent policy here.
const BUMP: u32 = PERSISTENT_TTL;
const BUMP_THRESH: u32 = PERSISTENT_TTL_THRESHOLD;

/// Maximum accepted length (in bytes) of a `resource_id`.
pub const MAX_RESOURCE_ID_LEN: u32 = 64;

/// Contract version, sourced from Cargo.toml at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A time-limited access grant issued to a buyer for a specific resource.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Lease {
    pub resource_id: String,
    pub buyer: Address,
    pub granted_at: u32,
    pub expires_at: u32,
    pub duration_ledgers: u32,
}

#[contracttype]
pub enum DataKey {
    Admin,
    /// Pending admin address awaiting acceptance via `accept_admin`.
    PendingAdmin,
    Lease(String, Address),
    Version,
}

/// Contract error codes.
///
/// Numbering is **per-contract** and intentionally not shared across the
/// SynapsVault contracts: each contract owns its own error namespace, so the
/// same numeric code may mean different things in `vault-registry`,
/// `access-lease` and `subscription` (e.g. `AlreadyInitialised` is 8 in the
/// registry, 7 here and 9 in the subscription contract). The cross-contract
/// mapping is documented in `docs/CONTRACTS.md`. The discriminants are pinned
/// by the `error_codes_are_stable` test so any renumbering must be deliberate.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    NotAdmin = 1,
    LeaseNotFound = 2,
    AlreadyActive = 3,
    InvalidDuration = 4,
    NotInitialised = 5,
    AlreadyInitialised = 7,
    LeaseExpired = 8,
    InvalidResourceId = 9,
    /// `accept_admin` was called but no proposal is pending.
    NoPendingAdmin = 10,
    /// `accept_admin` was called by an address that is not the pending admin.
    NotPendingAdmin = 11,
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

#[contractevent(topics = ["proposeadmin"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposeAdminEvent {
    pub pending_admin: Address,
}

#[contractevent(topics = ["acceptadmin"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptAdminEvent {
    pub new_admin: Address,
}

#[contractevent(topics = ["grant"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrantEvent {
    #[topic]
    pub resource_id: String,
    #[topic]
    pub buyer: Address,
    pub expires_at: u32,
}

#[contractevent(topics = ["extend"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtendEvent {
    #[topic]
    pub resource_id: String,
    #[topic]
    pub buyer: Address,
    pub expires_at: u32,
}

#[contractevent(topics = ["revoke"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevokeEvent {
    #[topic]
    pub resource_id: String,
    #[topic]
    pub buyer: Address,
}

#[contractevent(topics = ["upgrade"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradeEvent {
    pub new_wasm_hash: BytesN<32>,
}

#[contractevent(topics = ["migrate"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrateEvent {
    pub new_version: String,
}

#[contract]
pub struct AccessLease;

#[contractimpl]
impl AccessLease {
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

    /// Directly rotate the admin in a single step.
    ///
    /// **Deprecated** — prefer the two-step [`propose_admin`] / [`accept_admin`]
    /// flow (issue #42) to protect against mistyped addresses that would
    /// permanently lock the contract.  This function is kept for backwards
    /// compatibility only.
    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        Self::require_admin(&env)?;
        // Clear any pending proposal so it cannot be accepted after a direct
        // rotation.
        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        env.storage().instance().extend_ttl(BUMP_THRESH, BUMP);
        SetAdminEvent { new_admin }.publish(&env);
        Ok(())
    }

    /// Step 1 of 2 — propose a new admin address.
    ///
    /// The current admin nominates `pending_admin`.  The proposal is stored in
    /// instance storage and must be accepted by `pending_admin` via
    /// [`accept_admin`] to take effect.  Calling `propose_admin` again
    /// overwrites any previous pending proposal.
    pub fn propose_admin(env: Env, pending_admin: Address) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &pending_admin);
        env.storage().instance().extend_ttl(BUMP_THRESH, BUMP);
        ProposeAdminEvent { pending_admin }.publish(&env);
        Ok(())
    }

    /// Step 2 of 2 — accept the pending admin proposal.
    ///
    /// `pending_admin` must sign this call.  On success the caller becomes the
    /// new admin, the pending-admin slot is cleared, and a `setadmin` event is
    /// emitted.  Reverts with [`Error::NoPendingAdmin`] if no proposal is
    /// outstanding, or [`Error::NotPendingAdmin`] if the caller is not the
    /// nominated address.
    pub fn accept_admin(env: Env, pending_admin: Address) -> Result<(), Error> {
        // Verify that a proposal exists and the caller matches.
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .ok_or(Error::NoPendingAdmin)?;
        if stored != pending_admin {
            return Err(Error::NotPendingAdmin);
        }
        // The pending admin must authorise this call.
        pending_admin.require_auth();

        // Promote to admin.
        env.storage()
            .instance()
            .set(&DataKey::Admin, &pending_admin);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.storage().instance().extend_ttl(BUMP_THRESH, BUMP);
        AcceptAdminEvent {
            new_admin: pending_admin.clone(),
        }
        .publish(&env);
        // Also emit the canonical setadmin event for downstream indexers that
        // only listen for that topic.
        SetAdminEvent {
            new_admin: pending_admin,
        }
        .publish(&env);
        Ok(())
    }

    /// Return the pending admin address, if any proposal is outstanding.
    pub fn pending_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::PendingAdmin)
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

    pub fn grant_lease(
        env: Env,
        resource_id: String,
        buyer: Address,
        duration_ledgers: u32,
    ) -> Result<Lease, Error> {
        Self::require_admin(&env)?;
        Self::grant_lease_internal(&env, resource_id, buyer, duration_ledgers)
    }

    pub fn grant_leases(
        env: Env,
        requests: soroban_sdk::Vec<(String, Address, u32)>,
    ) -> Result<soroban_sdk::Vec<Lease>, Error> {
        Self::require_admin(&env)?;
        let mut leases = soroban_sdk::Vec::new(&env);
        for req in requests.iter() {
            leases.push_back(Self::grant_lease_internal(&env, req.0, req.1, req.2)?);
        }
        Ok(leases)
    }

    pub fn extend_lease(
        env: Env,
        resource_id: String,
        buyer: Address,
        extra_ledgers: u32,
    ) -> Result<Lease, Error> {
        Self::require_admin(&env)?;
        if extra_ledgers == 0 {
            return Err(Error::InvalidDuration);
        }
        let key = DataKey::Lease(resource_id.clone(), buyer.clone());
        let mut lease: Lease = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::LeaseNotFound)?;

        let now = env.ledger().sequence();
        let base = lease.expires_at.max(now);
        lease.expires_at = base
            .checked_add(extra_ledgers)
            .ok_or(Error::InvalidDuration)?;

        env.storage().persistent().set(&key, &lease);
        env.storage()
            .persistent()
            .extend_ttl(&key, BUMP_THRESH, BUMP);
        ExtendEvent {
            resource_id,
            buyer,
            expires_at: lease.expires_at,
        }
        .publish(&env);
        Ok(lease)
    }

    pub fn is_valid(env: Env, resource_id: String, buyer: Address) -> bool {
        let key = DataKey::Lease(resource_id, buyer);
        match env.storage().persistent().get::<_, Lease>(&key) {
            Some(lease) => {
                env.storage()
                    .persistent()
                    .extend_ttl(&key, BUMP_THRESH, BUMP);
                lease.expires_at > env.ledger().sequence()
            }
            None => false,
        }
    }

    pub fn get_lease(env: Env, resource_id: String, buyer: Address) -> Result<Lease, Error> {
        let key = DataKey::Lease(resource_id, buyer);
        let lease = env
            .storage()
            .persistent()
            .get::<_, Lease>(&key)
            .ok_or(Error::LeaseNotFound)?;
        env.storage()
            .persistent()
            .extend_ttl(&key, BUMP_THRESH, BUMP);
        Ok(lease)
    }

    pub fn revoke_lease(env: Env, resource_id: String, buyer: Address) -> Result<(), Error> {
        Self::require_admin(&env)?;
        let key = DataKey::Lease(resource_id.clone(), buyer.clone());
        if !env.storage().persistent().has(&key) {
            return Err(Error::LeaseNotFound);
        }
        env.storage().persistent().remove(&key);
        RevokeEvent { resource_id, buyer }.publish(&env);
        Ok(())
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
        let new_version = String::from_str(&env, VERSION);
        env.storage()
            .instance()
            .set(&DataKey::Version, &new_version);
        env.storage().instance().extend_ttl(BUMP_THRESH, BUMP);
        MigrateEvent { new_version }.publish(&env);
        Ok(())
    }
}

impl AccessLease {
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

    fn grant_lease_internal(
        env: &Env,
        resource_id: String,
        buyer: Address,
        duration_ledgers: u32,
    ) -> Result<Lease, Error> {
        if resource_id.is_empty() || resource_id.len() > MAX_RESOURCE_ID_LEN {
            return Err(Error::InvalidResourceId);
        }
        if duration_ledgers == 0 {
            return Err(Error::InvalidDuration);
        }
        let key = DataKey::Lease(resource_id.clone(), buyer.clone());
        let now = env.ledger().sequence();

        if let Some(existing) = env.storage().persistent().get::<_, Lease>(&key) {
            if existing.expires_at > now {
                return Err(Error::AlreadyActive);
            }
        }

        let expires_at = now
            .checked_add(duration_ledgers)
            .ok_or(Error::InvalidDuration)?;
        let lease = Lease {
            resource_id: resource_id.clone(),
            buyer: buyer.clone(),
            granted_at: now,
            expires_at,
            duration_ledgers,
        };

        env.storage().persistent().set(&key, &lease);
        env.storage()
            .persistent()
            .extend_ttl(&key, BUMP_THRESH, BUMP);
        GrantEvent {
            resource_id,
            buyer,
            expires_at,
        }
        .publish(env);
        Ok(lease)
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

    /// Real WASM for success-path `upgrade` tests: the host rejects a hash
    /// that was never uploaded. Reuses the vault-registry upgrade fixture.
    const UPGRADE_TARGET_WASM: &[u8] =
        include_bytes!("../../vault-registry/fixtures/vault_registry_upgrade_target.wasm");

    fn uploaded_wasm_hash(env: &Env) -> BytesN<32> {
        env.deployer().upload_contract_wasm(UPGRADE_TARGET_WASM)
    }

    fn setup<'a>() -> (Env, AccessLeaseClient<'a>, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let id = env.register(AccessLease, ());
        let client = AccessLeaseClient::new(&env, &id);
        let admin = Address::generate(&env);
        client.init(&admin);
        (env, client, admin)
    }

    fn advance(env: &Env, ledgers: u32) {
        env.ledger()
            .set_sequence_number(env.ledger().sequence() + ledgers);
    }

    #[test]
    fn test_grant_and_check_lease() {
        let (env, client, _admin) = setup();
        let buyer = Address::generate(&env);
        let resource_id = String::from_str(&env, "res_001");

        assert!(!client.is_valid(&resource_id, &buyer));

        let lease = client.grant_lease(&resource_id, &buyer, &1000u32);
        assert_eq!(lease.duration_ledgers, 1000u32);
        assert_eq!(lease.expires_at, lease.granted_at + 1000);
        assert!(client.is_valid(&resource_id, &buyer));
        assert_eq!(client.get_lease(&resource_id, &buyer), lease);
    }

    #[test]
    fn test_grant_leases_batch() {
        let (env, client, _admin) = setup();
        let b1 = Address::generate(&env);
        let b2 = Address::generate(&env);
        let rid1 = String::from_str(&env, "r1");
        let rid2 = String::from_str(&env, "r2");

        let mut requests = soroban_sdk::Vec::new(&env);
        requests.push_back((rid1.clone(), b1.clone(), 100u32));
        requests.push_back((rid2.clone(), b2.clone(), 200u32));

        let leases = client.grant_leases(&requests);
        assert_eq!(leases.len(), 2);
        assert!(client.is_valid(&rid1, &b1));
        assert!(client.is_valid(&rid2, &b2));
        assert_eq!(leases.get_unchecked(0).duration_ledgers, 100);
        assert_eq!(leases.get_unchecked(1).duration_ledgers, 200);
    }

    #[test]
    fn test_revoke_lease() {
        let (env, client, _admin) = setup();
        let buyer = Address::generate(&env);
        let resource_id = String::from_str(&env, "res_001");

        client.grant_lease(&resource_id, &buyer, &500u32);
        assert!(client.is_valid(&resource_id, &buyer));

        client.revoke_lease(&resource_id, &buyer);
        assert!(!client.is_valid(&resource_id, &buyer));
        assert_eq!(
            client.try_get_lease(&resource_id, &buyer),
            Err(Ok(Error::LeaseNotFound))
        );
    }

    #[test]
    fn init_twice_is_rejected() {
        let (env, client, admin) = setup();
        let attacker = Address::generate(&env);
        assert_eq!(
            client.try_init(&attacker),
            Err(Ok(Error::AlreadyInitialised))
        );
        assert_eq!(client.admin(), admin);
    }

    #[test]
    fn admin_calls_fail_before_init() {
        let env = Env::default();
        env.mock_all_auths();
        let client = AccessLeaseClient::new(&env, &env.register(AccessLease, ()));
        let buyer = Address::generate(&env);
        let rid = String::from_str(&env, "r");
        assert_eq!(client.try_admin(), Err(Ok(Error::NotInitialised)));
        assert_eq!(
            client.try_grant_lease(&rid, &buyer, &10u32),
            Err(Ok(Error::NotInitialised))
        );
    }

    #[test]
    fn grant_requires_admin_auth() {
        let (env, client, admin) = setup();
        let buyer = Address::generate(&env);
        let rid = String::from_str(&env, "r");
        client.grant_lease(&rid, &buyer, &10u32);
        assert_eq!(env.auths()[0].0, admin);
    }

    #[test]
    fn grant_by_non_admin_is_rejected() {
        let (env, client, _admin) = setup();
        let mallory = Address::generate(&env);
        let buyer = Address::generate(&env);
        let rid = String::from_str(&env, "r");
        env.mock_auths(&[MockAuth {
            address: &mallory,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "grant_lease",
                args: (rid.clone(), buyer.clone(), 10u32).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        assert!(client.try_grant_lease(&rid, &buyer, &10u32).is_err());
    }

    #[test]
    fn set_admin_by_non_admin_is_rejected() {
        let (env, client, _admin) = setup();
        let mallory = Address::generate(&env);
        let new_admin = Address::generate(&env);
        env.mock_auths(&[MockAuth {
            address: &mallory,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "set_admin",
                args: (new_admin.clone(),).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        assert!(client.try_set_admin(&new_admin).is_err());
    }

    #[test]
    fn upgrade_success() {
        let (env, client, _admin) = setup();
        let hash = uploaded_wasm_hash(&env);
        client.upgrade(&hash);
        // Auth checks passed successfully
    }

    #[test]
    fn upgrade_by_non_admin_is_rejected() {
        let (env, client, _admin) = setup();
        let mallory = Address::generate(&env);
        let hash = BytesN::from_array(&env, &[1; 32]);
        env.mock_auths(&[MockAuth {
            address: &mallory,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "upgrade",
                args: (hash.clone(),).into_val(&env),
                sub_invokes: &[],
            },
        }]);
        assert!(client.try_upgrade(&hash).is_err());
    }

    #[test]
    fn upgrade_fails_before_init() {
        let env = Env::default();
        env.mock_all_auths();
        let client = AccessLeaseClient::new(&env, &env.register(AccessLease, ()));
        let hash = BytesN::from_array(&env, &[1; 32]);
        assert_eq!(client.try_upgrade(&hash), Err(Ok(Error::NotInitialised)));
    }

    #[test]
    fn zero_duration_rejected() {
        let (env, client, _admin) = setup();
        let buyer = Address::generate(&env);
        let rid = String::from_str(&env, "r");
        assert_eq!(
            client.try_grant_lease(&rid, &buyer, &0u32),
            Err(Ok(Error::InvalidDuration))
        );
        client.grant_lease(&rid, &buyer, &10u32);
        assert_eq!(
            client.try_extend_lease(&rid, &buyer, &0u32),
            Err(Ok(Error::InvalidDuration))
        );
    }

    #[test]
    fn overflowing_duration_rejected() {
        let (env, client, _admin) = setup();
        advance(&env, 10);
        let buyer = Address::generate(&env);
        let rid = String::from_str(&env, "r");
        assert_eq!(
            client.try_grant_lease(&rid, &buyer, &u32::MAX),
            Err(Ok(Error::InvalidDuration))
        );
        client.grant_lease(&rid, &buyer, &10u32);
        assert_eq!(
            client.try_extend_lease(&rid, &buyer, &u32::MAX),
            Err(Ok(Error::InvalidDuration))
        );
    }

    #[test]
    fn lease_expires_exactly_at_expiry() {
        let (env, client, _admin) = setup();
        let buyer = Address::generate(&env);
        let rid = String::from_str(&env, "r");
        client.grant_lease(&rid, &buyer, &100u32);
        advance(&env, 99);
        assert!(client.is_valid(&rid, &buyer));
        advance(&env, 1);
        assert!(!client.is_valid(&rid, &buyer));
        // Record is still readable after expiry.
        assert_eq!(client.get_lease(&rid, &buyer).duration_ledgers, 100);
    }

    #[test]
    fn active_lease_cannot_be_regranted_but_expired_can() {
        let (env, client, _admin) = setup();
        let buyer = Address::generate(&env);
        let rid = String::from_str(&env, "r");
        client.grant_lease(&rid, &buyer, &100u32);
        assert_eq!(
            client.try_grant_lease(&rid, &buyer, &100u32),
            Err(Ok(Error::AlreadyActive))
        );
        advance(&env, 100);
        let lease = client.grant_lease(&rid, &buyer, &50u32);
        assert_eq!(lease.granted_at, env.ledger().sequence());
        assert!(client.is_valid(&rid, &buyer));
    }

    #[test]
    fn extend_active_lease_stacks_on_expiry() {
        let (env, client, _admin) = setup();
        let buyer = Address::generate(&env);
        let rid = String::from_str(&env, "r");
        let lease = client.grant_lease(&rid, &buyer, &100u32);
        advance(&env, 40);
        let extended = client.extend_lease(&rid, &buyer, &50u32);
        assert_eq!(extended.expires_at, lease.expires_at + 50);
    }

    #[test]
    fn extend_expired_lease_restarts_from_now() {
        let (env, client, _admin) = setup();
        let buyer = Address::generate(&env);
        let rid = String::from_str(&env, "r");
        client.grant_lease(&rid, &buyer, &10u32);
        advance(&env, 500);
        let extended = client.extend_lease(&rid, &buyer, &50u32);
        assert_eq!(extended.expires_at, env.ledger().sequence() + 50);
        assert!(client.is_valid(&rid, &buyer));
    }

    #[test]
    fn extend_and_revoke_missing_lease_fail() {
        let (env, client, _admin) = setup();
        let buyer = Address::generate(&env);
        let rid = String::from_str(&env, "missing");
        assert_eq!(
            client.try_extend_lease(&rid, &buyer, &10u32),
            Err(Ok(Error::LeaseNotFound))
        );
        assert_eq!(
            client.try_revoke_lease(&rid, &buyer),
            Err(Ok(Error::LeaseNotFound))
        );
    }

    #[test]
    fn leases_are_scoped_per_resource_and_buyer() {
        let (env, client, _admin) = setup();
        let alice = Address::generate(&env);
        let bob = Address::generate(&env);
        let r1 = String::from_str(&env, "r1");
        let r2 = String::from_str(&env, "r2");
        client.grant_lease(&r1, &alice, &10u32);
        assert!(client.is_valid(&r1, &alice));
        assert!(!client.is_valid(&r2, &alice));
        assert!(!client.is_valid(&r1, &bob));
    }

    #[test]
    fn set_admin_rotates_admin() {
        let (env, client, admin) = setup();
        let new_admin = Address::generate(&env);
        client.set_admin(&new_admin);
        assert_eq!(env.auths()[0].0, admin);
        assert_eq!(client.admin(), new_admin);
    }

    /// Event topics/data must stay wire-compatible with what indexers expect
    /// (see docs/CONTRACTS.md).
    #[test]
    fn events_have_documented_shape() {
        let (env, client, admin) = setup();
        let buyer = Address::generate(&env);
        let rid = String::from_str(&env, "r");
        let new_admin = Address::generate(&env);

        // `env.events().all()` only holds the events of the most recent
        // invocation, so each call's event is checked straight after it.
        let assert_last_event = |topics: soroban_sdk::Vec<soroban_sdk::Val>,
                                 data: soroban_sdk::Val| {
            assert_eq!(
                env.events().all().filter_by_contract(&client.address),
                vec![&env, (client.address.clone(), topics, data)]
            );
        };

        // 1. init (from setup)
        assert_last_event(
            (symbol_short!("init"),).into_val(&env),
            admin.into_val(&env),
        );

        // 2. grant
        let lease = client.grant_lease(&rid, &buyer, &10u32);
        assert_last_event(
            (symbol_short!("grant"), rid.clone(), buyer.clone()).into_val(&env),
            lease.expires_at.into_val(&env),
        );

        // 3. extend
        let extended = client.extend_lease(&rid, &buyer, &5u32);
        assert_last_event(
            (symbol_short!("extend"), rid.clone(), buyer.clone()).into_val(&env),
            extended.expires_at.into_val(&env),
        );

        // 4. revoke
        client.revoke_lease(&rid, &buyer);
        assert_last_event(
            (symbol_short!("revoke"), rid.clone(), buyer.clone()).into_val(&env),
            ().into_val(&env),
        );

        // 5. setadmin
        client.set_admin(&new_admin);
        assert_last_event(
            (symbol_short!("setadmin"),).into_val(&env),
            new_admin.into_val(&env),
        );

        // 6. upgrade (uploading the WASM is itself a host call, so do it last)
        let hash = uploaded_wasm_hash(&env);
        client.upgrade(&hash);
        assert_last_event(
            (symbol_short!("upgrade"),).into_val(&env),
            hash.into_val(&env),
        );
    }

    #[test]
    fn is_valid_false_exactly_at_expires_at_after_extend_on_expired_lease() {
        let (env, client, _admin) = setup();
        let buyer = Address::generate(&env);
        let rid = String::from_str(&env, "r");

        // Grant and let it expire.
        client.grant_lease(&rid, &buyer, &10u32);
        advance(&env, 500);
        assert!(!client.is_valid(&rid, &buyer));

        // Extend the expired lease by 50 ledgers — base is now (500), so expires_at = 550.
        let extended = client.extend_lease(&rid, &buyer, &50u32);
        let expected_expiry = env.ledger().sequence() + 50;
        assert_eq!(extended.expires_at, expected_expiry);
        assert!(client.is_valid(&rid, &buyer));

        // One ledger before expiry: still valid.
        advance(&env, 49);
        assert!(client.is_valid(&rid, &buyer));

        // Exactly at expires_at: no longer valid.
        advance(&env, 1);
        assert_eq!(env.ledger().sequence(), expected_expiry);
        assert!(!client.is_valid(&rid, &buyer));
    }

    #[test]
    fn grant_lease_boundary_at_u32_max() {
        let (env, client, _admin) = setup();
        // Position ledger so that now + duration == u32::MAX exactly.
        let now = 100u32;
        env.ledger().set_sequence_number(now);
        let duration = u32::MAX - now;
        let buyer = Address::generate(&env);
        let rid = String::from_str(&env, "r");

        let lease = client.grant_lease(&rid, &buyer, &duration);
        assert_eq!(lease.expires_at, u32::MAX);
        assert_eq!(lease.granted_at, now);
        assert_eq!(lease.duration_ledgers, duration);
        assert!(client.is_valid(&rid, &buyer));
    }

    #[test]
    fn revoke_then_regrant_same_ledger() {
        let (env, client, _admin) = setup();
        let buyer = Address::generate(&env);
        let rid = String::from_str(&env, "r");

        client.grant_lease(&rid, &buyer, &100u32);
        assert!(client.is_valid(&rid, &buyer));

        // Revoke and immediately re-grant without advancing the ledger.
        client.revoke_lease(&rid, &buyer);
        assert!(!client.is_valid(&rid, &buyer));

        let lease = client.grant_lease(&rid, &buyer, &50u32);
        assert_eq!(lease.granted_at, env.ledger().sequence());
        assert_eq!(lease.expires_at, env.ledger().sequence() + 50);
        assert!(client.is_valid(&rid, &buyer));
    }

    #[test]
    fn version_check() {
        let (env, client, _admin) = setup();
        assert_eq!(client.get_version(), String::from_str(&env, VERSION));
        assert!(client.is_compatible());

        let fresh = AccessLeaseClient::new(&env, &env.register(AccessLease, ()));
        assert!(!fresh.is_compatible());
    }

    #[test]
    fn migrate_updates_stored_version() {
        let (env, client, admin) = setup();

        // Simulate a stale deployment: stored version differs from compiled.
        env.as_contract(&client.address, || {
            env.storage()
                .instance()
                .set(&DataKey::Version, &String::from_str(&env, "0.0.0"));
        });
        assert_eq!(client.get_version(), String::from_str(&env, "0.0.0"));
        assert!(!client.is_compatible());

        client.migrate();
        assert_eq!(env.auths()[0].0, admin);
        assert_eq!(client.get_version(), String::from_str(&env, VERSION));
        assert!(client.is_compatible());
    }

    #[test]
    fn migrate_requires_admin_auth() {
        let (env, client, _admin) = setup();
        let mallory = Address::generate(&env);
        env.mock_auths(&[MockAuth {
            address: &mallory,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "migrate",
                args: ().into_val(&env),
                sub_invokes: &[],
            },
        }]);
        assert!(client.try_migrate().is_err());
    }

    #[test]
    fn migrate_fails_before_init() {
        let env = Env::default();
        env.mock_all_auths();
        let client = AccessLeaseClient::new(&env, &env.register(AccessLease, ()));
        assert_eq!(client.try_migrate(), Err(Ok(Error::NotInitialised)));
    }

    /// Pins the numeric discriminants of [`Error`].
    ///
    /// Error codes are part of the contract's public ABI: clients and
    /// indexers match on the raw `u32` returned by a failed invocation, so a
    /// renumbering is a breaking change. This test makes any such change
    /// deliberate — update the expected values here (and the cross-contract
    /// mapping in `docs/CONTRACTS.md`) only when the renumbering is intended.
    #[test]
    fn error_codes_are_stable() {
        assert_eq!(Error::NotAdmin as u32, 1);
        assert_eq!(Error::LeaseNotFound as u32, 2);
        assert_eq!(Error::AlreadyActive as u32, 3);
        assert_eq!(Error::InvalidDuration as u32, 4);
        assert_eq!(Error::NotInitialised as u32, 5);
        // 6 is intentionally unused (historical gap).
        assert_eq!(Error::AlreadyInitialised as u32, 7);
        assert_eq!(Error::LeaseExpired as u32, 8);
        assert_eq!(Error::InvalidResourceId as u32, 9);
        assert_eq!(Error::NoPendingAdmin as u32, 10);
        assert_eq!(Error::NotPendingAdmin as u32, 11);
    }

    /// Golden XDR-encoded length of a representative [`Lease`].
    ///
    /// `Lease` is a `#[contracttype]`, so its XDR layout is part of the
    /// on-chain storage and event ABI. Changing a field's type, order or
    /// adding/removing a field changes the encoded size and can silently
    /// break existing stored leases and downstream decoders.
    ///
    /// CI runs this test: if it fails, the `Lease` layout changed. Only update
    /// `LEASE_XDR_LEN` after confirming the change is intentional and after
    /// migrating any persisted data / updating `docs/CONTRACTS.md`.
    const LEASE_XDR_LEN: u32 = 0;

    #[test]
    fn lease_xdr_encoding_size_is_stable() {
        use soroban_sdk::xdr::ToXdr;

        let env = Env::default();
        let lease = Lease {
            resource_id: String::from_str(&env, "res_001"),
            buyer: Address::from_str(
                &env,
                "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
            ),
            granted_at: 1_000,
            expires_at: 2_000,
            duration_ledgers: 1_000,
        };

        let encoded = lease.to_xdr(&env);
        assert_eq!(
            encoded.len(),
            LEASE_XDR_LEN,
            "Lease XDR encoding changed: update LEASE_XDR_LEN deliberately \
             (and docs/CONTRACTS.md) if this layout change is intended"
        );
    }
}
#[cfg(test)]
mod expiry_props;
