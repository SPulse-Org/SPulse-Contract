#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, vec, Address, BytesN,
    Env, Error, IntoVal, String, Symbol, Val, Vec,
};

// ── Constants ─────────────────────────────────────────────────────────────────

const MIN_BET: i128 = 10_000_000; // minimum net stake: 1 XLM in stroops

const MAX_BETS_PER_USER: u32 = 20;
const MAX_MARKETS_PER_HOUR: u32 = 10;
const MIN_MARKET_DURATION_SECS: u64 = 60; // issue #10: no instantly-expired markets
const MAX_BETTORS_PER_PAGE: u32 = 100;

// Fee constants — multiply before divide to avoid precision loss
const TOTAL_FEE_BPS: i128 = 200;
const PLATFORM_FEE_BPS: i128 = 150;
const BPS_DENOM: i128 = 10_000;
const NET_NUMERATOR: i128 = 9_800;

const WIN_POINTS: u64 = 30;
const LOSE_POINTS: u64 = 10;
const WIN_TOKENS: i128 = 10_0000000;
const LOSE_TOKENS: i128 = 2_0000000;

// ── Interface versioning (upgrade coordination) ──────────────────────────────
// INTERFACE_VERSION identifies the ABI this contract exposes to the other
// contracts it interoperates with. It MUST be bumped in the source AND
// committed to storage via set_interface_version() whenever any function
// another contract calls changes incompatibly, so downstream callers can
// fail closed instead of executing against a mismatched ABI.
pub const INTERFACE_VERSION: u32 = 1;
// Minimum interface versions required from each dependency before this
// contract will invoke a cross-contract function (fail closed otherwise).
const REFERRAL_CREDIT_INTERFACE_VERSION: u32 = 1;
const LEADERBOARD_REWARD_INTERFACE_VERSION: u32 = 1;

// Withdrawal safety (issue #12): a single payout is capped and the non-admin
// path is timelocked, so a compromised fee recipient cannot drain the whole
// accumulator to an arbitrary address in one call.
const WITHDRAW_DELAY_SECS: u64 = 86_400; // 24h timelock between request and payout
const MAX_WITHDRAWAL_BPS: i128 = 2_000; // per-request cap: 20% of accumulated fees

// TTL: ~1yr threshold, ~2yr extend (mainnet: ~1 ledger/5s)
const TTL_BUMP: u32 = 3_153_600;
const TTL_HIGH: u32 = 6_307_200;

// ── Errors ────────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum MarketError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    NotAdmin = 3,
    MarketNotFound = 4,
    MarketExpired = 5,
    MarketNotExpired = 6,
    MarketResolved = 7,
    MarketCancelled = 8,
    MarketNotResolved = 9,
    BetTooSmall = 10,
    OppositeSideBet = 11,
    AlreadyClaimed = 12,
    NoBetFound = 13,
    InvalidAmount = 14,
    NoFeesToWithdraw = 15,
    NotResolver = 16,
    TooManyBets = 17,
    NotAuthorized = 18,
    MarketNotCancelled = 19,
    RateLimitExceeded = 20,
    InvalidFeeRecipient = 21,
    WithdrawalTooLarge = 22,
    WithdrawalRequestExists = 23,
    NoWithdrawalRequest = 24,
    WithdrawalTooSoon = 25,
    // ── Interface versioning (upgrade coordination) ────────────────────────
    InterfaceVersionMissing = 26,
    IncompatibleInterface = 27,
    // ── Revocable trust model (issue #40) ─────────────────────────────────
    TrustRevoked = 28,
    InvalidRole = 29,
    // ── Emergency pause ───────────────────────────────────────────────────
    ContractPaused = 30,
    // ── Market duration (issue #10) ───────────────────────────────────────
    InvalidDuration = 31,
}

