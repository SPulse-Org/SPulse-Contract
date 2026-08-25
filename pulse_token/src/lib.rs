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
pub const INTERFACE_VERSION: u32 = 1;

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
    MaxSupplyExceeded = 7,
    // An attempt to set the max supply below the current total supply.
    CapBelowCurrentSupply = 8,
    // An attempt to raise an already-declared max supply (one-way ratchet).
    CapTooHigh = 9,
    InsufficientAllowance = 10,
    InvalidExpirationLedger = 11,
    ContractPaused = 12,
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
    // ── Supply cap (monetary policy) ─────────────────────────────────────
    MaxSupply, // i128 — hard ceiling on total_supply, enforced at mint time
    Allowance(Address, Address),
    Paused,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllowanceValue {
    pub amount: i128,
    pub expiration_ledger: u32,
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
        max_supply: i128,
    ) -> Result<(), TokenError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(TokenError::AlreadyInitialized);
        }
        if max_supply < 0 {
            return Err(TokenError::InvalidAmount);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Name, &name);
        env.storage().instance().set(&DataKey::Symbol, &symbol);
        env.storage().instance().set(&DataKey::Decimals, &decimals);
        env.storage().instance().set(&DataKey::TotalSupply, &0_i128);
        env.storage()
            .instance()
            .set(&DataKey::MaxSupply, &max_supply);
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

    /// Halt mint/transfer/burn in an emergency. Admin only. View functions
    /// (balance, total_supply, ...) keep working so integrators can still
    /// read state while the contract is paused.
    pub fn pause(env: Env, admin: Address) -> Result<(), TokenError> {
        let stored = Self::require_admin(&env)?;
        if admin != stored {
            return Err(TokenError::NotAdmin);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Paused, &true);
        Ok(())
    }

    /// Resume mint/transfer/burn. Admin only.
    pub fn unpause(env: Env, admin: Address) -> Result<(), TokenError> {
        let stored = Self::require_admin(&env)?;
        if admin != stored {
            return Err(TokenError::NotAdmin);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Paused, &false);
        Ok(())
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
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

    // ── Supply cap (monetary policy) ───────────────────────────────────────
    // MAX_SUPPLY is a hard ceiling on total_supply, enforced inside mint().
    // It is set once at deploy time (initialize) and can subsequently only be
    // lowered by the admin (one-way ratchet) — it can never be raised, so an
    // admin cannot mint their way past the declared monetary policy.

    /// Read the current max supply ceiling.
    ///
    /// `0` means the contract has no declared cap (a legacy deployment that
    /// was never migrated via `set_max_supply`). Minting fails closed in that
    /// state with `MaxSupplyExceeded`.
    pub fn max_supply(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::MaxSupply)
            .unwrap_or(0)
    }

    /// Lower the max supply ceiling. Admin only. One-way ratchet.
    ///
    /// The cap can never be raised once declared, and can never be set below
    /// the current total_supply (that would invalidate already-minted tokens).
    /// This is how a legacy deployment (max_supply == 0) declares a cap for
    /// the first time without allowing it to be inflated afterwards.
    pub fn set_max_supply(env: Env, admin: Address, new_cap: i128) -> Result<(), TokenError> {
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(TokenError::NotInitialized)?;
        if admin != stored {
            return Err(TokenError::NotAdmin);
        }
        admin.require_auth();
        let current_supply: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalSupply)
            .unwrap_or(0);
        if new_cap < current_supply {
            return Err(TokenError::CapBelowCurrentSupply);
        }
        let current_cap: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MaxSupply)
            .unwrap_or(0);
        if current_cap != 0 && new_cap > current_cap {
            return Err(TokenError::CapTooHigh);
        }
        env.storage().instance().set(&DataKey::MaxSupply, &new_cap);
        Ok(())
    }

    /// Mint `amount` PULSE to `to`. Authorized minter only.
    ///
    /// The entire mint succeeds or fails atomically against MAX_SUPPLY: the
    /// resulting supply is computed with checked arithmetic first and the
    /// balance/total_supply are written only if the cap is not exceeded. A
    /// failed mint never partially updates balances or supply.
    pub fn mint(env: Env, minter: Address, to: Address, amount: i128) -> Result<(), TokenError> {
        Self::require_not_paused(&env)?;
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
        let supply: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalSupply)
            .unwrap_or(0);
        let cap: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MaxSupply)
            .unwrap_or(0);
        // checked_add wraps instead of panicking — treat overflow as a cap
        // violation rather than letting arithmetic bypass the ceiling.
        let new_supply: i128 = match supply.checked_add(amount) {
            Some(v) => v,
            None => return Err(TokenError::MaxSupplyExceeded),
        };
        if cap == 0 || new_supply > cap {
            return Err(TokenError::MaxSupplyExceeded);
        }
        let balance = Self::balance(env.clone(), to.clone());
        env.storage()
            .persistent()
            .set(&DataKey::Balance(to), &(balance + amount));
        env.storage()
            .instance()
            .set(&DataKey::TotalSupply, &new_supply);
        Ok(())
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) -> Result<(), TokenError> {
        Self::require_not_paused(&env)?;
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

    /// Allow `spender` to transfer up to `amount` of `from`'s PULSE, until
    /// `expiration_ledger` (inclusive). Pass `amount == 0` to revoke.
    pub fn approve(
        env: Env,
        from: Address,
        spender: Address,
        amount: i128,
        expiration_ledger: u32,
    ) -> Result<(), TokenError> {
        if amount < 0 {
            return Err(TokenError::InvalidAmount);
        }
        from.require_auth();

        if amount > 0 && expiration_ledger < env.ledger().sequence() {
            return Err(TokenError::InvalidExpirationLedger);
        }

        let key = DataKey::Allowance(from, spender);
        if amount == 0 {
            env.storage().temporary().remove(&key);
            return Ok(());
        }

        let value = AllowanceValue {
            amount,
            expiration_ledger,
        };
        env.storage().temporary().set(&key, &value);
        let live_for = expiration_ledger
            .saturating_sub(env.ledger().sequence());
        env.storage()
            .temporary()
            .extend_ttl(&key, live_for, live_for);
        Ok(())
    }

    /// Amount `spender` is still allowed to transfer on `from`'s behalf.
    /// Returns 0 once the allowance has expired.
    pub fn allowance(env: Env, from: Address, spender: Address) -> i128 {
        let key = DataKey::Allowance(from, spender);
        match env.storage().temporary().get::<DataKey, AllowanceValue>(&key) {
            Some(allowance) if allowance.expiration_ledger >= env.ledger().sequence() => {
                allowance.amount
            }
            _ => 0,
        }
    }

    /// Transfer `amount` from `from` to `to`, spending down the allowance
    /// previously granted to `spender` via `approve`.
    pub fn transfer_from(
        env: Env,
        spender: Address,
        from: Address,
        to: Address,
        amount: i128,
    ) -> Result<(), TokenError> {
        if amount <= 0 {
            return Err(TokenError::InvalidAmount);
        }
        spender.require_auth();

        let current_allowance = Self::allowance(env.clone(), from.clone(), spender.clone());
        if current_allowance < amount {
            return Err(TokenError::InsufficientAllowance);
        }

        let from_balance = Self::balance(env.clone(), from.clone());
        if from_balance < amount {
            return Err(TokenError::InsufficientBalance);
        }

        let key = DataKey::Allowance(from.clone(), spender);
        let remaining = current_allowance - amount;
        if remaining == 0 {
            env.storage().temporary().remove(&key);
        } else {
            let expiration_ledger: u32 = env
                .storage()
                .temporary()
                .get::<DataKey, AllowanceValue>(&key)
                .map(|v| v.expiration_ledger)
                .unwrap_or(env.ledger().sequence());
            env.storage().temporary().set(
                &key,
                &AllowanceValue {
                    amount: remaining,
                    expiration_ledger,
                },
            );
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
        Self::require_not_paused(&env)?;
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

    fn require_not_paused(env: &Env) -> Result<(), TokenError> {
        if Self::is_paused(env.clone()) {
            return Err(TokenError::ContractPaused);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
