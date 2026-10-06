#![no_std]
//! MindVault on-chain vault registry.
//!
//! Records each vault resource on Stellar: its creator, price (in USDC
//! stroops, 7 decimals), and a metadata pointer (e.g. an IPFS URI or content
//! hash). Payment itself still flows through x402 + the USDC SAC off this
//! contract — this registry is the transparent, on-chain source of truth for
//! *what* exists, *who* owns it, and *what it costs*.
//!
//! Only the recorded creator can mutate a resource (enforced via
//! `require_auth`). Ownership can be transferred.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env,
    IntoVal, String, Val, Vec,
};

// ~5s ledgers → 17,280 per day. Persistent entries are bumped ~30 days on each
// write so an actively-managed resource is never archived out from under us.
const DAY_IN_LEDGERS: u32 = 17280;
const BUMP_AMOUNT: u32 = 30 * DAY_IN_LEDGERS;
const LIFETIME_THRESHOLD: u32 = BUMP_AMOUNT - DAY_IN_LEDGERS;
/// Maximum byte length of a metadata pointer (IPFS URI, content hash, compact
/// JSON anchor). Enforced on `register` and `update_metadata`; longer values
/// are rejected with [`Error::MetadataTooLong`].
pub const MAX_METADATA_POINTER_LEN: u32 = 512;
/// Maximum number of discovery tags allowed per resource. Exceeding this is
/// rejected with [`Error::InvalidTag`].
const MAX_TAGS: u32 = 8;
/// Maximum byte length of a single discovery tag. Tags must be non-empty and
/// no longer than this; violations are rejected with [`Error::InvalidTag`].
const MAX_TAG_LEN: u32 = 32;

/// A registered vault resource.
///
/// Invariants:
/// - `id` is unique across the registry and immutable once registered.
/// - `price` is strictly positive (in USDC stroops, 7 decimals).
/// - `metadata.len() <= MAX_METADATA_POINTER_LEN`.
/// - `tags.len() <= MAX_TAGS`, each tag non-empty and `<= MAX_TAG_LEN` bytes.
/// - Only `creator` may mutate the resource (enforced via `require_auth`).
/// Contract version, sourced from the crate manifest at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Minimum version this contract is compatible with.
pub const MIN_COMPATIBLE_VERSION: &str = "0.1.0";

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Resource {
    /// Unique, immutable resource identifier.
    pub id: String,
    /// Current owner; the only address authorized to mutate this resource.
    pub creator: Address,
    /// Price in USDC stroops (7 decimals); always strictly positive.
    pub price: i128,
    /// Off-chain content anchor (IPFS URI, content hash, etc.).
    pub metadata: String,
    /// Whether the resource is discoverable via `list`. Defaults to `true`.
    pub listed: bool,
    /// Discovery labels (e.g. "dataset", "research"). Distinct from `metadata`,
    /// which remains the off-chain content anchor (IPFS URI, content hash, etc.).
    pub tags: Vec<String>,
}

/// Storage keys for the registry.
///
/// `Resource(id)` and `Index(i)` live in persistent storage and have their TTL
/// bumped on every write; `Count` lives in instance storage and is bumped
/// alongside it.
#[contracttype]
pub enum DataKey {
    /// Maps a resource id to its [`Resource`] record.
    Resource(String),
    /// Monotonic count of resources ever registered (never decremented).
    Count,
    /// Maps insertion index `i` (in `0..Count`) to a resource id.
    Index(u32),
    Admin,
    Version,
}

/// Errors returned by registry operations.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// A resource with the given id is already registered.
    AlreadyRegistered = 1,
    /// No resource exists with the given id.
    NotFound = 2,
    /// Price must be strictly positive.
    InvalidPrice = 3,
    /// Metadata pointer exceeds [`MAX_METADATA_POINTER_LEN`].
    MetadataTooLong = 4,
    /// Tags exceed [`MAX_TAGS`], or a tag is empty or exceeds [`MAX_TAG_LEN`].
    InvalidTag = 5,
    NotAdmin = 6,
    NotInitialised = 7,
}

