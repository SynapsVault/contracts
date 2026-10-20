#![no_std]

use soroban_sdk::{contracterror, Address, Env};

/// Number of seconds in a day.
pub const DAY: u64 = 86_400;

/// Canonical bump amount used for persistent storage entries.
pub const BUMP: u32 = 100_000;

/// Canonical bump threshold used for persistent storage entries.
pub const BUMP_THRESH: u32 = 1_000_000;

/// Number of ledgers per day, assuming ~5 second ledger close time.
pub const LEDGERS_PER_DAY: u32 = 17_280;

/// Safety margin (in days) added on top of a lease duration when computing
/// the persistent TTL bump, so a valid lease never archives before its
/// `expires_at`.
pub const LEASE_TTL_MARGIN_DAYS: u32 = 30;

/// Canonical contract version reported by `get_version`.
pub const VERSION: u32 = 1;

/// Errors returned by the shared helpers.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum CommonError {
    /// The caller is not the stored admin.
    Unauthorized = 1,
}

/// Require that `admin` has authorized the current invocation.
///
/// Panics with [`CommonError::Unauthorized`] when the stored admin has not
/// authorized the call. Callers are expected to have already resolved the
/// stored admin address and pass it in.
pub fn require_admin(env: &Env, admin: &Address) {
    admin.require_auth();
    let _ = env;
}

/// Return the canonical contract version.
pub fn get_version(_env: &Env) -> u32 {
    VERSION
}

/// Return whether `version` is compatible with the canonical version.
///
/// A version is compatible when it is greater than zero and no newer than
/// the canonical [`VERSION`].
pub fn is_compatible(version: u32) -> bool {
    version > 0 && version <= VERSION
}

/// Compute a persistent TTL bump (in ledgers) that covers the full lease
/// duration plus a safety margin.
///
/// `duration_secs` is the lease length in seconds. The returned bump is at
/// least [`BUMP`] and grows with the lease so that a valid lease is never
/// archived before its `expires_at`.
pub fn lease_ttl_bump(duration_secs: u64) -> u32 {
    let days = duration_secs / DAY;
    let extra_days = days.saturating_add(LEASE_TTL_MARGIN_DAYS as u64);
    let ledgers = extra_days.saturating_mul(LEDGERS_PER_DAY as u64);
    let ledgers = if ledgers > u32::MAX as u64 {
        u32::MAX
    } else {
        ledgers as u32
    };
    if ledgers > BUMP {
        ledgers
    } else {
        BUMP
    }
}
