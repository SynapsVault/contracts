#![no_std]
//! SynapsVault — Access Lease Contract (Soroban / Stellar)
//!
//! Issues time-limited on-chain access grants to vault resources.
//! Any third party can verify a buyer's access via `is_valid` without
//! trusting the SynapsVault backend — the Stellar ledger is the source of truth.

use soroban_sdk::{contract, contracterror, contractimpl, contracttype, Address, Env, String};

const DAY:         u32 = 17_280; // ~5s/ledger × 17280 = 1 day
const BUMP:        u32 = 90 * DAY;
const BUMP_THRESH: u32 = BUMP - DAY;

/// Contract version, sourced from Cargo.toml at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Lease {
    pub resource_id:      String,
    pub buyer:            Address,
    pub granted_at:       u32,
    pub expires_at:       u32,
    pub duration_ledgers: u32,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Lease(String, Address),
    Version,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    NotAdmin        = 1,
    LeaseNotFound   = 2,
    AlreadyActive   = 3,
    InvalidDuration = 4,
    NotInitialised  = 5,
}

#[contract]
pub struct AccessLease;

#[contractimpl]
impl AccessLease {
    /// Initialise the contract — set the admin wallet (backend platform wallet).
    /// Must be called once immediately after deployment.
    pub fn init(env: Env, admin: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Version, &String::from_str(&env, VERSION));
        env.storage().instance().extend_ttl(BUMP_THRESH, BUMP);
    }

    /// Return the contract version string. Permissionless.
    pub fn get_version(env: Env) -> String {
        env.storage()
            .instance()
            .get(&DataKey::Version)
            .unwrap_or_else(|| String::from_str(&env, VERSION))
    }

    /// Grant a time-limited lease to `buyer` for `resource_id`.
    /// Only callable by the admin. Errors if a still-active lease already exists.
    pub fn grant_lease(
        env: Env,
        resource_id: String,
        buyer: Address,
        duration_ledgers: u32,
    ) -> Result<Lease, Error> {
        Self::require_admin(&env)?;

        if duration_ledgers == 0 {
            return Err(Error::InvalidDuration);
        }

        let key = DataKey::Lease(resource_id.clone(), buyer.clone());

        // Reject if an active (not yet expired) lease exists.
        if let Some(existing) = env.storage().persistent().get::<_, Lease>(&key) {
            if existing.expires_at > env.ledger().sequence() {
                return Err(Error::AlreadyActive);
            }
        }

        let now = env.ledger().sequence();
        let lease = Lease {
            resource_id,
            buyer,
            granted_at:       now,
            expires_at:       now + duration_ledgers,
            duration_ledgers,
        };

        env.storage().persistent().set(&key, &lease);
        env.storage().persistent().extend_ttl(&key, BUMP_THRESH, BUMP);
        Ok(lease)
    }

    /// Extend an existing lease by `extra_ledgers` (admin only).
    /// Works on both active and expired leases.
    pub fn extend_lease(
        env: Env,
        resource_id: String,
        buyer: Address,
        extra_ledgers: u32,
    ) -> Result<Lease, Error> {
        Self::require_admin(&env)?;
        let key = DataKey::Lease(resource_id.clone(), buyer.clone());
        let mut lease: Lease = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::LeaseNotFound)?;

        // Extend from end of current period, or from now if already expired.
        let base = lease.expires_at.max(env.ledger().sequence());
        lease.expires_at = base + extra_ledgers;

        env.storage().persistent().set(&key, &lease);
        env.storage().persistent().extend_ttl(&key, BUMP_THRESH, BUMP);
        Ok(lease)
    }

    /// Returns `true` if `buyer` currently holds a valid lease for `resource_id`.
    /// Permissionless — any caller can verify.
    pub fn is_valid(env: Env, resource_id: String, buyer: Address) -> bool {
        let key = DataKey::Lease(resource_id, buyer);
        match env.storage().persistent().get::<_, Lease>(&key) {
            Some(lease) => lease.expires_at > env.ledger().sequence(),
            None        => false,
        }
    }

    /// Return the full Lease struct. Permissionless.
    pub fn get_lease(env: Env, resource_id: String, buyer: Address) -> Result<Lease, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Lease(resource_id, buyer))
            .ok_or(Error::LeaseNotFound)
    }

    /// Revoke a lease immediately (admin only). Used on refund or ToS violation.
    pub fn revoke_lease(env: Env, resource_id: String, buyer: Address) -> Result<(), Error> {
        Self::require_admin(&env)?;
        let key = DataKey::Lease(resource_id, buyer);
        if !env.storage().persistent().has(&key) {
            return Err(Error::LeaseNotFound);
        }
        env.storage().persistent().remove(&key);
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

    /// Returns `true` if the stored version matches the compiled-in version.
    /// Used to detect stale deployments that need re-initialisation.
    pub fn is_compatible(env: Env) -> bool {
        match env.storage().instance().get::<_, String>(&DataKey::Version) {
            Some(stored) => stored == String::from_str(&env, VERSION),
            None         => false,
        }
    }
}





#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Address, Env, String};

    fn setup<'a>() -> (Env, AccessLeaseClient<'a>) {
        let env = Env::default();
        env.mock_all_auths();
        let id = env.register(AccessLease, ());
        let client = AccessLeaseClient::new(&env, &id);
        (env, client)
    }

    #[test]
    fn test_grant_and_check_lease() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let buyer = Address::generate(&env);
        let resource_id = String::from_str(&env, "res_001");

        client.init(&admin);
        assert!(!client.is_valid(&resource_id, &buyer));

        let lease = client.grant_lease(&resource_id, &buyer, &1000u32);
        assert_eq!(lease.duration_ledgers, 1000u32);
        assert!(client.is_valid(&resource_id, &buyer));
    }

    #[test]
    fn test_revoke_lease() {
        let (env, client) = setup();
        let admin = Address::generate(&env);
        let buyer = Address::generate(&env);
        let resource_id = String::from_str(&env, "res_001");

        client.init(&admin);
        client.grant_lease(&resource_id, &buyer, &500u32);
        assert!(client.is_valid(&resource_id, &buyer));

        client.revoke_lease(&resource_id, &buyer);
        assert!(!client.is_valid(&resource_id, &buyer));
    }
}