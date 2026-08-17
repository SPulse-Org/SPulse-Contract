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

        // Update top list if needed
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

    pub fn get_top_players(env: Env, offset: u32, page_size: u32) -> Vec<PlayerEntry> {
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TopPlayerCount)
            .unwrap_or(0_u32);

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

    pub fn get_top_count(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::TopPlayerCount)
            .unwrap_or(0_u32)
    }

    pub fn get_min_points(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::MinPoints)
            .unwrap_or(0_u64)
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    fn update_top_list(env: &Env, user: Address, points: u64) {
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TopPlayerCount)
            .unwrap_or(0_u32);

        // If user is already in the list, update their points and re-sort in place.
        if let Some(slot) = env.storage().persistent().get(&DataKey::TopPlayerSlot(user.clone())) {
            let mut entry: PlayerEntry = env
                .storage()
                .persistent()
                .get(&DataKey::TopPlayerAt(slot))
                .unwrap();
            entry.points = points;
            env.storage().persistent().set(&DataKey::TopPlayerAt(slot), &entry);
            Self::bubble_up(env, slot);
            Self::bubble_down(env, slot);
            return;
        }

        // New user: insert if list not full or points exceed the minimum.
        let min_points: u64 = env
            .storage()
            .instance()
            .get(&DataKey::MinPoints)
            .unwrap_or(0_u64);

        if count < MAX_TOP_PLAYERS {
            let slot = count;
            let entry = PlayerEntry {
                address: user.clone(),
                points,
            };
            env.storage().persistent().set(&DataKey::TopPlayerAt(slot), &entry);
            env.storage().persistent().set(&DataKey::TopPlayerSlot(user.clone()), &slot);
            env.storage().instance().set(&DataKey::TopPlayerCount, &(count + 1));
            Self::bubble_up(env, slot);
            // Update min after possible reorder
            Self::update_min(env);
        } else if points > min_points {
            // Replace the weakest entry (at MinSlot)
            let min_slot: u32 = env
                .storage()
                .instance()
                .get(&DataKey::MinSlot)
                .unwrap_or(0_u32);
            let old: PlayerEntry = env
                .storage()
                .persistent()
                .get(&DataKey::TopPlayerAt(min_slot))
                .unwrap();
            env.storage().persistent().remove(&DataKey::TopPlayerSlot(old.address));

            let entry = PlayerEntry {
                address: user.clone(),
                points,
            };
            env.storage().persistent().set(&DataKey::TopPlayerAt(min_slot), &entry);
            env.storage().persistent().set(&DataKey::TopPlayerSlot(user.clone()), &min_slot);
            Self::bubble_up(env, min_slot);
            Self::bubble_down(env, min_slot);
            Self::update_min(env);
        }
    }

    fn bubble_up(env: &Env, mut slot: u32) {
        while slot > 0 {
            let parent = (slot - 1) / 2;
            let current: PlayerEntry = env
                .storage()
                .persistent()
                .get(&DataKey::TopPlayerAt(slot))
                .unwrap();
            let parent_entry: PlayerEntry = env
                .storage()
                .persistent()
                .get(&DataKey::TopPlayerAt(parent))
                .unwrap();
            if current.points > parent_entry.points {
                // Swap
                env.storage().persistent().set(&DataKey::TopPlayerAt(slot), &parent_entry);
                env.storage().persistent().set(&DataKey::TopPlayerAt(parent), &current);
                env.storage().persistent().set(&DataKey::TopPlayerSlot(current.address.clone()), &parent);
                env.storage().persistent().set(&DataKey::TopPlayerSlot(parent_entry.address.clone()), &slot);
                slot = parent;
            } else {
                break;
            }
        }
    }

    fn bubble_down(env: &Env, mut slot: u32) {
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TopPlayerCount)
            .unwrap_or(0_u32);
        loop {
            let left = 2 * slot + 1;
            let right = 2 * slot + 2;
            if left >= count {
                break;
            }
            let mut largest = slot;
            let current: PlayerEntry = env
                .storage()
                .persistent()
                .get(&DataKey::TopPlayerAt(slot))
                .unwrap();
            let left_entry: PlayerEntry = env
                .storage()
                .persistent()
                .get(&DataKey::TopPlayerAt(left))
                .unwrap();
            if left_entry.points > current.points {
                largest = left;
            }
            if right < count {
                let right_entry: PlayerEntry = env
                    .storage()
                    .persistent()
                    .get(&DataKey::TopPlayerAt(right))
                    .unwrap();
                let largest_entry: PlayerEntry = env
                    .storage()
                    .persistent()
                    .get(&DataKey::TopPlayerAt(largest))
                    .unwrap();
                if right_entry.points > largest_entry.points {
                    largest = right;
                }
            }
            if largest == slot {
                break;
            }
            // Swap slot with largest
            let largest_entry: PlayerEntry = env
                .storage()
                .persistent()
                .get(&DataKey::TopPlayerAt(largest))
                .unwrap();
            let current_entry: PlayerEntry = env
                .storage()
                .persistent()
                .get(&DataKey::TopPlayerAt(slot))
                .unwrap();
            env.storage().persistent().set(&DataKey::TopPlayerAt(slot), &largest_entry);
            env.storage().persistent().set(&DataKey::TopPlayerAt(largest), &current_entry);
            env.storage().persistent().set(&DataKey::TopPlayerSlot(largest_entry.address.clone()), &slot);
            env.storage().persistent().set(&DataKey::TopPlayerSlot(current_entry.address.clone()), &largest);
            slot = largest;
        }
    }

    fn update_min(env: &Env) {
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TopPlayerCount)
            .unwrap_or(0_u32);
        if count == 0 {
            env.storage().instance().set(&DataKey::MinPoints, &0_u64);
            env.storage().instance().set(&DataKey::MinSlot, &0_u32);
            return;
        }
        // The minimum is the last element of the heap array (not necessarily the heap min, but the weakest entry to replace).
        // Since we maintain a max-heap, the minimum is at the end of the array.
        let min_slot = count - 1;
        let min_entry: PlayerEntry = env
            .storage()
            .persistent()
            .get(&DataKey::TopPlayerAt(min_slot))
            .unwrap();
        env.storage().instance().set(&DataKey::MinPoints, &min_entry.points);
        env.storage().instance().set(&DataKey::MinSlot, &min_slot);
    }
}