#[contract]
pub struct VaultRegistry;

#[contractimpl]
impl VaultRegistry {
    /// Initialise the contract with an admin address. Only callable once.
    pub fn init(env: Env, admin: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyRegistered);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        Self::bump_instance(&env);
        Ok(())
    }

    /// Upgrade the contract's WASM. Only the stored admin may call this.
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.deployer()
            .update_current_contract_wasm(new_wasm_hash.clone());
        env.events()
            .publish((symbol_short!("upgrade"),), new_wasm_hash);
        Ok(())
    }

    /// Register a new resource.
    ///
    /// Requires the creator's authorization. The resource is listed by default.
    ///
    /// # Errors
    /// - [`Error::InvalidPrice`] if `price <= 0`.
    /// - [`Error::MetadataTooLong`] if `metadata` exceeds
    ///   [`MAX_METADATA_POINTER_LEN`].
    /// - [`Error::InvalidTag`] if `tags` exceed [`MAX_TAGS`] or any tag is
    ///   empty or exceeds [`MAX_TAG_LEN`].
    /// - [`Error::AlreadyRegistered`] if `id` already exists.
    ///
    /// # Invariants
    /// On success, `Count` is incremented by one (monotonic) and the new
    /// resource plus its index entry have their persistent TTL bumped.
    pub fn register(
        env: Env,
        creator: Address,
        id: String,
        price: i128,
        metadata: String,
        tags: Vec<String>,
    ) -> Result<(), Error> {
        creator.require_auth();
        if price <= 0 {
            return Err(Error::InvalidPrice);
        }
        Self::validate_metadata_pointer(&metadata)?;
        Self::validate_tags(&env, &tags)?;
        let key = DataKey::Resource(id.clone());
        if env.storage().persistent().has(&key) {
            return Err(Error::AlreadyRegistered);
        }

        let resource = Resource {
            id: id.clone(),
            creator: creator.clone(),
            price,
            metadata,
            listed: true, // Resources are listed by default when registered
            tags,
        };
        env.storage().persistent().set(&key, &resource);
        Self::bump_persistent(&env, &key);

        let count: u32 = env.storage().instance().get(&DataKey::Count).unwrap_or(0);
        let idx_key = DataKey::Index(count);
        env.storage().persistent().set(&idx_key, &id);
        Self::bump_persistent(&env, &idx_key);
        env.storage().instance().set(&DataKey::Count, &(count + 1));
        env.storage()
            .instance()
            .set(&DataKey::Version, &String::from_str(&env, VERSION));
        Self::bump_instance(&env);

        env.events()
            .publish((symbol_short!("register"), creator), id);
        Ok(())
    }

    /// Update a resource's price.
    ///
    /// Only the creator may call this (enforced via `require_auth`).
    ///
    /// # Errors
    /// - [`Error::InvalidPrice`] if `new_price <= 0`.
    /// - [`Error::NotFound`] if no resource exists with `id`.
    pub fn set_price(env: Env, id: String, new_price: i128) -> Result<(), Error> {
        if new_price <= 0 {
            return Err(Error::InvalidPrice);
        }
        let mut resource = Self::load(&env, &id)?;
        resource.creator.require_auth();
        resource.price = new_price;
        Self::save(&env, &resource);
        env.events()
            .publish((symbol_short!("setprice"), id), new_price);
        Ok(())
    }

    /// Update a resource's metadata pointer.
    ///
    /// Only the creator may call this (enforced via `require_auth`).
    ///
    /// # Errors
    /// - [`Error::NotFound`] if no resource exists with `id`.
    /// - [`Error::MetadataTooLong`] if `metadata` exceeds
    ///   [`MAX_METADATA_POINTER_LEN`].
    pub fn update_metadata(env: Env, id: String, metadata: String) -> Result<(), Error> {
        let mut resource = Self::load(&env, &id)?;
        resource.creator.require_auth();
        Self::validate_metadata_pointer(&metadata)?;
        resource.metadata = metadata;
        Self::save(&env, &resource);
        env.events().publish((symbol_short!("updmeta"), id), ());
        Ok(())
    }

    /// Replace a resource's discovery tags.
    ///
    /// Only the creator may call this (enforced via `require_auth`). Does not
    /// modify `metadata` (the off-chain content pointer).
    ///
    /// # Errors
    /// - [`Error::InvalidTag`] if `tags` exceed [`MAX_TAGS`] or any tag is
    ///   empty or exceeds [`MAX_TAG_LEN`].
    /// - [`Error::NotFound`] if no resource exists with `id`.
    pub fn set_tags(env: Env, id: String, tags: Vec<String>) -> Result<(), Error> {
        Self::validate_tags(&env, &tags)?;
        let mut resource = Self::load(&env, &id)?;
        resource.creator.require_auth();
        resource.tags = tags.clone();
        Self::save(&env, &resource);
        env.events().publish((symbol_short!("settags"), id), tags);
        Ok(())
    }

    /// Hand ownership to a new creator.
    ///
    /// Only the current creator may call this (enforced via `require_auth`).
    /// The registry `Count` is not affected by transfers.
    ///
    /// # Errors
    /// - [`Error::NotFound`] if no resource exists with `id`.
    pub fn transfer_ownership(env: Env, id: String, new_creator: Address) -> Result<(), Error> {
        let mut resource = Self::load(&env, &id)?;
        resource.creator.require_auth();
        resource.creator = new_creator.clone();
        Self::save(&env, &resource);
        env.events()
            .publish((symbol_short!("transfer"), id), new_creator);
        Ok(())
    }

    /// Set the listing state of a resource.
    ///
    /// Only the creator may call this (enforced via `require_auth`).
    ///
    /// # Errors
    /// - [`Error::NotFound`] if no resource exists with `id`.
    pub fn set_listed(env: Env, id: String, listed: bool) -> Result<(), Error> {
        let mut resource = Self::load(&env, &id)?;
        resource.creator.require_auth();
        resource.listed = listed;
        Self::save(&env, &resource);
        env.events()
            .publish((symbol_short!("setlisted"), id), listed);
        Ok(())
    }

    /// Delist a resource (convenience method for `set_listed(false)`).
    ///
    /// Only the creator may call this (enforced via `require_auth`).
    ///
    /// # Errors
    /// - [`Error::NotFound`] if no resource exists with `id`.
    pub fn delist(env: Env, id: String) -> Result<(), Error> {
        Self::set_listed(env, id, false)
    }

    /// Paginated resource list in insertion order.
    ///
    /// Returns up to `limit` resources starting at insertion index `start`;
    /// `limit` is capped at 20. Missing index or resource entries are skipped.
    pub fn list(env: Env, start: u32, limit: u32) -> Vec<Resource> {
        let total: u32 = env.storage().instance().get(&DataKey::Count).unwrap_or(0);
        let page_size = limit.min(20);
        let mut result: Vec<Resource> = Vec::new(&env);
        let mut i = start;
        while i < total && result.len() < page_size {
            if let Some(id) = env
                .storage()
                .persistent()
                .get::<DataKey, String>(&DataKey::Index(i))
            {
                if let Some(resource) = env
                    .storage()
                    .persistent()
                    .get::<DataKey, Resource>(&DataKey::Resource(id))
                {
                    result.push_back(resource);
                }
            }
            i += 1;
        }
        result
    }

    /// Fetch a resource.
    ///
    /// # Errors
    /// - [`Error::NotFound`] if no resource exists with `id`.
    pub fn get(env: Env, id: String) -> Result<Resource, Error> {
        Self::load(&env, &id)
    }

    /// Whether a resource with `id` is registered.
    pub fn exists(env: Env, id: String) -> bool {
        env.storage().persistent().has(&DataKey::Resource(id))
    }

    /// Get the owner address of a resource.
    ///
    /// # Errors
    /// - [`Error::NotFound`] if no resource exists with `id`.
    pub fn get_owner(env: Env, id: String) -> Result<Address, Error> {
        let resource = Self::load(&env, &id)?;
        Ok(resource.creator)
    }

    /// Total number of resources successfully registered.
    ///
    /// Monotonic: incremented on each successful `register` and never
    /// decremented (including on ownership transfer).
    pub fn count(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::Count).unwrap_or(0)
    }

    /// Return the contract version string. Falls back to the compile-time
    /// `VERSION` constant if no version has been stored yet.
    pub fn get_version(env: Env) -> String {
        env.storage()
            .instance()
            .get(&DataKey::Version)
            .unwrap_or_else(|| String::from_str(&env, VERSION))
    }

    /// Whether `version` is compatible with this contract. A version is
    /// considered compatible when its major/minor components match the
    /// compile-time `VERSION`.
    pub fn is_compatible(version: String) -> bool {
        Self::compatibility_check(&version)
    }
}

