#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, vec, Address, BytesN, Env, String, Symbol,
    Val, Vec,
};

// Issue #84: bump whenever a function signature, argument order, or return
// type that a caller relies on changes, so a caller pinning this version can
// detect an incompatible upgrade before invoking.
pub const INTERFACE_VERSION: u32 = 1;

// Issue #100: hard cap on the total PULSE supply (1_000_000_000 tokens with
// 7 decimals). This bounds the combined welcome-bonus + betting-reward minting
// forever, so no parameter combination (referral depth, bet count, points) can
// inflate supply without bound.
const MAX_SUPPLY: i128 = 1_000_000_000_0000000;
const _: () = assert!(MAX_SUPPLY > 0);

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
    InsufficientAllowance = 7,
    InvalidExpirationLedger = 8,
    // Issue #95: operation blocked because the contract is paused.
    Paused = 9,
    ContractPaused = 9,
    AlreadyMinter = 10,
    NotMinter = 11,
    MinterListFull = 12,
    // Issue #5: timelocked + multi-sig upgrade governance.
    NotGovernor = 13,
    UpgradeExists = 14,
    NoUpgrade = 15,
    UpgradeTooSoon = 16,
    AlreadyApproved = 17,
    InsufficientApprovals = 18,
    InvalidThreshold = 19,
    // Issue #100: mint would exceed the hard supply cap.
    SupplyCapExceeded = 20,
}

// TTL: ~1yr threshold, ~2yr extend
const TTL_BUMP: u32 = 3_153_600;
const TTL_HIGH: u32 = 6_307_200;
const MAX_MINTERS: u32 = 10;
const MAX_GOVERNORS: u32 = 10;
// Issue #5: 24h timelock between upgrade proposal and execution.
const UPGRADE_DELAY_SECS: u64 = 86_400;

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
    Allowance(Address, Address),
    Paused,
    MinterAt(u32),
    MinterCount,
    MinterIndex(Address),
    // Issue #5: governance for timelocked WASM upgrades.
    Governor(Address),
    GovernorCount,
    GovernorThreshold,
    PendingUpgrade,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllowanceValue {
    pub amount: i128,
    pub expiration_ledger: u32,
}

/// Timelocked, multi-sig WASM upgrade (issue #5).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingUpgradeChange {
    pub new_wasm_hash: BytesN<32>,
    pub requested_at: u64,
    pub approvers: Vec<Address>,
}

