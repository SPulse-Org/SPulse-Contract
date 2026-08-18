#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, vec, Address, BytesN, Env,
    Error, IntoVal, Symbol, Val, Vec,
};

const MAX_TOP_PLAYERS: u32 = 50;
const TTL_BUMP: u32 = 3_153_600;
const TTL_HIGH: u32 = 6_307_200;

// ── Interface versioning (upgrade coordination) ──────────────────────────────
// INTERFACE_VERSION identifies the ABI this contract exposes to its callers
// (prediction_market -> reward, referral_registry -> reward_bonus/add_bonus_pts).
// It MUST be bumped in the source AND committed to storage via
// set_interface_version() on every incompatible ABI change, so callers can
// fail closed instead of executing against a mismatched ABI.
const INTERFACE_VERSION: u32 = 1;
// Minimum interface version required from the PULSE token before invoking
// mint, either from reward() or reward_bonus().
const TOKEN_MINT_INTERFACE_VERSION: u32 = 1;

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum LeaderboardError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    UnauthorizedCaller = 3,
    InvalidPoints = 4,
    NotAdmin = 5,
    // ── Interface versioning (upgrade coordination) ────────────────────────
    // The target contract does not expose the interface_version() function.
    InterfaceVersionMissing = 6,
    // The target contract's interface version is below the required minimum.
    IncompatibleInterface = 7,
    // ── Revocable trust model (issue #40) ─────────────────────────────────
    // The trusted contract for a role was revoked by the admin; every operation
    // that depends on that role fails closed until restore_trust is called.
    TrustRevoked = 8,
    // revoke_trust/restore_trust was called with an unknown role symbol.
    InvalidRole = 9,
}

// OPT: was 4 separate keys per user (Points, TotalBets, WonBets, LostBets).
//      Now 1 key per user (Stats) — saves 3 storage reads + 3 writes on
//      every add_pts call and 3 reads on every get_stats call.
//      TopPlayerSlot retained as a reverse lookup for O(1) in-place update.
//      TopPlayerCount moves to instance storage (free to read with other keys).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    Admin,
    MarketContract,
    ReferralContract,
    // Lever G: token address so reward() can mint PULSE internally — one
    // cross-call from the market instead of two (add_pts + mint).
    TokenContract,
    Stats(Address), // was: Points + TotalBets + WonBets + LostBets (4 keys → 1)
    TopPlayerAt(u32),
    TopPlayerCount,
    TopPlayerSlot(Address),
    MinPoints, // u64 — points of the weakest entry currently in the top list
    MinSlot,   // u32 — slot index of that weakest entry
    // ── Interface versioning (upgrade coordination) ───────────────────────
    InterfaceVersion, // u32 — current interface version of this contract
    // ── Revocable trust model (issue #40) ─────────────────────────────────
    // Persistent flag (true) meaning the trusted contract for `role` is
    // revoked. Absent key == trusted/enabled. Stored in persistent storage so
    // it survives instance-storage rewrites and follows the same TTL
    // management as the rest of the contract's persistent state.
    TrustRevoked(Symbol),
}

// OPT: PlayerEntry now embeds points directly (avoids a Stats read during sort)
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerEntry {
    pub address: Address,
    pub points: u64,
}

// External-facing stats struct (ABI stable)
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerStats {
    pub points: u64,
    // Total activity: settled wins + settled losses + bonus awards.
    pub total_bets: u32,
    pub won_bets: u32,
    pub lost_bets: u32,
}

// Internal packed stats —

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct LeaderboardContract;