impl VaultRegistry {
    fn require_admin(env: &Env) -> Result<(), Error> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialised)?;
        admin.require_auth();
        Ok(())
    }

    fn validate_metadata_pointer(metadata: &String) -> Result<(), Error> {
        if metadata.len() > MAX_METADATA_POINTER_LEN {
            return Err(Error::MetadataTooLong);
        }
        Ok(())
    }

    fn validate_tags(_env: &Env, tags: &Vec<String>) -> Result<(), Error> {
        if tags.len() > MAX_TAGS {
            return Err(Error::InvalidTag);
        }
        for i in 0..tags.len() {
            let tag = tags.get(i).unwrap();
            let len = tag.len();
            if len == 0 || len > MAX_TAG_LEN {
                return Err(Error::InvalidTag);
            }
        }
        Ok(())
    }

    fn load(env: &Env, id: &String) -> Result<Resource, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Resource(id.clone()))
            .ok_or(Error::NotFound)
    }

    fn save(env: &Env, resource: &Resource) {
        let key = DataKey::Resource(resource.id.clone());
        env.storage().persistent().set(&key, resource);
        Self::bump_persistent(env, &key);
    }

    /// Extend a persistent entry's TTL when below threshold (Soroban archival
    /// safety). Called on every persistent write so actively-managed resources
    /// are never archived out from under us.
    fn bump_persistent<K>(env: &Env, key: &K)
    where
        K: IntoVal<Env, Val>,
    {
        env.storage()
            .persistent()
            .extend_ttl(key, LIFETIME_THRESHOLD, BUMP_AMOUNT);
    }

    /// Extend the instance entry's TTL when below threshold. Called alongside
    /// persistent bumps so `Count` and the index stay live.
    fn bump_instance(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(LIFETIME_THRESHOLD, BUMP_AMOUNT);
    }

    /// Compare the major/minor components of `version` against `VERSION`.
    /// Returns true when they match, indicating wire-compatible storage.
    fn compatibility_check(version: &String) -> bool {
        Self::major_minor(version) == Self::major_minor(&String::from_str(&version.env(), VERSION))
    }

    /// Extract the "major.minor" prefix of a dotted version string.
    fn major_minor(version: &String) -> String {
        let env = version.env();
        let mut result = String::from_str(&env, "");
        let mut dots = 0u32;
        for i in 0..version.len() {
            let b = version.get(i).unwrap();
            if b == b'.' {
                dots += 1;
                if dots == 2 {
                    break;
                }
            }
            let mut buf = [0u8; 1];
            buf[0] = b;
            result.push_str(&String::from_bytes(&env, &buf));
        }
        result
    }
}

#[cfg(test)]
pub(crate) const TTL_BUMP_AMOUNT: u32 = BUMP_AMOUNT;

mod test;
