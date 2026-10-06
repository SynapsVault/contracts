#![no_std]
//! SynapsVault — Access Lease Contract (Soroban / Stellar)
//!
//! Issues time-limited on-chain access grants to vault resources.
//! Any third party can verify a buyer's access via `is_valid` without
//! trusting the SynapsVault backend — the Stellar ledger is the source of truth.

use soroban_sdk::{contract, contracterror, contractimpl, contracttype, Address, BytesN, Env, String};

const DAY:         u32 = 17_280; // ~5s/ledger × 17280 = 1 day
const BUMP:        u32 = 90 * DAY;
const BUMP_THRESH: u32 = BUMP - DAY;

/// A time-limited access grant issued to a buyer for a specific resource.
///
/// A lease is considered **active** (valid) while
/// `expires_at > env.ledger().sequence()`. Once the current ledger sequence
/// reaches or passes `expires_at`, the lease is expired and `is_valid`
/// returns `false`, even though the record may still be stored on-chain.
/// Contract version, sourced from Cargo.toml at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Lease {
    /// Opaque identifier of the vault resource the lease grants access to.
    pub resource_id:      String,
    /// Wallet address entitled to access the resource while the lease is active.
    pub buyer:            Address,
    /// Ledger sequence at which the lease was granted.
    pub granted_at:       u32,
    /// Ledger sequence at which the lease stops being valid (exclusive).
    pub expires_at:       u32,
    /// Length of the lease in ledgers, as originally requested.
    pub duration_ledgers: u32,
}

/// Storage keys used by the contract.
///
/// * `Admin` — instance-storage key holding the admin [`Address`] set by
///   [`AccessLease::init`]. Its presence is also the contract's
///   "initialised" flag.
/// * `Lease(resource_id, buyer)` — persistent-storage key holding the
///   [`Lease`] for a given `(resource_id, buyer)` pair.
#[contracttype]
pub enum DataKey {
    Admin,
    Lease(String, Address),
    Version,
}

/// Errors returned by the contract's fallible entry points.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    /// The caller is not the configured admin (or failed auth).
    NotAdmin        = 1,
    /// No lease exists for the given `(resource_id, buyer)` pair.
    LeaseNotFound   = 2,
    /// An unexpired lease already exists for the given pair.
    AlreadyActive   = 3,
    /// The requested duration was zero.
    InvalidDuration = 4,
    /// `init` has not been called, so no admin is configured.
    NotInitialised  = 5,
    UpgradeNotAllowed = 6,
}

#[contract]
pub struct AccessLease;

#[contractimpl]
impl AccessLease {
    /// Initialise the contract — set the admin wallet (backend platform wallet).
    ///
    /// Must be called once immediately after deployment. Stores `admin` under
    /// [`DataKey::Admin`] in instance storage and bumps the instance TTL.
    ///
    /// # Parameters
    /// * `admin` — the platform wallet authorised to perform admin-only
    ///   mutations ([`grant_lease`](Self::grant_lease),
    ///   [`extend_lease`](Self::extend_lease),
    ///   [`revoke_lease`](Self::revoke_lease)).
    ///
    /// # Notes
    /// This function does not check whether an admin is already set; calling it
    /// again overwrites the existing admin.
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
    ///
    /// Admin-only: the caller must be the configured admin and must have
    /// authorised the call (see [`require_admin`](Self::require_admin)).
    ///
    /// # Parameters
    /// * `resource_id` — identifier of the resource being leased.
    /// * `buyer` — wallet receiving access.
    /// * `duration_ledgers` — lease length in ledgers; must be non-zero.
    ///
    /// # Errors
    /// * [`Error::NotInitialised`] — `init` has not been called.
    /// * [`Error::NotAdmin`] — caller is not the admin / auth failed.
    /// * [`Error::InvalidDuration`] — `duration_ledgers == 0`.
    /// * [`Error::AlreadyActive`] — an unexpired lease already exists for the
    ///   `(resource_id, buyer)` pair.
    ///
    /// # Invariants
    /// On success, `expires_at == granted_at + duration_ledgers`, the lease is
    /// written to persistent storage, and its TTL is bumped to [`BUMP`].
    /// Expired leases may be replaced by a new grant.
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