// Emitted when an upgrade is proposed, so off-chain monitors can alert.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradeStaged {
    pub pending_at: u64,
    pub new_wasm_hash: BytesN<32>,
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
        // Bootstrap governance: the initializer is the first governor with
        // a 1-of-1 threshold. Production deploys should add more governors
        // and raise the threshold before relying on upgrades.
        env.storage()
            .persistent()
            .set(&DataKey::Governor(admin.clone()), &true);
        env.storage().instance().set(&DataKey::GovernorCount, &1_u32);
        env.storage()
            .instance()
            .set(&DataKey::GovernorThreshold, &1_u32);
        env.events().publish(
            (Symbol::new(&env, "initialized"), admin),
            (name, symbol, decimals),
        );
        Ok(())
    }

    // ── Timelocked WASM upgrade (issue #5) ───────────────────────────────
    // A single compromised admin key can no longer atomically replace the
    // contract's bytecode. The upgrade must be: (1) proposed by a governor,
    // (2) approved by GovernorThreshold governors, and (3) mature past
    // UPGRADE_DELAY_SECS before execution. Any governor may cancel in
    // between. Off-chain monitors receive an UpgradeStaged event.

    /// Propose a WASM upgrade. Does NOT take effect immediately.
    pub fn propose_upgrade(
        env: Env,
        caller: Address,
        new_wasm_hash: BytesN<32>,
    ) -> Result<(), TokenError> {
        caller.require_auth();
        Self::require_governor(&env, &caller)?;
        if env.storage().instance().has(&DataKey::PendingUpgrade) {
            return Err(TokenError::UpgradeExists);
        }
        let mut approvers: Vec<Address> = Vec::new(&env);
        approvers.push_back(caller.clone());
        let pending = PendingUpgradeChange {
            new_wasm_hash: new_wasm_hash.clone(),
            requested_at: env.ledger().timestamp(),
            approvers,
        };
        let staged = UpgradeStaged {
            pending_at: pending.requested_at,
            new_wasm_hash,
        };
        env.storage()
            .instance()
            .set(&DataKey::PendingUpgrade, &pending);
        staged.publish(&env);
        Ok(())
    }

    /// A governor attests a pending WASM upgrade during the dispute window.
    pub fn approve_upgrade(env: Env, caller: Address) -> Result<u32, TokenError> {
        caller.require_auth();
        Self::require_governor(&env, &caller)?;
        let mut pending: PendingUpgradeChange = env
            .storage()
            .instance()
            .get(&DataKey::PendingUpgrade)
            .ok_or(TokenError::NoUpgrade)?;
        if Self::approver_index(&pending.approvers, &caller).is_some() {
            return Err(TokenError::AlreadyApproved);
        }
        pending.approvers.push_back(caller.clone());
        let count = pending.approvers.len();
        env.storage()
            .instance()
            .set(&DataKey::PendingUpgrade, &pending);
        env.events().publish(
            (Symbol::new(&env, "upgrade_ok"), caller),
            count,
        );
        Ok(count)
    }

    /// Activate a matured, sufficiently-approved WASM upgrade.
    pub fn execute_upgrade(env: Env, caller: Address) -> Result<(), TokenError> {
        caller.require_auth();
        Self::require_governor(&env, &caller)?;
        let pending: PendingUpgradeChange = env
            .storage()
            .instance()
            .get(&DataKey::PendingUpgrade)
            .ok_or(TokenError::NoUpgrade)?;

        let now = env.ledger().timestamp();
        if now < pending.requested_at
            || now - pending.requested_at < UPGRADE_DELAY_SECS
        {
            return Err(TokenError::UpgradeTooSoon);
        }
        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::GovernorThreshold)
            .unwrap_or(1);
        if pending.approvers.len() < threshold {
            return Err(TokenError::InsufficientApprovals);
        }

        env.deployer()
            .update_current_contract_wasm(pending.new_wasm_hash);
        env.storage().instance().remove(&DataKey::PendingUpgrade);
        env.events().publish(
            (Symbol::new(&env, "upgrade_exec"), caller),
            pending.new_wasm_hash,
        );
        Ok(())
    }

    /// Cancel a pending WASM upgrade during the dispute window.
    pub fn cancel_upgrade(env: Env, caller: Address) -> Result<(), TokenError> {
        caller.require_auth();
        Self::require_governor(&env, &caller)?;
        if !env.storage().instance().has(&DataKey::PendingUpgrade) {
            return Err(TokenError::NoUpgrade);
        }
        env.storage().instance().remove(&DataKey::PendingUpgrade);
        env.events().publish(
            (Symbol::new(&env, "upgrade_can"), caller),
            1_u32,
        );
        Ok(())
    }

    /// Read the currently staged (not yet effective) upgrade, if any.
    pub fn get_pending_upgrade(env: Env) -> Option<PendingUpgradeChange> {
        env.storage().instance().get(&DataKey::PendingUpgrade)
    }

    // ── Governor management (issue #5) ──────────────────────────────────

    pub fn add_governor(env: Env, admin: Address, governor: Address) -> Result<(), TokenError> {
        let stored = Self::require_admin(&env)?;
        if admin != stored {
            return Err(TokenError::NotAdmin);
        }
        admin.require_auth();
        let key = DataKey::Governor(governor.clone());
        if env.storage().persistent().get(&key).unwrap_or(false) {
            return Ok(());
        }
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::GovernorCount)
            .unwrap_or(0);
        if count >= MAX_GOVERNORS {
            return Err(TokenError::InvalidThreshold);
        }
        env.storage().persistent().set(&key, &true);
        env.storage()
            .instance()
            .set(&DataKey::GovernorCount, &(count + 1));
        Ok(())
    }

    pub fn remove_governor(env: Env, admin: Address, governor: Address) -> Result<(), TokenError> {
        let stored = Self::require_admin(&env)?;
        if admin != stored {
            return Err(TokenError::NotAdmin);
        }
        admin.require_auth();
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::GovernorCount)
            .unwrap_or(0);
        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::GovernorThreshold)
            .unwrap_or(1);
        if count <= threshold {
            return Err(TokenError::InvalidThreshold);
        }
        let key = DataKey::Governor(governor);
        if !env.storage().persistent().get(&key).unwrap_or(false) {
            return Err(TokenError::NotGovernor);
        }
        env.storage().persistent().remove(&key);
        env.storage()
            .instance()
            .set(&DataKey::GovernorCount, &(count - 1));
        Ok(())
    }

    pub fn set_governor_threshold(
        env: Env,
        admin: Address,
        threshold: u32,
    ) -> Result<(), TokenError> {
        let stored = Self::require_admin(&env)?;
        if admin != stored {
            return Err(TokenError::NotAdmin);
        }
        admin.require_auth();
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::GovernorCount)
            .unwrap_or(0);
        if threshold == 0 || threshold > count {
            return Err(TokenError::InvalidThreshold);
        }
        env.storage()
            .instance()
            .set(&DataKey::GovernorThreshold, &threshold);
        Ok(())
    }

    pub fn is_governor(env: Env, account: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::Governor(account))
            .unwrap_or(false)
    }

    pub fn get_governor_threshold(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::GovernorThreshold)
            .unwrap_or(1)
    }

    pub fn get_governor_count(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::GovernorCount)
            .unwrap_or(0)
    }

    /// The cross-contract ABI version this deployment implements (issue #84).
    pub fn interface_version(_env: Env) -> u32 {
        INTERFACE_VERSION
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
        env.events().publish((Symbol::new(&env, "paused"), admin), true);
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
        env.events().publish((Symbol::new(&env, "unpaused"), admin), true);
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
        if env
            .storage()
            .persistent()
            .get::<_, bool>(&DataKey::AuthorizedMinter(minter.clone()))
            .unwrap_or(false)
        {
            return Err(TokenError::AlreadyMinter);
        }
        env.storage()
            .persistent()
            .set(&DataKey::AuthorizedMinter(minter.clone()), &true);
        // Track in the audit list
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MinterCount)
            .unwrap_or(0);
        if count >= MAX_MINTERS {
            return Err(TokenError::MinterListFull);
        }
        env.storage()
            .persistent()
            .set(&DataKey::MinterIndex(minter.clone()), &count);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::MinterIndex(minter.clone()), TTL_BUMP, TTL_HIGH);
        env.storage()
            .instance()
            .set(&DataKey::MinterAt(count), &minter);
        env.storage()
            .instance()
            .set(&DataKey::MinterCount, &(count + 1));
        env.events().publish((Symbol::new(&env, "minter_added"), minter), true);
        Ok(())
    }

    pub fn remove_minter(env: Env, minter: Address) -> Result<(), TokenError> {
        let admin: Address = Self::require_admin(&env)?;
        admin.require_auth();
        if !env
            .storage()
            .persistent()
            .get::<_, bool>(&DataKey::AuthorizedMinter(minter.clone()))
            .unwrap_or(false)
        {
            return Err(TokenError::NotMinter);
        }
        env.storage()
            .persistent()
            .remove(&DataKey::AuthorizedMinter(minter));
        Ok(())
    }

    /// Issue #95 circuit breaker: pause all supply-changing operations while
    /// an emergency is handled. The caller must be the admin; idempotent.
    /// Transfers/allowances stay available so user funds are never locked.
    pub fn set_paused(env: Env, caller: Address, paused: bool) -> Result<(), TokenError> {
        let admin: Address = Self::require_admin(&env)?;
        if caller != admin {
            return Err(TokenError::NotAdmin);
        }
        caller.require_auth();
        env.storage().instance().set(&DataKey::Paused, &paused);
        Ok(())
    }

    pub fn paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get::<_, bool>(&DataKey::Paused)
            .unwrap_or(false)
    }

    pub fn mint(env: Env, minter: Address, to: Address, amount: i128) -> Result<(), TokenError> {
        if Self::paused(env.clone()) {
            return Err(TokenError::Paused);
        }
    pub fn get_authorized_minters(env: Env) -> soroban_sdk::Vec<Address> {
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MinterCount)
            .unwrap_or(0);
        let mut result: soroban_sdk::Vec<Address> = soroban_sdk::Vec::new(&env);
        for i in 0..count {
            if let Some(addr) = env
                .storage()
                .instance()
                .get::<DataKey, Address>(&DataKey::MinterAt(i))
            {
                if env
                    .storage()
                    .persistent()
                    .get::<_, bool>(&DataKey::AuthorizedMinter(addr.clone()))
                    .unwrap_or(false)
                {
                    result.push_back(addr);
                }
            }
        }
        result
    }

    pub fn mint(env: Env, minter: Address, to: Address, amount: i128) -> Result<(), TokenError> {
        Self::require_not_paused(&env)?;
        if amount <= 0 {
            return Err(TokenError::InvalidAmount);
        }
        minter.require_auth();
        let is_minter: bool = env
            .storage()
            .persistent()
            .get(&DataKey::AuthorizedMinter(minter.clone()))
            .unwrap_or(false);
        if !is_minter {
            return Err(TokenError::UnauthorizedMinter);
        }
        let supply: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalSupply)
            .unwrap_or(0);
        // Issue #100: enforce the hard supply cap before any state change.
        if amount > MAX_SUPPLY.saturating_sub(supply) {
            return Err(TokenError::SupplyCapExceeded);
        }
        let balance = Self::balance(env.clone(), to.clone());
        let to_key = DataKey::Balance(to.clone());
        env.storage()
            .persistent()
            .set(&to_key, &(balance + amount));
        env.storage()
            .persistent()
            .extend_ttl(&to_key, TTL_BUMP, TTL_HIGH);
        env.storage()
            .instance()
            .set(&DataKey::TotalSupply, &(supply + amount));
        env.events().publish(
            (Symbol::new(&env, "mint"), minter, to),
            amount,
        );
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
        let from_key = DataKey::Balance(from.clone());
        env.storage()
            .persistent()
            .set(&from_key, &(from_balance - amount));
        env.storage()
            .persistent()
            .extend_ttl(&from_key, TTL_BUMP, TTL_HIGH);
            .set(&DataKey::Balance(from.clone()), &(from_balance - amount));
        let to_balance = Self::balance(env.clone(), to.clone());
        let to_key = DataKey::Balance(to.clone());
        env.storage()
            .persistent()
            .set(&to_key, &(to_balance + amount));
        env.storage()
            .persistent()
            .extend_ttl(&to_key, TTL_BUMP, TTL_HIGH);
            .set(&DataKey::Balance(to.clone()), &(to_balance + amount));
        env.events().publish(
            (Symbol::new(&env, "transfer"), from, to),
            amount,
        );
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
            .set(&DataKey::Balance(from.clone()), &(from_balance - amount));
        let to_balance = Self::balance(env.clone(), to.clone());
        let to_key = DataKey::Balance(to.clone());
        env.storage()
            .persistent()
            .set(&to_key, &(to_balance + amount));
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Balance(from), TTL_BUMP, TTL_HIGH);
        env.storage()
            .persistent()
            .extend_ttl(&to_key, TTL_BUMP, TTL_HIGH);
            .set(&DataKey::Balance(to.clone()), &(to_balance + amount));
        env.events().publish(
            (Symbol::new(&env, "transfer"), from, to),
            amount,
        );
        Ok(())
    }

    pub fn burn(env: Env, from: Address, amount: i128) -> Result<(), TokenError> {
        if Self::paused(env.clone()) {
            return Err(TokenError::Paused);
        }
        Self::require_not_paused(&env)?;
        if amount <= 0 {
            return Err(TokenError::InvalidAmount);
        }
        from.require_auth();
        let from_balance = Self::balance(env.clone(), from.clone());
        if from_balance < amount {
            return Err(TokenError::InsufficientBalance);
        }
        let from_key = DataKey::Balance(from.clone());
        env.storage()
            .persistent()
            .set(&from_key, &(from_balance - amount));
        env.storage()
            .persistent()
            .extend_ttl(&from_key, TTL_BUMP, TTL_HIGH);
            .set(&DataKey::Balance(from.clone()), &(from_balance - amount));
        let supply: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalSupply)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::TotalSupply, &(supply - amount));
        env.events().publish((Symbol::new(&env, "burn"), from), amount);
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

    fn require_governor(env: &Env, caller: &Address) -> Result<(), TokenError> {
        if env
            .storage()
            .persistent()
            .get(&DataKey::Governor(caller.clone()))
            .unwrap_or(false)
        {
            return Ok(());
        }
        Err(TokenError::NotGovernor)
    }

    fn approver_index(approvers: &Vec<Address>, who: &Address) -> Option<u32> {
        let n = approvers.len();
        for i in 0..n {
            if approvers.get(i).unwrap() == *who {
                return Some(i);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests;
