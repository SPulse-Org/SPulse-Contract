#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, vec, Address, BytesN, Env, IntoVal,
    Symbol, Val, Vec,
};

const MAX_TOP_PLAYERS: u32 = 50;
const MAX_PAGE_SIZE: u32 = 20;
const TTL_BUMP: u32 = 3_153_600;
const TTL_HIGH: u32 = 6_307_200;

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum LeaderboardError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    UnauthorizedCaller = 3,
    InvalidPoints = 4,
    NotAdmin = 5,
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
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackedStats {
    pub points: u64,
    pub total_bets: u32,
    pub won_bets: u32,
    pub lost_bets: u32,
}

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
        env.storage().instance().set(&DataKey::MarketContract, &market_contract);
        env.storage().instance().set(&DataKey::ReferralContract, &referral_contract);
        env.storage().instance().set(&DataKey::TopPlayerCount, &0_u32);
        env.storage().instance().set(&DataKey::MinPoints, &0_u64);
        env.storage().instance().set(&DataKey::MinSlot, &0_u32);
        Ok(())
    }

    pub fn set_token_contract(env: Env, admin: Address, token: Address) -> Result<(), LeaderboardError> {
        let stored: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(LeaderboardError::NotInitialized)?;
        if admin != stored {
            return Err(LeaderboardError::NotAdmin);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::TokenContract, &token);
        Ok(())
    }

    pub fn add_pts(env: Env, caller: Address, user: Address, pts: u64, is_win: bool) -> Result<(), LeaderboardError> {
        let market: Address = env
            .storage()
            .instance()
            .get(&DataKey::MarketContract)
            .ok_or(LeaderboardError::NotInitialized)?;
        if caller != market {
            return Err(LeaderboardError::UnauthorizedCaller);
        }
        caller.require_auth();

        let mut stats: PackedStats = env
            .storage()
            .persistent()
            .get(&DataKey::Stats(user.clone()))
            .unwrap_or(PackedStats {
                points: 0,
                total_bets: 0,
                won_bets: 0,
                lost_bets: 0,
            });

        stats.points += pts;
        stats.total_bets += 1;
        if is_win {
            stats.won_bets += 1;
        } else {
            stats.lost_bets += 1;
        }

        env.storage().persistent().set(&DataKey::Stats(user.clone()), &stats);
        env.storage().persistent().extend_ttl(&DataKey::Stats(user.clone()), TTL_BUMP, TTL_HIGH);

        // Update top list if this user qualifies
        Self::update_top_list(&env, user, stats.points);

        Ok(())
    }

    pub fn add_bonus_pts(env: Env, caller: Address, user: Address, pts: u64) -> Result<(), LeaderboardError> {
        let referral: Address = env
            .storage()
            .instance()
            .get(&DataKey::ReferralContract)
            .ok_or(LeaderboardError::NotInitialized)?;
        if caller != referral {
            return Err(LeaderboardError::UnauthorizedCaller);
        }
        caller.require_auth();

        let mut stats: PackedStats = env
            .storage()
            .persistent()
            .get(&DataKey::Stats(user.clone()))
            .unwrap_or(PackedStats {
                points: 0,
                total_bets: 0,
                won_bets: 0,
                lost_bets: 0,
            });

        stats.points += pts;
        stats.total_bets += 1; // bonus counts as activity

        env.storage().persistent().set(&DataKey::Stats(user.clone()), &stats);
        env.storage().persistent().extend_ttl(&DataKey::Stats(user.clone()), TTL_BUMP, TTL_HIGH);

        Self::update_top_list(&env, user, stats.points);

        Ok(())
    }

    pub fn get_points(env: Env, user: Address) -> u64 {
        let stats: PackedStats = env
            .storage()
            .persistent()
            .get(&DataKey::Stats(user))
            .unwrap_or(PackedStats {
                points: 0,
                total_bets: 0,
                won_bets: 0,
                lost_bets: 0,
            });
        stats.points
    }

    pub fn get_stats(env: Env, user: Address) -> PlayerStats {
        let stats: PackedStats = env
            .storage()
            .persistent()
            .get(&DataKey::Stats(user))
            .unwrap_or(PackedStats {
                points: 0,
                total_bets: 0,
                won_bets: 0,
                lost_bets: 0,
            });
        PlayerStats {
            points: stats.points,
            total_bets: stats.total_bets,
            won_bets: stats.won_bets,
            lost_bets: stats.lost_bets,
        }
    }

    // ── Top list maintenance ────────────────────────────────────────────────
    // Maintains a persistent sorted list (descending by points) at write time.
    // This makes get_top_players O(page_size) instead of O(n log n) per page.
    fn update_top_list(env: &Env, user: Address, points: u64) {
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TopPlayerCount)
            .unwrap_or(0);

        // If user already in top list, update their entry in place (O(1) lookup)
        if let Some(slot) = env.storage().persistent().get::<_, u32>(&DataKey::TopPlayerSlot(user.clone())) {
            let existing: PlayerEntry = env
                .storage()
                .persistent()
                .get(&DataKey::TopPlayerAt(slot))
                .unwrap();
            if existing.points == points {
                return; // no change
            }
            // Remove old entry (will re-insert below)
            env.storage().persistent().remove(&DataKey::TopPlayerAt(slot));
            env.storage().persistent().remove(&DataKey::TopPlayerSlot(user.clone()));
            // Shift all entries after slot down by one
            for i in slot..count - 1 {
                let next: PlayerEntry = env
                    .storage()
                    .persistent()
                    .get(&DataKey::TopPlayerAt(i + 1))
                    .unwrap();
                env.storage().persistent().set(&DataKey::TopPlayerAt(i), &next);
                env.storage().persistent().set(&DataKey::TopPlayerSlot(next.address.clone()), &i);
            }
            env.storage().instance().set(&DataKey::TopPlayerCount, &(count - 1));
            // Re-insert with updated points
            Self::insert_sorted(env, user, points);
            return;
        }

        // New user: insert if list not full or points exceed minimum
        if count < MAX_TOP_PLAYERS {
            Self::insert_sorted(env, user, points);
        } else {
            let min_points: u64 = env.storage().instance().get(&DataKey::MinPoints).unwrap_or(0);
            if points > min_points {
                // Remove weakest entry
                let min_slot: u32 = env.storage().instance().get(&DataKey::MinSlot).unwrap_or(0);
                let weakest: PlayerEntry = env
                    .storage()
                    .persistent()
                    .get(&DataKey::TopPlayerAt(min_slot))
                    .unwrap();
                env.storage().persistent().remove(&DataKey::TopPlayerAt(min_slot));
                env.storage().persistent().remove(&DataKey::TopPlayerSlot(weakest.address.clone()));
                // Shift entries after min_slot down
                for i in min_slot..count - 1 {
                    let next: PlayerEntry = env
                        .storage()
                        .persistent()
                        .get(&DataKey::TopPlayerAt(i + 1))
                        .unwrap();
                    env.storage().persistent().set(&DataKey::TopPlayerAt(i), &next);
                    env.storage().persistent().set(&DataKey::TopPlayerSlot(next.address.clone()), &i);
                }
                env.storage().instance().set(&DataKey::TopPlayerCount, &(count - 1));
                Self::insert_sorted(env, user, points);
            }
        }
    }

    // Insert a new entry into the sorted list (descending by points).
    // O(n) worst-case per write, but n ≤ 50, and pagination becomes O(page_size).
    fn insert_sorted(env: &Env, user: Address, points: u64) {
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TopPlayerCount)
            .unwrap_or(0);

        // Find insertion position (first entry with points < new points)
        let mut pos = count;
        for i in 0..count {
            let entry: PlayerEntry = env
                .storage()
                .persistent()
                .get(&DataKey::TopPlayerAt(i))
                .unwrap();
            if entry.points < points {
                pos = i;
                break;
            }
        }

        // Shift entries from pos..count up by one
        for i in (pos..count).rev() {
            let entry: PlayerEntry = env
                .storage()
                .persistent()
                .get(&DataKey::TopPlayerAt(i))
                .unwrap();
            env.storage().persistent().set(&DataKey::TopPlayerAt(i + 1), &entry);
            env.storage().persistent().set(&DataKey::TopPlayerSlot(entry.address.clone()), &(i + 1));
        }

        // Insert new entry
        let new_entry = PlayerEntry { address: user.clone(), points };
        env.storage().persistent().set(&DataKey::TopPlayerAt(pos), &new_entry);
        env.storage().persistent().set(&DataKey::TopPlayerSlot(user.clone()), &pos);

        let new_count = count + 1;
        env.storage().instance().set(&DataKey::TopPlayerCount, &new_count);

        // Update min tracking (last entry)
        if new_count > 0 {
            let last: PlayerEntry = env
                .storage()
                .persistent()
                .get(&DataKey::TopPlayerAt(new_count - 1))
                .unwrap();
            env.storage().instance().set(&DataKey::MinPoints, &last.points);
            env.storage().instance().set(&DataKey::MinSlot, &(new_count - 1));
        }
    }

    // ── Pagination ──────────────────────────────────────────────────────────
    // Now O(page_size) — reads only the requested slice from the persistent sorted list.
    pub fn get_top_players(env: Env, offset: u32, page_size: u32) -> Vec<PlayerEntry> {
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TopPlayerCount)
            .unwrap_or(0);

        if offset >= count {
            return Vec::new(&env);
        }

        let size = page_size.min(MAX_PAGE_SIZE).min(count - offset);
        let mut result = Vec::new(&env);
        for i in offset..offset + size {
            let entry: PlayerEntry = env
                .storage()
                .persistent()
                .get(&DataKey::TopPlayerAt(i))
                .unwrap();
            result.push_back(entry);
        }
        result
    }

    pub fn get_top_player_count(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::TopPlayerCount)
            .unwrap_or(0)
    }
}
