#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, Address, BytesN, Env, String,
};

// ── Interface versioning (upgrade coordination) ──────────────────────────────
// INTERFACE_VERSION identifies the ABI this contract exposes to its callers
// (the leaderboard invokes mint). It MUST be bumped in the source AND
// committed to storage via set_interface_version() on every incompatible ABI
// change, so callers can fail closed instead of executing against a mismatched
// ABI. Non-zero means the contract declares a version; 0 means uncoordinated.
const INTERFACE_VERSION: u32 = 1;

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum TokenError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    UnauthorizedMinter = 3,
    InsufficientBalance = 4,
    InvalidAmount = 5,
    NotAdmin = 6,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    Admin,
    AuthorizedMinter(Address),
    Balance(Address),
    TotalSupply,
    Name,
    Symbol,
    Decimals,
    // ── Interface versioning (upgrade coordination) ───────────────────────
    InterfaceVersion, // u32 — current interface version of this contract
}

#[contract]
pub struct PULSETokenContract;

#[contractimpl]
impl PULSETokenContract {
    pub fn initialize(
        env: Env,
        admin: Address,
        name: String,
        symbol: String,
        decimals: u32,
    ) -> Result<(), TokenError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(TokenError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Name, &name);
        env.storage().instance().set(&DataKey::Symbol, &symbol);
        env.storage().instance().set(&DataKey::Decimals, &decimals);
        env.storage().instance().set(&DataKey::TotalSupply, &0_i128);
        env.storage()
            .instance()
            .set(&DataKey::InterfaceVersion, &INTERFACE_VERSION);
        Ok(())
    }

    /// Replace this contract's WASM in place. Admin only. Balances preserved.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) -> Result<(), TokenError> {
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(TokenError::NotInitialized)?;
        if admin != stored {
            return Err(TokenError::NotAdmin);
        }
        admin.require_auth();
        env.deployer().update_current_contract_wasm(new_wasm_hash);
        Ok(())
    }

    pub fn set_minter(env: Env, minter: Address) -> Result<(), TokenError> {
        let admin: Address = Self::require_admin(&env)?;
        admin.require_auth();
        env.storage()
            .persistent()
            .set(&DataKey::AuthorizedMinter(minter), &true);
        Ok(())
    }

    pub fn remove_minter(env: Env, minter: Address) -> Result<(), TokenError> {
        let admin: Address = Self::require_admin(&env)?;
        admin.require_auth();
        env.storage()
            .persistent()
            .remove(&DataKey::AuthorizedMinter(minter));
        Ok(())
    }

    // ── Interface Versioning (upgrade coordination) ──────────────────────
    // This contract is invoked cross-contract (leaderboard calls mint), so it
    // exposes a stable interface version that callers verify before invoking.

    /// Read this contract's current interface version.
    ///
    /// `0` means the contract has no declared version (a legacy deployment
    /// that was never migrated via `set_interface_version`, or an
    /// uninitialized contract). Callers treat `0` as incompatible with any
    /// positive requirement — this is the fail-closed behavior for
    /// uncoordinated deployments.
    pub fn interface_version(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::InterfaceVersion)
            .unwrap_or(0)
    }

    /// Declare this contract's interface version. Admin only.
    ///
    /// Required after an in-place WASM upgrade that changes the `mint` ABI the
    /// leaderboard relies on: bump `INTERFACE_VERSION` in the new source and
    /// commit the new value here. Until this is done, upgraded callers that
    /// require the newer version will fail closed with `IncompatibleInterface`
    /// instead of silently executing against an uncoordinated ABI.
    pub fn set_interface_version(env: Env, version: u32) -> Result<(), TokenError> {
        let admin: Address = Self::require_admin(&env)?;
        admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::InterfaceVersion, &version);
        Ok(())
    }

    pub fn mint(env: Env, minter: Address, to: Address, amount: i128) -> Result<(), TokenError> {
        if amount <= 0 {
            return Err(TokenError::InvalidAmount);
        }
        minter.require_auth();
        let is_minter: bool = env
            .storage()
            .persistent()
            .get(&DataKey::AuthorizedMinter(minter))
            .unwrap_or(false);
        if !is_minter {
            return Err(TokenError::UnauthorizedMinter);
        }
        let balance = Self::balance(env.clone(), to.clone());
        env.storage()
            .persistent()
            .set(&DataKey::Balance(to), &(balance + amount));
        let supply: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalSupply)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::TotalSupply, &(supply + amount));
        Ok(())
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) -> Result<(), TokenError> {
        if amount <= 0 {
            return Err(TokenError::InvalidAmount);
        }
        from.require_auth();
        let from_balance = Self::balance(env.clone(), from.clone());
        if from_balance < amount {
            return Err(TokenError::InsufficientBalance);
        }
        env.storage()
            .persistent()
            .set(&DataKey::Balance(from), &(from_balance - amount));
        let to_balance = Self::balance(env.clone(), to.clone());
        env.storage()
            .persistent()
            .set(&DataKey::Balance(to), &(to_balance + amount));
        Ok(())
    }

    pub fn burn(env: Env, from: Address, amount: i128) -> Result<(), TokenError> {
        if amount <= 0 {
            return Err(TokenError::InvalidAmount);
        }
        from.require_auth();
        let from_balance = Self::balance(env.clone(), from.clone());
        if from_balance < amount {
            return Err(TokenError::InsufficientBalance);
        }
        env.storage()
            .persistent()
            .set(&DataKey::Balance(from), &(from_balance - amount));
        let supply: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalSupply)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::TotalSupply, &(supply - amount));
        Ok(())
    }

    pub fn balance(env: Env, account: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::Balance(account))
            .unwrap_or(0)
    }

    pub fn total_supply(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::TotalSupply)
            .unwrap_or(0)
    }

    pub fn name(env: Env) -> String {
        env.storage()
            .instance()
            .get(&DataKey::Name)
            .unwrap_or_else(|| String::from_str(&env, "PULSE"))
    }

    pub fn symbol(env: Env) -> String {
        env.storage()
            .instance()
            .get(&DataKey::Symbol)
            .unwrap_or_else(|| String::from_str(&env, "PLSE"))
    }

    pub fn decimals(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Decimals)
            .unwrap_or(7)
    }

    fn require_admin(env: &Env) -> Result<Address, TokenError> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(TokenError::NotInitialized)
    }
}

#[cfg(test)]
mod tests;
