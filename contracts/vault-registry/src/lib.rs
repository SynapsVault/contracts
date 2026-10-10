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
    contract, contracterror, contractevent, contractimpl, contracttype, Address, BytesN,
    ContractExecutable, Env, IntoVal, String, Val, Vec,
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
pub const MAX_TAGS: u32 = 8;
/// Maximum byte length of a single discovery tag. Tags must be non-empty and
/// no longer than this; violations are rejected with [`Error::InvalidTag`].
pub const MAX_TAG_LEN: u32 = 32;

/// Maximum page size returned by [`VaultRegistry::list`].
pub const MAX_PAGE_SIZE: u32 = 20;
/// Contract version, sourced from the crate manifest at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Minimum version this contract is compatible with.
pub const MIN_COMPATIBLE_VERSION: &str = "0.1.0";

/// A registered vault resource.
///
/// Invariants:
/// - `id` is unique across the registry and immutable once registered.
/// - `price` is strictly positive (in USDC stroops, 7 decimals).
/// - `metadata.len() <= MAX_METADATA_POINTER_LEN`.
/// - `tags.len() <= MAX_TAGS`, each tag non-empty and `<= MAX_TAG_LEN` bytes.
/// - Only `creator` may mutate the resource (enforced via `require_auth`).
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
    /// Admin address allowed to upgrade the contract (instance storage).
    Admin,
    /// Contract version recorded at `init` (instance storage).
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
    /// The caller is not the configured admin.
    NotAdmin = 6,
    /// `init` has not been called, so no admin is configured.
    NotInitialised = 7,
    /// `init` has already been called; the admin cannot be overwritten.
    AlreadyInitialised = 8,
}

/// Emitted by `init`. Topics: `("init",)`; data: admin.
#[contractevent(topics = ["init"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitEvent {
    pub admin: Address,
}

/// Emitted by `upgrade`. Topics: `("upgrade",)`; data: new WASM hash.
#[contractevent(topics = ["upgrade"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradeEvent {
    pub new_wasm_hash: BytesN<32>,
}

/// Emitted by `register`. Topics: `("register", creator)`; data: resource id.
#[contractevent(topics = ["register"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterEvent {
    #[topic]
    pub creator: Address,
    pub id: String,
}

/// Emitted by `set_price`. Topics: `("setprice", id)`; data: new price.
#[contractevent(topics = ["setprice"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetPriceEvent {
    #[topic]
    pub id: String,
    pub price: i128,
}

/// Emitted by `update_metadata`. Topics: `("updmeta", id)`; no data.
#[contractevent(topics = ["updmeta"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateMetadataEvent {
    #[topic]
    pub id: String,
}

/// Emitted by `set_tags`. Topics: `("settags", id)`; data: tags.
#[contractevent(topics = ["settags"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetTagsEvent {
    #[topic]
    pub id: String,
    pub tags: Vec<String>,
}

/// Emitted by `transfer_ownership`. Topics: `("transfer", id)`; data: new creator.
#[contractevent(topics = ["transfer"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferEvent {
    #[topic]
    pub id: String,
    pub new_creator: Address,
}

/// Emitted by `set_listed`. Topics: `("setlisted", id)`; data: listed flag.
#[contractevent(topics = ["setlisted"], data_format = "single-value")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetListedEvent {
    #[topic]
    pub id: String,
    pub listed: bool,
}

#[contract]
pub struct VaultRegistry;

#[contractimpl]
impl VaultRegistry {
    /// Initialise the contract with an admin address. Only callable once.
    ///
    /// The admin is only needed for [`upgrade`](VaultRegistry::upgrade); the registry
    /// itself is usable without initialisation.
    ///
    /// # Errors
    /// - [`Error::AlreadyInitialised`] if an admin is already set.
    pub fn init(env: Env, admin: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialised);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Version, &String::from_str(&env, VERSION));
        Self::bump_instance(&env);
        InitEvent { admin }.publish(&env);
        Ok(())
    }

    /// Return the configured admin address.
    ///
    /// # Errors
    /// - [`Error::NotInitialised`] if `init` has not been called.
    pub fn admin(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialised)
    }

    /// Upgrade the contract's WASM. Only the stored admin may call this.
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.deployer()
            .update_current_contract(ContractExecutable::Wasm(new_wasm_hash.clone()));
        UpgradeEvent { new_wasm_hash }.publish(&env);
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
        Self::validate_tags(&tags)?;
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
        Self::bump_instance(&env);

        RegisterEvent { creator, id }.publish(&env);
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
        SetPriceEvent {
            id,
            price: new_price,
        }
        .publish(&env);
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
        UpdateMetadataEvent { id }.publish(&env);
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
        Self::validate_tags(&tags)?;
        let mut resource = Self::load(&env, &id)?;
        resource.creator.require_auth();
        resource.tags = tags.clone();
        Self::save(&env, &resource);
        SetTagsEvent { id, tags }.publish(&env);
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
        TransferEvent { id, new_creator }.publish(&env);
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
        SetListedEvent { id, listed }.publish(&env);
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
    /// `limit` is capped at [`MAX_PAGE_SIZE`]. Missing index or resource
    /// entries are skipped.
    pub fn list(env: Env, start: u32, limit: u32) -> Vec<Resource> {
        let total: u32 = env.storage().instance().get(&DataKey::Count).unwrap_or(0);
        let page_size = limit.min(MAX_PAGE_SIZE);
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

    /// Return the compile-time contract version of the running WASM.
    pub fn version(env: Env) -> String {
        String::from_str(&env, VERSION)
    }

    /// Return the version recorded at `init`. Falls back to the compile-time
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

    fn validate_tags(tags: &Vec<String>) -> Result<(), Error> {
        if tags.len() > MAX_TAGS {
            return Err(Error::InvalidTag);
        }
        for tag in tags.iter() {
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
        const BUF_LEN: usize = 32;
        let len = version.len() as usize;
        if len == 0 || len > BUF_LEN {
            return false;
        }
        let mut buf = [0u8; BUF_LEN];
        version.copy_into_slice(&mut buf[..len]);
        Self::major_minor(&buf[..len]) == Self::major_minor(VERSION.as_bytes())
    }

    /// Return the "major.minor" prefix of a dotted version string.
    fn major_minor(version: &[u8]) -> &[u8] {
        let mut dots = 0u32;
        for (i, b) in version.iter().enumerate() {
            if *b == b'.' {
                dots += 1;
                if dots == 2 {
                    return &version[..i];
                }
            }
        }
        version
    }
}

#[cfg(test)]
pub(crate) const TTL_BUMP_AMOUNT: u32 = BUMP_AMOUNT;

mod test;