// ── Storage Keys ──────────────────────────────────────────────────────────────
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    Admin,
    Cfg,
    MarketCount,
    AccumulatedFees,
    Market(u64),
    Bet(u64, Address),
    BettorCount(u64),
    BettorAt(u64, u32),
    Resolver(Address),
    FeeRecipient(Address),
    HasReferrer(Address),
    RateWindow,
    Payout(u64, Address),
    PendingWithdrawal(Address),
    // ── Interface versioning (upgrade coordination) ───────────────────────
    InterfaceVersion,
    // ── Revocable trust model (issue #40) ─────────────────────────────────
    TrustRevoked(Symbol),
    // ── Emergency pause ───────────────────────────────────────────────────
    Paused,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub token: Address,
    pub referral: Address,
    pub leaderboard: Address,
    pub xlm_sac: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BetEntry {
    pub net: i128,
    pub gross: i128,
    pub is_yes: bool,
    pub claimed: bool,
    pub count: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WithdrawalRequest {
    pub recipient: Address,
    pub amount: i128,
    pub requested_at: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Category {
    Crypto,
    Sports,
    Politics,
    Entertainment,
    Science,
    Other,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Market {
    pub id: u64,
    pub question: String,
    pub image_url: String,
    pub category: Category,
    pub end_time: u64,
    pub total_yes: i128,
    pub total_no: i128,
    pub resolved: bool,
    pub outcome: bool,
    pub cancelled: bool,
    pub creator: Address,
    pub bet_count: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bet {
    pub amount: i128,
    pub is_yes: bool,
    pub claimed: bool,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct PredictionMarketContract;

#[contractimpl]
impl PredictionMarketContract {
    pub fn initialize(
        env: Env,
        admin: Address,
        token_contract: Address,
        referral_contract: Address,
        leaderboard_contract: Address,
        xlm_sac: Address,
    ) -> Result<(), MarketError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(MarketError::AlreadyInitialized);
        }
        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(
            &DataKey::Cfg,
            &Config {
                token: token_contract,
                referral: referral_contract,
                leaderboard: leaderboard_contract,
                xlm_sac,
            },
        );
        env.storage().instance().set(&DataKey::MarketCount, &0_u64);
        env.storage()
            .instance()
            .set(&DataKey::AccumulatedFees, &0_i128);
        env.storage()
            .instance()
            .set(&DataKey::InterfaceVersion, &INTERFACE_VERSION);
        Ok(())
    }

    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) -> Result<(), MarketError> {
        Self::require_admin(&env, &admin)?;
        admin.require_auth();
        env.deployer().update_current_contract_wasm(new_wasm_hash);
        Ok(())
    }

    pub fn set_config(
        env: Env,
        admin: Address,
        token_contract: Address,
        referral_contract: Address,
        leaderboard_contract: Address,
        xlm_sac: Address,
    ) -> Result<(), MarketError> {
        Self::require_admin(&env, &admin)?;
        admin.require_auth();
        env.storage().instance().set(
            &DataKey::Cfg,
            &Config {
                token: token_contract,
                referral: referral_contract,
                leaderboard: leaderboard_contract,
                xlm_sac,
            },
        );
        Ok(())
    }

    pub fn get_config(env: Env) -> Config {
        env.storage().instance().get(&DataKey::Cfg).unwrap()
    }

    pub fn interface_version(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::InterfaceVersion)
            .unwrap_or(0)
    }

    pub fn set_interface_version(
        env: Env,
        admin: Address,
        version: u32,
    ) -> Result<(), MarketError> {
        Self::require_admin(&env, &admin)?;
        admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::InterfaceVersion, &version);
        Ok(())
    }

    // ── Revocable trust model (issue #40) ─────────────────────────────────

    pub fn revoke_trust(env: Env, admin: Address, role: Symbol) -> Result<(), MarketError> {
        Self::require_admin(&env, &admin)?;
        admin.require_auth();
        if !Self::is_known_role(&env, &role) {
            return Err(MarketError::InvalidRole);
        }
        let key = DataKey::TrustRevoked(role);
        env.storage().persistent().set(&key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_BUMP, TTL_HIGH);
        Ok(())
    }

    pub fn restore_trust(env: Env, admin: Address, role: Symbol) -> Result<(), MarketError> {
        Self::require_admin(&env, &admin)?;
        admin.require_auth();
        if !Self::is_known_role(&env, &role) {
            return Err(MarketError::InvalidRole);
        }
        env.storage()
            .persistent()
            .remove(&DataKey::TrustRevoked(role));
        Ok(())
    }

    pub fn is_trust_revoked(env: Env, role: Symbol) -> bool {
        env.storage()
            .persistent()
            .get::<DataKey, bool>(&DataKey::TrustRevoked(role))
            .unwrap_or(false)
    }

    // ── Emergency pause ───────────────────────────────────────────────────

    pub fn pause(env: Env, admin: Address) -> Result<(), MarketError> {
        Self::require_admin(&env, &admin)?;
        admin.require_auth();
        env.storage().instance().set(&DataKey::Paused, &true);
        Ok(())
    }

    pub fn unpause(env: Env, admin: Address) -> Result<(), MarketError> {
        Self::require_admin(&env, &admin)?;
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

    // ── Resolver Management ───────────────────────────────────────────────

    pub fn add_resolver(env: Env, admin: Address, resolver: Address) -> Result<(), MarketError> {
        Self::require_admin(&env, &admin)?;
        admin.require_auth();
        let key = DataKey::Resolver(resolver);
        env.storage().persistent().set(&key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_BUMP, TTL_HIGH);
        Ok(())
    }

    pub fn remove_resolver(env: Env, admin: Address, resolver: Address) -> Result<(), MarketError> {
        Self::require_admin(&env, &admin)?;
        admin.require_auth();
        env.storage()
            .persistent()
            .remove(&DataKey::Resolver(resolver));
        Ok(())
    }

    pub fn is_resolver(env: Env, resolver: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::Resolver(resolver))
            .unwrap_or(false)
    }

    // ── Fee Recipient Management ──────────────────────────────────────────

    pub fn add_fee_recipient(
        env: Env,
        admin: Address,
        recipient: Address,
    ) -> Result<(), MarketError> {
        Self::require_admin(&env, &admin)?;
        admin.require_auth();
        let key = DataKey::FeeRecipient(recipient);
        env.storage().persistent().set(&key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_BUMP, TTL_HIGH);
        Ok(())
    }

    pub fn remove_fee_recipient(
        env: Env,
        admin: Address,
        recipient: Address,
    ) -> Result<(), MarketError> {
        Self::require_admin(&env, &admin)?;
        admin.require_auth();
        env.storage()
            .persistent()
            .remove(&DataKey::FeeRecipient(recipient));
        Ok(())
    }

    // ── Market Management ─────────────────────────────────────────────────

    pub fn create_market(
        env: Env,
        admin: Address,
        question: String,
        image_url: String,
        category: Category,
        duration_secs: u64,
    ) -> Result<u64, MarketError> {
        Self::require_not_paused(&env)?;
        Self::require_admin(&env, &admin)?;
        admin.require_auth();
        Self::check_rate(&env)?;

        if duration_secs < MIN_MARKET_DURATION_SECS {
            return Err(MarketError::InvalidDuration);
        }

        let market_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::MarketCount)
            .unwrap_or(0)
            + 1;
        let end_time = env.ledger().timestamp() + duration_secs;

        let market = Market {
            id: market_id,
            question,
            image_url,
            category,
            end_time,
            total_yes: 0,
            total_no: 0,
            resolved: false,
            outcome: false,
            cancelled: false,
            creator: admin,
            bet_count: 0,
        };

        let mkt_key = DataKey::Market(market_id);
        env.storage().persistent().set(&mkt_key, &market);
        env.storage()
            .persistent()
            .extend_ttl(&mkt_key, TTL_BUMP, TTL_HIGH);
        env.storage()
            .instance()
            .set(&DataKey::MarketCount, &market_id);

        Ok(market_id)
    }

    // ── Betting ───────────────────────────────────────────────────────────

    pub fn place_bet(
        env: Env,
        user: Address,
        market_id: u64,
        is_yes: bool,
        amount: i128,
    ) -> Result<(), MarketError> {
        Self::require_not_paused(&env)?;
        user.require_auth();

        let net = amount * NET_NUMERATOR / BPS_DENOM;
        if net < MIN_BET {
            return Err(MarketError::BetTooSmall);
        }

        let mut market = Self::load_market(&env, market_id)?;
        if market.cancelled {
            return Err(MarketError::MarketCancelled);
        }
        if market.resolved {
            return Err(MarketError::MarketResolved);
        }
        if env.ledger().timestamp() >= market.end_time {
            return Err(MarketError::MarketExpired);
        }

        let bet_key = DataKey::Bet(market_id, user.clone());
        let existing: Option<BetEntry> = env.storage().persistent().get(&bet_key);

        if let Some(ref e) = existing {
            if e.count >= MAX_BETS_PER_USER {
                return Err(MarketError::TooManyBets);
            }
            if e.is_yes != is_yes {
                return Err(MarketError::OppositeSideBet);
            }
        }

        let is_increase = existing.is_some();

        let total_fee = amount * TOTAL_FEE_BPS / BPS_DENOM;
        let platform_fee = amount * PLATFORM_FEE_BPS / BPS_DENOM;
        let referral_fee = total_fee - platform_fee;

        let cfg: Config = env.storage().instance().get(&DataKey::Cfg).unwrap();

        let xlm = token::Client::new(&env, &cfg.xlm_sac);
        let this = env.current_contract_address();
        xlm.transfer(&user, &this, &amount);

        let mut acc_fees: i128 = env
            .storage()
            .instance()
            .get(&DataKey::AccumulatedFees)
            .unwrap_or(0);
        acc_fees += platform_fee;

        Self::require_trust_not_revoked(&env, symbol_short!("referral"))?;

        let hr_key = DataKey::HasReferrer(user.clone());
        let cached: Option<bool> = env.storage().persistent().get(&hr_key);

        let paid_referrer = if cached == Some(false) {
            false
        } else {
            Self::require_interface_version(
                &env,
                &cfg.referral,
                REFERRAL_CREDIT_INTERFACE_VERSION,
            )?;
            xlm.transfer(&this, &cfg.referral, &referral_fee);
            let result: bool = env.invoke_contract(
                &cfg.referral,
                &Symbol::new(&env, "credit"),
                vec![
                    &env,
                    this.clone().into_val(&env),
                    user.clone().into_val(&env),
                    referral_fee.into_val(&env),
                ],
            );
            if cached.is_none() {
                env.storage().persistent().set(&hr_key, &result);
                env.storage()
                    .persistent()
                    .extend_ttl(&hr_key, TTL_BUMP, TTL_HIGH);
            }
            result
        };

        if !paid_referrer {
            acc_fees += referral_fee;
        }
        env.storage()
            .instance()
            .set(&DataKey::AccumulatedFees, &acc_fees);

        let new_entry = match existing {
            Some(mut e) => {
                e.net += net;
                e.gross += amount;
                e.count += 1;
                e
            }
            None => BetEntry {
                net,
                gross: amount,
                is_yes,
                claimed: false,
                count: 1,
            },
        };
        env.storage().persistent().set(&bet_key, &new_entry);
        env.storage()
            .persistent()
            .extend_ttl(&bet_key, TTL_BUMP, TTL_HIGH);

        if !is_increase {
            let cnt_key = DataKey::BettorCount(market_id);
            let count: u32 = env.storage().persistent().get(&cnt_key).unwrap_or(0);
            let slot_key = DataKey::BettorAt(market_id, count);
            env.storage().persistent().set(&slot_key, &user);
            env.storage()
                .persistent()
                .extend_ttl(&slot_key, TTL_BUMP, TTL_HIGH);
            let new_count = count + 1;
            env.storage().persistent().set(&cnt_key, &new_count);
            env.storage()
                .persistent()
                .extend_ttl(&cnt_key, TTL_BUMP, TTL_HIGH);
            market.bet_count += 1;
        }

        if is_yes {
            market.total_yes += net;
        } else {
            market.total_no += net;
        }
        let mkt_key = DataKey::Market(market_id);
        env.storage().persistent().set(&mkt_key, &market);
        env.storage()
            .persistent()
            .extend_ttl(&mkt_key, TTL_BUMP, TTL_HIGH);
        Ok(())
    }

    // ── Resolution ────────────────────────────────────────────────────────

    pub fn resolve_market(
        env: Env,
        caller: Address,
        market_id: u64,
        outcome: bool,
    ) -> Result<(), MarketError> {
        Self::require_not_paused(&env)?;
        caller.require_auth();
        Self::require_admin_or_resolver(&env, &caller)?;

        let mut market = Self::load_market(&env, market_id)?;
        if market.resolved {
            return Err(MarketError::MarketResolved);
        }
        if market.cancelled {
            return Err(MarketError::MarketCancelled);
        }
        if env.ledger().timestamp() < market.end_time {
            return Err(MarketError::MarketNotExpired);
        }

        let total_pool: i128 = market.total_yes + market.total_no;
        let winning_side: i128 = if outcome {
            market.total_yes
        } else {
            market.total_no
        };

        let mut acc_fees: i128 = env
            .storage()
            .instance()
            .get(&DataKey::AccumulatedFees)
            .unwrap_or(0);

        if winning_side == 0 {
            if total_pool > 0 {
                acc_fees += total_pool;
            }
        } else {
            let mut payout_sum: i128 = 0;
            let bettors: u32 = env
                .storage()
                .persistent()
                .get(&DataKey::BettorCount(market_id))
                .unwrap_or(0);
            if bettors > 0 {
                env.storage().persistent().extend_ttl(
                    &DataKey::BettorCount(market_id),
                    TTL_BUMP,
                    TTL_HIGH,
                );
            }

            for i in 0..bettors {
                let slot_key = DataKey::BettorAt(market_id, i);
                let bettor: Address = if let Some(a) = env.storage().persistent().get(&slot_key) {
                    env.storage()
                        .persistent()
                        .extend_ttl(&slot_key, TTL_BUMP, TTL_HIGH);
                    a
                } else {
                    continue;
                };
                let bet_key = DataKey::Bet(market_id, bettor.clone());
                if let Some(entry) = env
                    .storage()
                    .persistent()
                    .get::<DataKey, BetEntry>(&bet_key)
                {
                    env.storage()
                        .persistent()
                        .extend_ttl(&bet_key, TTL_BUMP, TTL_HIGH);
                    if entry.is_yes == outcome {
                        let payout = (entry.net * total_pool) / winning_side;
                        let payout_key = DataKey::Payout(market_id, bettor.clone());
                        env.storage().persistent().set(&payout_key, &payout);
                        env.storage()
                            .persistent()
                            .extend_ttl(&payout_key, TTL_BUMP, TTL_HIGH);
                        payout_sum += payout;
                    }
                }
            }

            let dust: i128 = total_pool - payout_sum;
            debug_assert!(dust >= 0, "payouts must never exceed the pool");
            if dust > 0 {
                acc_fees += dust;
            }
        }

        env.storage()
            .instance()
            .set(&DataKey::AccumulatedFees, &acc_fees);

        market.resolved = true;
        market.outcome = outcome;
        let mkt_key = DataKey::Market(market_id);
        env.storage().persistent().set(&mkt_key, &market);
        env.storage()
            .persistent()
            .extend_ttl(&mkt_key, TTL_BUMP, TTL_HIGH);
        Ok(())
    }

    // ── Cancellation ──────────────────────────────────────────────────────

    pub fn cancel_market(env: Env, admin: Address, market_id: u64) -> Result<(), MarketError> {
        Self::require_not_paused(&env)?;
        Self::require_admin(&env, &admin)?;
        admin.require_auth();

        let mut market = Self::load_market(&env, market_id)?;
        if market.resolved {
            return Err(MarketError::MarketResolved);
        }
        if market.cancelled {
            return Err(MarketError::MarketCancelled);
        }

        market.cancelled = true;
        let mkt_key = DataKey::Market(market_id);
        env.storage().persistent().set(&mkt_key, &market);
        env.storage()
            .persistent()
            .extend_ttl(&mkt_key, TTL_BUMP, TTL_HIGH);

        let net_pool = market.total_yes + market.total_no;
        let fees_in_pool = net_pool * TOTAL_FEE_BPS / (BPS_DENOM - TOTAL_FEE_BPS);
        let mut acc_fees: i128 = env
            .storage()
            .instance()
            .get(&DataKey::AccumulatedFees)
            .unwrap_or(0);
        acc_fees = if fees_in_pool < acc_fees {
            acc_fees - fees_in_pool
        } else {
            0
        };
        env.storage()
            .instance()
            .set(&DataKey::AccumulatedFees, &acc_fees);

        Ok(())
    }

    pub fn cancel_refund(env: Env, user: Address, market_id: u64) -> Result<i128, MarketError> {
        user.require_auth();

        let market = Self::load_market(&env, market_id)?;
        if !market.cancelled {
            return Err(MarketError::MarketNotCancelled);
        }

        let bet_key = DataKey::Bet(market_id, user.clone());
        let mut entry: BetEntry = env
            .storage()
            .persistent()
            .get(&bet_key)
            .ok_or(MarketError::NoBetFound)?;

        if entry.gross == 0 {
            return Err(MarketError::NoBetFound);
        }

        let gross = entry.gross;
        entry.gross = 0;
        env.storage().persistent().set(&bet_key, &entry);
        env.storage()
            .persistent()
            .extend_ttl(&bet_key, TTL_BUMP, TTL_HIGH);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Market(market_id), TTL_BUMP, TTL_HIGH);

        let cfg: Config = env.storage().instance().get(&DataKey::Cfg).unwrap();
        token::Client::new(&env, &cfg.xlm_sac).transfer(
            &env.current_contract_address(),
            &user,
            &gross,
        );

        Ok(gross)
    }

    // ── Claim ─────────────────────────────────────────────────────────────

    pub fn claim(env: Env, user: Address, market_id: u64) -> Result<(), MarketError> {
        Self::require_not_paused(&env)?;
        user.require_auth();

        let market = Self::load_market(&env, market_id)?;
        if market.cancelled {
            return Err(MarketError::MarketCancelled);
        }
        if !market.resolved {
            return Err(MarketError::MarketNotResolved);
        }

        let bet_key = DataKey::Bet(market_id, user.clone());
        let mut entry: BetEntry = env
            .storage()
            .persistent()
            .get(&bet_key)
            .ok_or(MarketError::NoBetFound)?;

        if entry.claimed {
            return Err(MarketError::AlreadyClaimed);
        }

        let is_winner = entry.is_yes == market.outcome;
        let winning_side = if market.outcome {
            market.total_yes
        } else {
            market.total_no
        };

        let cfg: Config = env.storage().instance().get(&DataKey::Cfg).unwrap();
        Self::require_trust_not_revoked(&env, Symbol::new(&env, "leaderboard"))?;
        Self::require_interface_version(
            &env,
            &cfg.leaderboard,
            LEADERBOARD_REWARD_INTERFACE_VERSION,
        )?;
        let this = env.current_contract_address();

        entry.claimed = true;
        env.storage().persistent().set(&bet_key, &entry);
        env.storage()
            .persistent()
            .extend_ttl(&bet_key, TTL_BUMP, TTL_HIGH);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Market(market_id), TTL_BUMP, TTL_HIGH);
        let payout_key = DataKey::Payout(market_id, user.clone());
        if env.storage().persistent().has(&payout_key) {
            env.storage()
                .persistent()
                .extend_ttl(&payout_key, TTL_BUMP, TTL_HIGH);
        }

        let payout: i128 = env
            .storage()
            .persistent()
            .get::<DataKey, i128>(&payout_key)
            .unwrap_or_default();
        if is_winner && payout > 0 {
            token::Client::new(&env, &cfg.xlm_sac).transfer(&this, &user, &payout);
        }

        let real_win = is_winner && winning_side > 0;
        let (points, tokens): (u64, i128) = if real_win {
            (WIN_POINTS, WIN_TOKENS)
        } else {
            (LOSE_POINTS, LOSE_TOKENS)
        };

        let _: Val = env.invoke_contract(
            &cfg.leaderboard,
            &Symbol::new(&env, "reward"),
            vec![
                &env,
                this.clone().into_val(&env),
                user.clone().into_val(&env),
                points.into_val(&env),
                tokens.into_val(&env),
                real_win.into_val(&env),
            ],
        );

        Ok(())
    }

    // ── Withdraw Fees ─────────────────────────────────────────────────────

    pub fn withdraw_fees(
        env: Env,
        caller: Address,
        recipient: Address,
    ) -> Result<i128, MarketError> {
        Self::require_not_paused(&env)?;
        caller.require_auth();
        Self::require_admin(&env, &caller)?;
        Self::require_valid_fee_recipient(&env, &caller, &recipient)?;

        let fees: i128 = env
            .storage()
            .instance()
            .get(&DataKey::AccumulatedFees)
            .unwrap_or(0);
        if fees == 0 {
            return Err(MarketError::NoFeesToWithdraw);
        }

        let cfg: Config = env.storage().instance().get(&DataKey::Cfg).unwrap();
        token::Client::new(&env, &cfg.xlm_sac).transfer(
            &env.current_contract_address(),
            &recipient,
            &fees,
        );

        env.storage()
            .instance()
            .set(&DataKey::AccumulatedFees, &0_i128);
        Ok(fees)
    }

    pub fn request_withdraw_fees(
        env: Env,
        caller: Address,
        recipient: Address,
        amount: i128,
    ) -> Result<(), MarketError> {
        Self::require_not_paused(&env)?;
        caller.require_auth();
        Self::require_admin_or_fee_recipient(&env, &caller)?;
        Self::require_valid_fee_recipient(&env, &caller, &recipient)?;

        if amount <= 0 {
            return Err(MarketError::InvalidAmount);
        }
        let key = DataKey::PendingWithdrawal(caller.clone());
        if env.storage().persistent().has(&key) {
            return Err(MarketError::WithdrawalRequestExists);
        }

        let fees: i128 = env
            .storage()
            .instance()
            .get(&DataKey::AccumulatedFees)
            .unwrap_or(0);
        if amount > fees {
            return Err(MarketError::WithdrawalTooLarge);
        }
        let cap = fees * MAX_WITHDRAWAL_BPS / BPS_DENOM;
        if amount > cap {
            return Err(MarketError::WithdrawalTooLarge);
        }

        env.storage().persistent().set(
            &key,
            &WithdrawalRequest {
                recipient,
                amount,
                requested_at: env.ledger().timestamp(),
            },
        );
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_BUMP, TTL_HIGH);
        Ok(())
    }

    pub fn execute_withdraw_fees(env: Env, caller: Address) -> Result<i128, MarketError> {
        Self::require_not_paused(&env)?;
        caller.require_auth();
        let key = DataKey::PendingWithdrawal(caller.clone());
        let req: WithdrawalRequest = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(MarketError::NoWithdrawalRequest)?;

        let now = env.ledger().timestamp();
        if now < req.requested_at || now - req.requested_at < WITHDRAW_DELAY_SECS {
            return Err(MarketError::WithdrawalTooSoon);
        }

        let mut acc_fees: i128 = env
            .storage()
            .instance()
            .get(&DataKey::AccumulatedFees)
            .unwrap_or(0);
        if acc_fees < req.amount {
            return Err(MarketError::WithdrawalTooLarge);
        }
        acc_fees -= req.amount;

        env.storage()
            .instance()
            .set(&DataKey::AccumulatedFees, &acc_fees);
        env.storage().persistent().remove(&key);

        let cfg: Config = env.storage().instance().get(&DataKey::Cfg).unwrap();
        token::Client::new(&env, &cfg.xlm_sac).transfer(
            &env.current_contract_address(),
            &req.recipient,
            &req.amount,
        );

        Ok(req.amount)
    }

    pub fn cancel_withdrawal_request(
        env: Env,
        admin: Address,
        caller: Address,
    ) -> Result<(), MarketError> {
        Self::require_admin(&env, &admin)?;
        admin.require_auth();
        let key = DataKey::PendingWithdrawal(caller);
        if !env.storage().persistent().has(&key) {
            return Err(MarketError::NoWithdrawalRequest);
        }
        env.storage().persistent().remove(&key);
        Ok(())
    }

    // ── View Functions ────────────────────────────────────────────────────

    pub fn get_market(env: Env, market_id: u64) -> Result<Market, MarketError> {
        let market = Self::load_market(&env, market_id)?;
        Self::bump_ttl(&env, &DataKey::Market(market_id));
        Ok(market)
    }

    pub fn get_bet(env: Env, market_id: u64, user: Address) -> Result<Bet, MarketError> {
        let bet_key = DataKey::Bet(market_id, user.clone());
        let e: BetEntry = env
            .storage()
            .persistent()
            .get(&bet_key)
            .ok_or(MarketError::NoBetFound)?;
        Self::bump_ttl(&env, &bet_key);
        Ok(Bet {
            amount: e.net,
            is_yes: e.is_yes,
            claimed: e.claimed,
        })
    }

    pub fn get_market_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::MarketCount)
            .unwrap_or(0)
    }

    pub fn get_market_bettors(env: Env, market_id: u64) -> Result<Vec<Address>, MarketError> {
        Self::get_market_bettors_page(env, market_id, 0, MAX_BETTORS_PER_PAGE)
    }

    pub fn get_market_bettors_page(
        env: Env,
        market_id: u64,
        start: u32,
        limit: u32,
    ) -> Result<Vec<Address>, MarketError> {
        Self::load_market(&env, market_id)?;
        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::BettorCount(market_id))
            .unwrap_or(0);
        if count > 0 {
            Self::bump_ttl(&env, &DataKey::BettorCount(market_id));
        }
        let page_limit = limit.min(MAX_BETTORS_PER_PAGE);
        let end = start.saturating_add(page_limit).min(count);
        let mut result: Vec<Address> = Vec::new(&env);
        for i in start..end {
            let slot_key = DataKey::BettorAt(market_id, i);
            if let Some(addr) = env
                .storage()
                .persistent()
                .get::<DataKey, Address>(&slot_key)
            {
                Self::bump_ttl(&env, &slot_key);
                result.push_back(addr);
            }
        }
        Ok(result)
    }

    pub fn get_accumulated_fees(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::AccumulatedFees)
            .unwrap_or(0)
    }

    pub fn is_fee_recipient(env: Env, recipient: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::FeeRecipient(recipient))
            .unwrap_or(false)
    }

    pub fn get_pending_withdrawal(env: Env, caller: Address) -> Option<WithdrawalRequest> {
        env.storage()
            .persistent()
            .get(&DataKey::PendingWithdrawal(caller))
    }

    pub fn get_payout(env: Env, market_id: u64, user: Address) -> i128 {
        let payout_key = DataKey::Payout(market_id, user.clone());
        let payout = env
            .storage()
            .persistent()
            .get::<DataKey, i128>(&payout_key)
            .unwrap_or(0);
        if env.storage().persistent().has(&payout_key) {
            Self::bump_ttl(&env, &payout_key);
        }
        payout
    }

    pub fn get_user_bet_count(env: Env, market_id: u64, user: Address) -> u32 {
        env.storage()
            .persistent()
            .get::<DataKey, BetEntry>(&DataKey::Bet(market_id, user))
            .map(|e| e.count)
            .unwrap_or(0)
    }

    pub fn get_bet_gross(env: Env, market_id: u64, user: Address) -> i128 {
        env.storage()
            .persistent()
            .get::<DataKey, BetEntry>(&DataKey::Bet(market_id, user))
            .map(|e| e.gross)
            .unwrap_or(0)
    }

    // ── Internal Helpers ──────────────────────────────────────────────────

    fn is_known_role(env: &Env, role: &Symbol) -> bool {
        *role == Symbol::new(env, "referral") || *role == Symbol::new(env, "leaderboard")
    }

    fn require_trust_not_revoked(env: &Env, role: Symbol) -> Result<(), MarketError> {
        if env
            .storage()
            .persistent()
            .get::<DataKey, bool>(&DataKey::TrustRevoked(role))
            .unwrap_or(false)
        {
            return Err(MarketError::TrustRevoked);
        }
        Ok(())
    }

    fn require_not_paused(env: &Env) -> Result<(), MarketError> {
        if env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
        {
            return Err(MarketError::ContractPaused);
        }
        Ok(())
    }

    #[inline]
    fn load_market(env: &Env, market_id: u64) -> Result<Market, MarketError> {
        env.storage()
            .persistent()
            .get(&DataKey::Market(market_id))
            .ok_or(MarketError::MarketNotFound)
    }

    #[inline]
    fn bump_ttl(env: &Env, key: &DataKey) {
        env.storage()
            .persistent()
            .extend_ttl(key, TTL_BUMP, TTL_HIGH);
    }

    fn require_interface_version(
        env: &Env,
        target: &Address,
        required: u32,
    ) -> Result<(), MarketError> {
        let version: u32 = match env.try_invoke_contract::<u32, Error>(
            target,
            &Symbol::new(env, "interface_version"),
            vec![&env],
        ) {
            Ok(Ok(v)) => v,
            Ok(Err(_)) | Err(_) => return Err(MarketError::InterfaceVersionMissing),
        };
        if version < required {
            return Err(MarketError::IncompatibleInterface);
        }
        Ok(())
    }

    fn check_rate(env: &Env) -> Result<(), MarketError> {
        let now = env.ledger().timestamp();
        let packed: u64 = env
            .storage()
            .instance()
            .get(&DataKey::RateWindow)
            .unwrap_or(0);
        let window_start = packed >> 32;
        let count = (packed & 0xFFFF_FFFF) as u32;
        let window_len = 3_600u64;
        if now >= window_start && now - window_start < window_len {
            if count >= MAX_MARKETS_PER_HOUR {
                return Err(MarketError::RateLimitExceeded);
            }
            let new_packed = (window_start << 32) | ((count + 1) as u64);
            env.storage().instance().set(&DataKey::RateWindow, &new_packed);
        } else {
            let new_packed = (now << 32) | 1u64;
            env.storage().instance().set(&DataKey::RateWindow, &new_packed);
        }
        Ok(())
    }

    #[inline]
    fn require_admin(env: &Env, caller: &Address) -> Result<(), MarketError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(MarketError::NotInitialized)?;
        if *caller != admin {
            return Err(MarketError::NotAdmin);
        }
        Ok(())
    }

    fn require_admin_or_resolver(env: &Env, caller: &Address) -> Result<(), MarketError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(MarketError::NotInitialized)?;
        if *caller == admin {
            return Ok(());
        }
        if env
            .storage()
            .persistent()
            .get(&DataKey::Resolver(caller.clone()))
            .unwrap_or(false)
        {
            return Ok(());
        }
        Err(MarketError::NotResolver)
    }

    fn require_admin_or_fee_recipient(env: &Env, caller: &Address) -> Result<(), MarketError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(MarketError::NotInitialized)?;
        if *caller == admin {
            return Ok(());
        }
        if env
            .storage()
            .persistent()
            .get(&DataKey::FeeRecipient(caller.clone()))
            .unwrap_or(false)
        {
            return Ok(());
        }
        Err(MarketError::NotAuthorized)
    }

    fn require_valid_fee_recipient(
        env: &Env,
        caller: &Address,
        recipient: &Address,
    ) -> Result<(), MarketError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(MarketError::NotInitialized)?;
        if *recipient == admin || *recipient == *caller {
            return Ok(());
        }
        if env
            .storage()
            .persistent()
            .get(&DataKey::FeeRecipient(recipient.clone()))
            .unwrap_or(false)
        {
            return Ok(());
        }
        Err(MarketError::InvalidFeeRecipient)
    }
}

#[cfg(test)]
mod tests;