    /// Extend an existing lease by `extra_ledgers`.
    ///
    /// Admin-only. Works on both active and expired leases: the new expiry is
    /// computed from `max(existing.expires_at, current_ledger)` so that an
    /// expired lease restarts from the current ledger rather than from its
    /// stale expiry.
    ///
    /// # Parameters
    /// * `resource_id` — identifier of the leased resource.
    /// * `buyer` — wallet holding the lease.
    /// * `extra_ledgers` — number of ledgers to add to the effective expiry.
    ///
    /// # Errors
    /// * [`Error::NotInitialised`] / [`Error::NotAdmin`] — admin check failed.
    /// * [`Error::LeaseNotFound`] — no lease exists for the pair.
    ///
    /// # Invariants
    /// On success, `expires_at == max(old_expires_at, now) + extra_ledgers`
    /// and the persistent TTL is bumped to [`BUMP`].
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
    ///
    /// Permissionless — any caller can verify without trusting the backend.
    ///
    /// # Parameters
    /// * `resource_id` — identifier of the resource to check.
    /// * `buyer` — wallet whose access is being verified.
    ///
    /// # Invariants
    /// A lease is valid iff it exists and
    /// `expires_at > env.ledger().sequence()`. Returns `false` when no lease
    /// record exists.
    pub fn is_valid(env: Env, resource_id: String, buyer: Address) -> bool {
        let key = DataKey::Lease(resource_id, buyer);
        match env.storage().persistent().get::<_, Lease>(&key) {
            Some(lease) => lease.expires_at > env.ledger().sequence(),
            None        => false,
        }
    }

    /// Return the full [`Lease`] struct for the given pair.
    ///
    /// Permissionless. Unlike [`is_valid`](Self::is_valid), this returns the
    /// record even if the lease has expired.
    ///
    /// # Parameters
    /// * `resource_id` — identifier of the leased resource.
    /// * `buyer` — wallet holding the lease.
    ///
    /// # Errors
    /// * [`Error::LeaseNotFound`] — no lease exists for the pair.
    pub fn get_lease(env: Env, resource_id: String, buyer: Address) -> Result<Lease, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Lease(resource_id, buyer))
            .ok_or(Error::LeaseNotFound)
    }

    /// Revoke a lease immediately.
    ///
    /// Admin-only. Used on refund or Terms-of-Service violation. Removes the
    /// lease record from persistent storage, so subsequent
    /// [`is_valid`](Self::is_valid) calls return `false`.
    ///
    /// # Parameters
    /// * `resource_id` — identifier of the leased resource.
    /// * `buyer` — wallet whose lease is being revoked.
    ///
    /// # Errors
    /// * [`Error::NotInitialised`] / [`Error::NotAdmin`] — admin check failed.
    /// * [`Error::LeaseNotFound`] — no lease exists for the pair.
    pub fn revoke_lease(env: Env, resource_id: String, buyer: Address) -> Result<(), Error> {
        Self::require_admin(&env)?;
        let key = DataKey::Lease(resource_id, buyer);
        if !env.storage().persistent().has(&key) {
            return Err(Error::LeaseNotFound);
        }
        env.storage().persistent().remove(&key);
        Ok(())
    }

    /// Upgrade the contract WASM to `new_wasm_hash` (admin only).
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.deployer().update_current_contract_wasm(new_wasm_hash);
        Ok(())
    }

    // ─── Internal helpers ────────────────────────────────────────────────────

    /// Load the configured admin from instance storage and require its auth.
    ///
    /// # Errors
    /// * [`Error::NotInitialised`] — no admin has been set via `init`.
    /// * [`Error::NotAdmin`] — the admin's `require_auth` check failed.
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