#[contractimpl]
impl LeaderboardContract {
    pub fn initialize(
        env: Env,
        admin: Address,
        market_contract: Address,
        referral_contract: Address,
    ) -> Result<(), LeaderboardError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(LeaderboardError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::MarketContract, &market_contract);
        env.storage()
            .instance()
            .set(&DataKey::ReferralContract, &referral_contract);
        env.storage()
            .instance()
            .set(&DataKey::TopPlayerCount, &0_u32);
        env.storage().instance().set(&DataKey::MinPoints, &0_u64);
        env.storage().instance().set(&DataKey::MinSlot, &0_u32);
        env.storage()
            .instance()
            .set(&DataKey::InterfaceVersion, &INTERFACE_VERSION);
        env.storage().instance().extend_ttl(TTL_BUMP, TTL_HIGH);
        Ok(())
    }

    pub fn set_token_contract(
        env: Env,
        admin: Address,
        token: Address,
    ) -> Result<(), LeaderboardError> {
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(LeaderboardError::NotInitialized)?;
        if admin != stored {
            return Err(LeaderboardError::NotAdmin);
        }
        admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::TokenContract, &token);
        env.storage().instance().extend_ttl(TTL_BUMP, TTL_HIGH);
        Ok(())
    }

    /// Replace this contract's WASM bytecode in place. Admin only.
    /// Storage is preserved — only the executable changes. After an upgrade,
    /// call set_interface_version() to declare the new interface version so
    /// callers that require it can proceed.
    pub fn upgrade(
        env: Env,
        admin: Address,
        new_wasm_hash: BytesN<32>,
    ) -> Result<(), LeaderboardError> {
        Self::require_admin(&env, &admin)?;
        admin.require_auth();
        env.deployer().update_current_contract_wasm(new_wasm_hash);
        Ok(())
    }

    /// Historical ABI alias for `set_token_contract`. Admin only.
    pub fn set_token(env: Env, admin: Address, token: Address) -> Result<(), LeaderboardError> {
        Self::set_token_contract(env, admin, token)
    }

    // ── Interface Versioning (upgrade coordination) ──────────────────────
    // Every contract that participates in cross-contract calls exposes a
    // stable interface version. Callers read it and refuse to invoke a
    // dependency whose version is below the minimum they require.

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
    /// Required after an in-place WASM upgrade that changes any ABI another
    /// contract calls: bump `INTERFACE_VERSION` in the new source and commit
    /// the new value here. Until this is done, upgraded callers that require
    /// the newer version will fail closed with `IncompatibleInterface` instead
    /// of silently executing against an uncoordinated ABI.
    pub fn set_interface_version(
        env: Env,
        admin: Address,
        version: u32,
    ) -> Result<(), LeaderboardError> {
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(LeaderboardError::NotInitialized)?;
        if admin != stored {
            return Err(LeaderboardError::NotAdmin);
        }
        admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::InterfaceVersion, &version);
        env.storage().instance().extend_ttl(TTL_BUMP, TTL_HIGH);
        Ok(())
    }

    // ── Revocable trust model (issue #40) ─────────────────────────────────
    // Each trusted dependency (market, referral, token) is tracked as a role.
    // The admin can revoke a role to sever the trust relationship immediately —
    // every operation that depends on it then fails closed with TrustRevoked.
    // Restoration is an explicit, admin-only act; replacing the trusted address
    // (set_contracts/set_token_contract) does NOT re-enable a revoked role, so
    // a rushed re-point cannot silently bypass an emergency revocation.

    /// Revoke trust in the contract bound to `role`. Admin only.
    ///
    /// Roles: "market", "referral", "token". After revocation, all functions
    /// that depend on the role fail closed with `TrustRevoked` until the admin
    /// explicitly calls `restore_trust` (a replacement address via
    /// `set_contracts`/`set_token_contract` does not clear the revocation).
    pub fn revoke_trust(env: Env, admin: Address, role: Symbol) -> Result<(), LeaderboardError> {
        Self::require_admin(&env, &admin)?;
        admin.require_auth();
        if !Self::is_known_role(&env, &role) {
            return Err(LeaderboardError::InvalidRole);
        }
        let key = DataKey::TrustRevoked(role);
        env.storage().persistent().set(&key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_BUMP, TTL_HIGH);
        env.storage().instance().extend_ttl(TTL_BUMP, TTL_HIGH);
        Ok(())
    }

    /// Restore trust in the contract bound to `role`. Admin only.
    ///
    /// Clears a prior `revoke_trust` for the role. The configured address for
    /// the role (current or replaced via set_contracts/set_token_contract) is
    /// trusted again. This is the ONLY way to re-enable a revoked role.
    pub fn restore_trust(env: Env, admin: Address, role: Symbol) -> Result<(), LeaderboardError> {
        Self::require_admin(&env, &admin)?;
        admin.require_auth();
        if !Self::is_known_role(&env, &role) {
            return Err(LeaderboardError::InvalidRole);
        }
        env.storage()
            .persistent()
            .remove(&DataKey::TrustRevoked(role));
        env.storage().instance().extend_ttl(TTL_BUMP, TTL_HIGH);
        Ok(())
    }

    /// Read-only trust status for `role` (true == revoked). Never panics and
    /// never requires admin — usable by anyone to observe revocation state.
    pub fn is_trust_revoked(env: Env, role: Symbol) -> bool {
        env.storage()
            .persistent()
            .get::<DataKey, bool>(&DataKey::TrustRevoked(role))
            .unwrap_or(false)
    }

    fn is_known_role(env: &Env, role: &Symbol) -> bool {
        *role == Symbol::new(env, "market")
            || *role == Symbol::new(env, "referral")
            || *role == Symbol::new(env, "token")
    }

    fn require_trust_not_revoked(env: &Env, role: Symbol) -> Result<(), LeaderboardError> {
        if env
            .storage()
            .persistent()
            .get::<DataKey, bool>(&DataKey::TrustRevoked(role))
            .unwrap_or(false)
        {
            return Err(LeaderboardError::TrustRevoked);
        }
        Ok(())
    }

    pub fn add_pts(
        env: Env,
        caller: Address,
        user: Address,
        pts: u64,
        is_won: bool,
    ) -> Result<(), LeaderboardError> {
        let market: Address = env
            .storage()
            .instance()
            .get(&DataKey::MarketContract)
            .ok_or(LeaderboardError::NotInitialized)?;
        if caller != market {
            return Err(LeaderboardError::UnauthorizedCaller);
        }
        // Issue #40: even the on-record market cannot drive add_pts once its
        // trust role has been revoked by the admin.
        Self::require_trust_not_revoked(&env, symbol_short!("market"))?;
        caller.require_auth();

        let mut stats: PlayerStats = env
            .storage()
            .persistent()
            .get(&DataKey::Stats(user.clone()))
            .unwrap_or(PlayerStats {
                points: 0,
                total_bets: 0,
                won_bets: 0,
                lost_bets: 0,
            });

        stats.points += pts;
        stats.total_bets += 1;
        if is_won {
            stats.won_bets += 1;
        } else {
            stats.lost_bets += 1;
        }

        env.storage()
            .persistent()
            .set(&DataKey::Stats(user.clone()), &stats);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Stats(user.clone()), TTL_BUMP, TTL_HIGH);

        Self::update_top_players(&env, user, stats.points);
        // Instance storage (TopPlayerCount, MinPoints, MinSlot, Admin, etc.)
        // has its own TTL that is never bumped by persistent-key writes above —
        // refresh it on every write so the leaderboard's cached min survives.
        env.storage().instance().extend_ttl(TTL_BUMP, TTL_HIGH);
        Ok(())
    }

    /// Settle a market-resolution reward for `user`: award `points` (and
    /// win/loss activity) and mint `tokens` PULSE to them. Market contract only.
    ///
    /// The PULSE token is minted internally (Lever G), so before the mint this
    /// contract verifies the token exposes a compatible `mint` interface —
    /// failing closed with IncompatibleInterface/InterfaceVersionMissing on a
    /// mismatched or unverifiable upgrade.
    pub fn reward(
        env: Env,
        caller: Address,
        user: Address,
        points: u64,
        tokens: i128,
        is_winner: bool,
    ) -> Result<(), LeaderboardError> {
        caller.require_auth();
        Self::require_market_contract(&env, &caller)?;
        if points == 0 {
            return Err(LeaderboardError::InvalidPoints);
        }

        // Upgrade coordination: verify the PULSE token exposes a compatible
        // `mint` interface BEFORE any local state changes, so a unilateral
        // incompatible upgrade fails the whole call atomically instead of
        // awarding points and only then discovering the ABI mismatch.
        if tokens > 0 {
            // Issue #40: a revoked token role fails the entire reward (points
            // included) — no partial award when the mint that owes the user
            // their PULSE would be refused by a severed trust relationship.
            Self::require_trust_not_revoked(&env, symbol_short!("token"))?;
            let token: Address = env
                .storage()
                .instance()
                .get(&DataKey::TokenContract)
                .ok_or(LeaderboardError::NotInitialized)?;
            Self::require_interface_version(&env, &token, TOKEN_MINT_INTERFACE_VERSION)?;
        }

        let sk = DataKey::Stats(user.clone());
        let mut stats: PlayerStats = env.storage().persistent().get(&sk).unwrap_or(PlayerStats {
            points: 0,
            total_bets: 0,
            won_bets: 0,
            lost_bets: 0,
        });
        stats.points += points;
        stats.total_bets += 1;
        if is_winner {
            stats.won_bets += 1;
        } else {
            stats.lost_bets += 1;
        }
        env.storage().persistent().set(&sk, &stats);
        env.storage()
            .persistent()
            .extend_ttl(&sk, TTL_BUMP, TTL_HIGH);
        Self::update_top_players(&env, user.clone(), stats.points);

        if tokens > 0 {
            let token: Address = env
                .storage()
                .instance()
                .get(&DataKey::TokenContract)
                .ok_or(LeaderboardError::NotInitialized)?;
            // Upgrade coordination: the token mint interface was already
            // verified compatible before the stats were updated.
            let this = env.current_contract_address();
            let _: Val = env.invoke_contract(
                &token,
                &Symbol::new(&env, "mint"),
                vec![
                    &env,
                    this.into_val(&env),
                    user.into_val(&env),
                    tokens.into_val(&env),
                ],
            );
        }
        env.storage().instance().extend_ttl(TTL_BUMP, TTL_HIGH);
        Ok(())
    }

    pub fn add_bonus_pts(
        env: Env,
        caller: Address,
        user: Address,
        pts: u64,
    ) -> Result<(), LeaderboardError> {
        let referral: Address = env
            .storage()
            .instance()
            .get(&DataKey::ReferralContract)
            .ok_or(LeaderboardError::NotInitialized)?;
        if caller != referral {
            return Err(LeaderboardError::UnauthorizedCaller);
        }
        // Issue #40: even the on-record referral cannot drive add_bonus_pts
        // once its trust role has been revoked by the admin.
        Self::require_trust_not_revoked(&env, symbol_short!("referral"))?;
        caller.require_auth();

        let mut stats: PlayerStats = env
            .storage()
            .persistent()
            .get(&DataKey::Stats(user.clone()))
            .unwrap_or(PlayerStats {
                points: 0,
                total_bets: 0,
                won_bets: 0,
                lost_bets: 0,
            });

        stats.points += pts;
        stats.total_bets += 1; // bonus counts as activity

        env.storage()
            .persistent()
            .set(&DataKey::Stats(user.clone()), &stats);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Stats(user.clone()), TTL_BUMP, TTL_HIGH);

        Self::update_top_players(&env, user, stats.points);
        env.storage().instance().extend_ttl(TTL_BUMP, TTL_HIGH);
        Ok(())
    }

    /// Award a referral or welcome bonus to `user`: add `points` and mint
    /// `tokens` PULSE to them. Referral contract only.
    ///
    /// Like reward(), the PULSE token is minted internally, so the token's
    /// `mint` interface version is verified before the invoke — failing closed
    /// on a mismatched or unverifiable upgrade.
    pub fn reward_bonus(
        env: Env,
        caller: Address,
        user: Address,
        points: u64,
        tokens: i128,
    ) -> Result<(), LeaderboardError> {
        caller.require_auth();
        Self::require_referral_contract(&env, &caller)?;
        if points == 0 {
            return Err(LeaderboardError::InvalidPoints);
        }

        // Upgrade coordination: verify the PULSE token exposes a compatible
        // `mint` interface BEFORE any local state changes (see reward()).
        if tokens > 0 {
            // Issue #40: a revoked token role fails the entire reward_bonus —
            // no partial bonus when the mint it owes the user would be refused.
            Self::require_trust_not_revoked(&env, symbol_short!("token"))?;
            let token: Address = env
                .storage()
                .instance()
                .get(&DataKey::TokenContract)
                .ok_or(LeaderboardError::NotInitialized)?;
            Self::require_interface_version(&env, &token, TOKEN_MINT_INTERFACE_VERSION)?;
        }

        let sk = DataKey::Stats(user.clone());
        let mut stats: PlayerStats = env.storage().persistent().get(&sk).unwrap_or(PlayerStats {
            points: 0,
            total_bets: 0,
            won_bets: 0,
            lost_bets: 0,
        });
        stats.points += points;
        stats.total_bets += 1; // bonus counts as activity

        env.storage().persistent().set(&sk, &stats);
        env.storage()
            .persistent()
            .extend_ttl(&sk, TTL_BUMP, TTL_HIGH);
        Self::update_top_players(&env, user.clone(), stats.points);

        if tokens > 0 {
            let token: Address = env
                .storage()
                .instance()
                .get(&DataKey::TokenContract)
                .ok_or(LeaderboardError::NotInitialized)?;
            // Upgrade coordination: the token mint interface was already
            // verified compatible before the stats were updated.
            let this = env.current_contract_address();
            let _: Val = env.invoke_contract(
                &token,
                &Symbol::new(&env, "mint"),
                vec![
                    &env,
                    this.into_val(&env),
                    user.into_val(&env),
                    tokens.into_val(&env),
                ],
            );
        }
        env.storage().instance().extend_ttl(TTL_BUMP, TTL_HIGH);
        Ok(())
    }

    pub fn get_points(env: Env, user: Address) -> u64 {
        env.storage()
            .persistent()
            .get::<_, PlayerStats>(&DataKey::Stats(user))
            .map(|s| s.points)
            .unwrap_or(0)
    }

    pub fn get_stats(env: Env, user: Address) -> PlayerStats {
        env.storage()
            .persistent()
            .get(&DataKey::Stats(user))
            .unwrap_or(PlayerStats {
                points: 0,
                total_bets: 0,
                won_bets: 0,
                lost_bets: 0,
            })
    }

    pub fn get_top_players(env: Env, offset: u32, page_size: u32) -> Vec<PlayerEntry> {
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TopPlayerCount)
            .unwrap_or(0);

        if offset >= count || page_size == 0 {
            return vec![&env];
        }

        let end = (offset + page_size).min(count);
        let mut result = Vec::new(&env);
        for i in offset..end {
            if let Some(entry) = env.storage().persistent().get(&DataKey::TopPlayerAt(i)) {
                result.push_back(entry);
            }
        }
        result
    }

    pub fn get_top_player_count(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::TopPlayerCount)
            .unwrap_or(0)
    }

    pub fn get_min_points(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::MinPoints)
            .unwrap_or(0)
    }

    pub fn get_min_slot(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::MinSlot).unwrap_or(0)
    }

    /// Number of players currently in the top list (≤ MAX_TOP_PLAYERS).
    pub fn get_player_count(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::TopPlayerCount)
            .unwrap_or(0)
    }

    /// 1-based rank of `user` in the top list, or 0 when not present.
    pub fn get_rank(env: Env, user: Address) -> u32 {
        let slot: Option<u32> = env
            .storage()
            .persistent()
            .get(&DataKey::TopPlayerSlot(user.clone()));
        if slot.is_none() {
            return 0;
        }

        let user_pts: u64 = env
            .storage()
            .persistent()
            .get::<_, PlayerStats>(&DataKey::Stats(user.clone()))
            .map(|s| s.points)
            .unwrap_or(0);

        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TopPlayerCount)
            .unwrap_or(0);
        let mut rank: u32 = 1;
        for i in 0..count {
            if let Some(e) = env
                .storage()
                .persistent()
                .get::<_, PlayerEntry>(&DataKey::TopPlayerAt(i))
            {
                if e.address != user && e.points > user_pts {
                    rank += 1;
                }
            }
        }
        rank
    }

    /// Legacy ABI no-op kept for compatibility — total_bets is now derived
    /// from rewarded activity (reward/reward_bonus/add_bonus_pts). Market
    /// contract only; performs no state change.
    pub fn record_bet(env: Env, caller: Address, _user: Address) -> Result<(), LeaderboardError> {
        caller.require_auth();
        Self::require_market_contract(&env, &caller)
    }

    // ── Internal: maintain a persistent sorted top list ──────────────────────

    fn require_admin(env: &Env, caller: &Address) -> Result<(), LeaderboardError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(LeaderboardError::NotInitialized)?;
        if *caller != admin {
            return Err(LeaderboardError::NotAdmin);
        }
        Ok(())
    }

    fn require_market_contract(env: &Env, caller: &Address) -> Result<(), LeaderboardError> {
        let market: Address = env
            .storage()
            .instance()
            .get(&DataKey::MarketContract)
            .ok_or(LeaderboardError::NotInitialized)?;
        if *caller != market {
            return Err(LeaderboardError::UnauthorizedCaller);
        }
        // Issue #40: even an on-record market cannot drive add_pts/reward/
        // record_bet once its trust role has been revoked by the admin.
        Self::require_trust_not_revoked(env, symbol_short!("market"))?;
        Ok(())
    }

    fn require_referral_contract(env: &Env, caller: &Address) -> Result<(), LeaderboardError> {
        let referral: Address = env
            .storage()
            .instance()
            .get(&DataKey::ReferralContract)
            .ok_or(LeaderboardError::NotInitialized)?;
        if *caller != referral {
            return Err(LeaderboardError::UnauthorizedCaller);
        }
        // Issue #40: a revoked referral can no longer drive add_bonus_pts /
        // reward_bonus.
        Self::require_trust_not_revoked(env, symbol_short!("referral"))?;
        Ok(())
    }

    // ── Interface versioning helpers (upgrade coordination) ──────────────
    // Reads `interface_version()` on the target contract and fails closed with
    // a typed error when the target does not expose the function
    // (InterfaceVersionMissing) or reports a version below the required
    // minimum (IncompatibleInterface). Uses try_invoke_contract so a missing
    // function or panicking target is caught deterministically instead of
    // aborting with a generic host panic.
    fn require_interface_version(
        env: &Env,
        target: &Address,
        required: u32,
    ) -> Result<(), LeaderboardError> {
        let version: u32 = match env.try_invoke_contract::<u32, Error>(
            target,
            &Symbol::new(env, "interface_version"),
            vec![&env],
        ) {
            Ok(Ok(v)) => v,
            Ok(Err(_)) | Err(_) => return Err(LeaderboardError::InterfaceVersionMissing),
        };
        if version < required {
            return Err(LeaderboardError::IncompatibleInterface);
        }
        Ok(())
    }

    fn update_top_players(env: &Env, user: Address, new_points: u64) {
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TopPlayerCount)
            .unwrap_or(0);

        // If user is already in the list, update their points and re-sort in place.
        if let Some(slot) = env
            .storage()
            .persistent()
            .get::<_, u32>(&DataKey::TopPlayerSlot(user.clone()))
        {
            let mut entry: PlayerEntry = env
                .storage()
                .persistent()
                .get(&DataKey::TopPlayerAt(slot))
                .unwrap();
            entry.points = new_points;
            env.storage()
                .persistent()
                .set(&DataKey::TopPlayerAt(slot), &entry);
            env.storage()
                .persistent()
                .extend_ttl(&DataKey::TopPlayerAt(slot), TTL_BUMP, TTL_HIGH);

            // Bubble the updated entry up to maintain descending order.
            let mut current = slot;
            while current > 0 {
                let prev: PlayerEntry = env
                    .storage()
                    .persistent()
                    .get(&DataKey::TopPlayerAt(current - 1))
                    .unwrap();
                if prev.points < entry.points {
                    // Swap
                    env.storage()
                        .persistent()
                        .set(&DataKey::TopPlayerAt(current - 1), &entry);
                    env.storage()
                        .persistent()
                        .set(&DataKey::TopPlayerAt(current), &prev);
                    env.storage().persistent().set(
                        &DataKey::TopPlayerSlot(entry.address.clone()),
                        &(current - 1),
                    );
                    env.storage()
                        .persistent()
                        .set(&DataKey::TopPlayerSlot(prev.address.clone()), &current);
                    current -= 1;
                } else {
                    break;
                }
            }

            // Update min points/slot if this was the last slot.
            if count > 0 {
                let min_slot = count - 1;
                let min_entry: PlayerEntry = env
                    .storage()
                    .persistent()
                    .get(&DataKey::TopPlayerAt(min_slot))
                    .unwrap();
                env.storage()
                    .instance()
                    .set(&DataKey::MinPoints, &min_entry.points);
                env.storage().instance().set(&DataKey::MinSlot, &min_slot);
            }
            return;
        }

        // New user: insert if list not full or if they beat the current minimum.
        if count < MAX_TOP_PLAYERS {
            let slot = count;
            let entry = PlayerEntry {
                address: user.clone(),
                points: new_points,
            };
            env.storage()
                .persistent()
                .set(&DataKey::TopPlayerAt(slot), &entry);
            env.storage()
                .persistent()
                .set(&DataKey::TopPlayerSlot(user.clone()), &slot);
            env.storage()
                .persistent()
                .extend_ttl(&DataKey::TopPlayerAt(slot), TTL_BUMP, TTL_HIGH);
            env.storage()
                .instance()
                .set(&DataKey::TopPlayerCount, &(count + 1));

            // Bubble up to maintain order.
            let mut current = slot;
            while current > 0 {
                let prev: PlayerEntry = env
                    .storage()
                    .persistent()
                    .get(&DataKey::TopPlayerAt(current - 1))
                    .unwrap();
                if prev.points < entry.points {
                    env.storage()
                        .persistent()
                        .set(&DataKey::TopPlayerAt(current - 1), &entry);
                    env.storage()
                        .persistent()
                        .set(&DataKey::TopPlayerAt(current), &prev);
                    env.storage().persistent().set(
                        &DataKey::TopPlayerSlot(entry.address.clone()),
                        &(current - 1),
                    );
                    env.storage()
                        .persistent()
                        .set(&DataKey::TopPlayerSlot(prev.address.clone()), &current);
                    current -= 1;
                } else {
                    break;
                }
            }

            // Update min if we just filled the last slot.
            if count + 1 == MAX_TOP_PLAYERS {
                let min_slot = MAX_TOP_PLAYERS - 1;
                let min_entry: PlayerEntry = env
                    .storage()
                    .persistent()
                    .get(&DataKey::TopPlayerAt(min_slot))
                    .unwrap();
                env.storage()
                    .instance()
                    .set(&DataKey::MinPoints, &min_entry.points);
                env.storage().instance().set(&DataKey::MinSlot, &min_slot);
            }
        } else {
            // List full: replace the minimum if the new points beat it.
            let min_points: u64 = env
                .storage()
                .instance()
                .get(&DataKey::MinPoints)
                .unwrap_or(0);
            if new_points > min_points {
                let min_slot: u32 = env.storage().instance().get(&DataKey::MinSlot).unwrap_or(0);
                let old_entry: PlayerEntry = env
                    .storage()
                    .persistent()
                    .get(&DataKey::TopPlayerAt(min_slot))
                    .unwrap();

                // Remove old slot mapping.
                env.storage()
                    .persistent()
                    .remove(&DataKey::TopPlayerSlot(old_entry.address.clone()));

                let new_entry = PlayerEntry {
                    address: user.clone(),
                    points: new_points,
                };
                env.storage()
                    .persistent()
                    .set(&DataKey::TopPlayerAt(min_slot), &new_entry);
                env.storage()
                    .persistent()
                    .set(&DataKey::TopPlayerSlot(user.clone()), &min_slot);
                env.storage().persistent().extend_ttl(
                    &DataKey::TopPlayerAt(min_slot),
                    TTL_BUMP,
                    TTL_HIGH,
                );

                // Bubble up from min_slot.
                let mut current = min_slot;
                while current > 0 {
                    let prev: PlayerEntry = env
                        .storage()
                        .persistent()
                        .get(&DataKey::TopPlayerAt(current - 1))
                        .unwrap();
                    if prev.points < new_entry.points {
                        env.storage()
                            .persistent()
                            .set(&DataKey::TopPlayerAt(current - 1), &new_entry);
                        env.storage()
                            .persistent()
                            .set(&DataKey::TopPlayerAt(current), &prev);
                        env.storage().persistent().set(
                            &DataKey::TopPlayerSlot(new_entry.address.clone()),
                            &(current - 1),
                        );
                        env.storage()
                            .persistent()
                            .set(&DataKey::TopPlayerSlot(prev.address.clone()), &current);
                        current -= 1;
                    } else {
                        break;
                    }
                }

                // Recompute min (now at the last slot after bubbling).
                let new_min_slot = MAX_TOP_PLAYERS - 1;
                let new_min_entry: PlayerEntry = env
                    .storage()
                    .persistent()
                    .get(&DataKey::TopPlayerAt(new_min_slot))
                    .unwrap();
                env.storage()
                    .instance()
                    .set(&DataKey::MinPoints, &new_min_entry.points);
                env.storage()
                    .instance()
                    .set(&DataKey::MinSlot, &new_min_slot);
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ttl_tests;
