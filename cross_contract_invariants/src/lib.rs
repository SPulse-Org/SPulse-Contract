//! Cross-Contract Invariant Tests
//!
//! These tests verify that the combination of constants across all contracts
//! produces safe behavior. Each test exercises interactions between multiple
//! contracts (prediction_market, leaderboard, referral_registry, pulse_token).

#![no_std]
#![cfg(test)]

use soroban_sdk::{
    testutils::{Address as _, Ledger, LedgerInfo},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env, String,
};

use leaderboard::LeaderboardContract;
use prediction_market::{Category, PredictionMarketContract};
use pulse_token::PULSETokenContract;
use referral_registry::ReferralRegistryContract;

#[allow(dead_code)]
struct CrossContractSetup {
    env: Env,
    market: prediction_market::PredictionMarketContractClient<'static>,
    leaderboard: leaderboard::LeaderboardContractClient<'static>,
    referral: referral_registry::ReferralRegistryContractClient<'static>,
    token: pulse_token::PULSETokenContractClient<'static>,
    xlm: TokenClient<'static>,
    xlm_admin: StellarAssetClient<'static>,
    admin: Address,
    market_id: Address,
    referral_id: Address,
    leaderboard_id: Address,
}

fn cross_setup() -> CrossContractSetup {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    env.ledger().set(LedgerInfo {
        timestamp: 1_000_000,
        protocol_version: 26,
        sequence_number: 100,
        network_id: Default::default(),
        base_reserve: 10,
        min_temp_entry_ttl: 100,
        min_persistent_entry_ttl: 100,
        max_entry_ttl: 10_000_000,
    });

    let admin = Address::generate(&env);

    let xlm_sac_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let xlm_admin = StellarAssetClient::new(&env, &xlm_sac_id);
    let xlm = TokenClient::new(&env, &xlm_sac_id);

    let token_id = env.register(PULSETokenContract, ());
    let token = pulse_token::PULSETokenContractClient::new(&env, &token_id);
    token.initialize(
        &admin,
        &String::from_str(&env, "PULSE"),
        &String::from_str(&env, "PLSE"),
        &7u32,
    );

    let leaderboard_id = env.register(LeaderboardContract, ());
    let leaderboard = leaderboard::LeaderboardContractClient::new(&env, &leaderboard_id);

    let referral_id = env.register(ReferralRegistryContract, ());
    let referral = referral_registry::ReferralRegistryContractClient::new(&env, &referral_id);

    let market_id = env.register(PredictionMarketContract, ());
    let market = prediction_market::PredictionMarketContractClient::new(&env, &market_id);

    market.initialize(
        &admin,
        &token_id,
        &referral_id,
        &leaderboard_id,
        &xlm_sac_id,
    );
    leaderboard.initialize(&admin, &market_id, &referral_id);
    referral.initialize(&admin, &market_id, &token_id, &leaderboard_id, &xlm_sac_id);

    leaderboard.set_token_contract(&admin, &token_id, &pulse_token::INTERFACE_VERSION);
    token.set_minter(&leaderboard_id);
    token.set_minter(&market_id);
    token.set_minter(&referral_id);

    CrossContractSetup {
        env,
        market,
        leaderboard,
        referral,
        token,
        xlm,
        xlm_admin,
        admin,
        market_id,
        referral_id,
        leaderboard_id,
    }
}

fn advance_time(env: &Env, secs: u64) {
    let current = env.ledger().timestamp();
    env.ledger().set(LedgerInfo {
        timestamp: current + secs,
        protocol_version: 26,
        sequence_number: env.ledger().sequence() + 1,
        network_id: Default::default(),
        base_reserve: 10,
        min_temp_entry_ttl: 100,
        min_persistent_entry_ttl: 100,
        max_entry_ttl: 10_000_000,
    });
}

fn create_market(
    market: &prediction_market::PredictionMarketContractClient<'static>,
    admin: &Address,
) -> u64 {
    market.create_market(
        admin,
        &String::from_str(&market.env, "Test Market"),
        &String::from_str(&market.env, "https://test.png"),
        &Category::Other,
        &3600_u64,
    )
}

/// Invariant 1: Accumulator consistency across cancel + withdraw
/// Tests that AccumulatedFees == sum(MarketFees) + LegacyFees after
/// cross-contract operations (cancel in prediction_market, withdraw)
#[test]
fn test_inv_cross_accumulator_consistency() {
    let t = cross_setup();
    let market_id = create_market(&t.market, &t.admin);

    let alice = Address::generate(&t.env);
    let bob = Address::generate(&t.env);
    t.xlm_admin.mint(&alice, &500_0000000);
    t.xlm_admin.mint(&bob, &500_0000000);

    t.market
        .place_bet(&alice, &market_id, &true, &100_0000000_i128);
    t.market
        .place_bet(&bob, &market_id, &false, &100_0000000_i128);

    let acc_before = t.market.get_accumulated_fees();
    let market_fees = t.market.get_market_fees(&market_id);
    let legacy = t.market.get_legacy_fees();

    assert_eq!(acc_before, market_fees + legacy);

    t.market.cancel_market(&t.admin, &market_id);

    assert_eq!(t.market.get_accumulated_fees(), 0);
    assert_eq!(t.market.get_market_fees(&market_id), 0);
}

/// Invariant 2: Referral fee payment across contracts
/// Tests that referral fees are correctly paid to referrer through
/// the referral_registry contract when a bet is placed
#[test]
fn test_inv_cross_referral_fee_payment() {
    let t = cross_setup();
    let market_id = create_market(&t.market, &t.admin);

    let referrer = Address::generate(&t.env);
    let user = Address::generate(&t.env);
    t.xlm_admin.mint(&user, &500_0000000);

    t.referral.register_referral(
        &referrer,
        &String::from_str(&t.env, "Referrer"),
        &Option::<Address>::None,
    );
    t.referral.register_referral(
        &user,
        &String::from_str(&t.env, "User"),
        &Some(referrer.clone()),
    );

    let referrer_balance_before = t.xlm.balance(&referrer);

    t.market
        .place_bet(&user, &market_id, &true, &100_0000000_i128);

    assert_eq!(t.xlm.balance(&referrer), referrer_balance_before + 5000000);
}

/// Invariant 3: Late referrer registration (cache invalidation fix)
/// Tests that a user who registers a referrer after their first bet
/// correctly receives referral fees on subsequent bets
#[test]
fn test_inv_cross_late_referrer_registration() {
    let t = cross_setup();
    let market_id = create_market(&t.market, &t.admin);

    let referrer = Address::generate(&t.env);
    let user = Address::generate(&t.env);
    t.xlm_admin.mint(&user, &1000_0000000);

    t.referral.register_referral(
        &user,
        &String::from_str(&t.env, "User"),
        &Option::<Address>::None,
    );

    let user_balance_before = t.xlm.balance(&user);
    t.market
        .place_bet(&user, &market_id, &true, &100_0000000_i128);
    assert_eq!(
        t.xlm.balance(&user),
        user_balance_before - 100_0000000 + 5000000
    );

    t.referral.register_referral(
        &referrer,
        &String::from_str(&t.env, "Referrer"),
        &Option::<Address>::None,
    );

    t.referral.register_referral(
        &user,
        &String::from_str(&t.env, "User"),
        &Some(referrer.clone()),
    );

    let referrer_balance_before = t.xlm.balance(&referrer);
    t.market
        .place_bet(&user, &market_id, &true, &100_0000000_i128);
    assert_eq!(t.xlm.balance(&referrer), referrer_balance_before + 5000000);
}

/// Invariant 4: PULSE minting bounded by constants
/// Tests that total PULSE minted matches WIN_TOKENS after a full market
/// lifecycle (cross-contract: prediction_market -> leaderboard -> pulse_token).
/// A loss no longer mints a LOSE_TOKENS consolation prize (issue #24).
#[test]
fn test_inv_cross_pulse_minting_bounded() {
    let t = cross_setup();
    let market_id = create_market(&t.market, &t.admin);

    let alice = Address::generate(&t.env);
    let bob = Address::generate(&t.env);
    t.xlm_admin.mint(&alice, &500_0000000);
    t.xlm_admin.mint(&bob, &500_0000000);

    t.market
        .place_bet(&alice, &market_id, &true, &100_0000000_i128);
    t.market
        .place_bet(&bob, &market_id, &false, &100_0000000_i128);

    advance_time(&t.env, 3601);
    t.market.resolve_market(&t.admin, &market_id, &true);

    t.market.claim(&alice, &market_id);
    t.market.claim(&bob, &market_id);

    assert_eq!(t.token.total_supply(), 10_0000000);
    assert_eq!(t.token.balance(&alice), 10_0000000);
    assert_eq!(t.token.balance(&bob), 0);
}

/// Invariant 5: Leaderboard points conservation
/// Tests that points are correctly credited through the leaderboard contract
/// when a market is resolved and claims are processed
#[test]
fn test_inv_cross_leaderboard_points() {
    let t = cross_setup();
    let market_id = create_market(&t.market, &t.admin);

    let alice = Address::generate(&t.env);
    let bob = Address::generate(&t.env);
    t.xlm_admin.mint(&alice, &500_0000000);
    t.xlm_admin.mint(&bob, &500_0000000);

    t.market
        .place_bet(&alice, &market_id, &true, &100_0000000_i128);
    t.market
        .place_bet(&bob, &market_id, &false, &100_0000000_i128);

    advance_time(&t.env, 3601);
    t.market.resolve_market(&t.admin, &market_id, &true);

    t.market.claim(&alice, &market_id);
    t.market.claim(&bob, &market_id);

    // Bob loses: penalized LOSE_POINTS, saturating at 0 (issue #24).
    assert_eq!(t.leaderboard.get_points(&alice), 30);
    assert_eq!(t.leaderboard.get_points(&bob), 0);
}

/// Invariant 6: Withdraw cap prevents draining
/// Tests that a single withdraw_fees call cannot drain the entire accumulator
/// even after multiple markets have accumulated fees
#[test]
fn test_inv_cross_withdraw_cap() {
    let t = cross_setup();
    let market_id = create_market(&t.market, &t.admin);

    let alice = Address::generate(&t.env);
    t.xlm_admin.mint(&alice, &500_0000000);

    t.market
        .place_bet(&alice, &market_id, &true, &100_0000000_i128);
    t.market
        .place_bet(&alice, &market_id, &true, &100_0000000_i128);
    t.market
        .place_bet(&alice, &market_id, &true, &100_0000000_i128);

    let total_fees = t.market.get_accumulated_fees();

    // Settle market so fees are earned and withdrawable
    advance_time(&t.env, 3601);
    t.market.resolve_market(&t.admin, &market_id, &true);

    let treasury = Address::generate(&t.env);
    t.market.add_fee_recipient(&t.admin, &treasury);

    let withdrawn = t.market.withdraw_fees(&t.admin, &treasury);
    assert!(withdrawn < total_fees);
    assert_eq!(t.market.get_accumulated_fees(), total_fees - withdrawn);
}

/// Invariant 7: Multi-market fee isolation
/// Tests that canceling one market does not affect fees in other markets
#[test]
fn test_inv_cross_multi_market_isolation() {
    let t = cross_setup();
    let market_id1 = create_market(&t.market, &t.admin);
    let market_id2 = create_market(&t.market, &t.admin);

    let alice = Address::generate(&t.env);
    t.xlm_admin.mint(&alice, &500_0000000);

    t.market
        .place_bet(&alice, &market_id1, &true, &100_0000000_i128);
    t.market
        .place_bet(&alice, &market_id2, &true, &100_0000000_i128);

    let market2_fees_before = t.market.get_market_fees(&market_id2);

    t.market.cancel_market(&t.admin, &market_id1);

    assert_eq!(t.market.get_market_fees(&market_id2), market2_fees_before);
    assert_eq!(t.market.get_market_fees(&market_id1), 0);
}

/// Invariant 8: Dispute window fits within TTL
/// Tests that market entries survive the dispute window for claims
#[test]
fn test_inv_cross_dispute_window_ttl() {
    let t = cross_setup();
    let market_id = create_market(&t.market, &t.admin);

    let alice = Address::generate(&t.env);
    t.xlm_admin.mint(&alice, &500_0000000);

    t.market
        .place_bet(&alice, &market_id, &true, &100_0000000_i128);

    advance_time(&t.env, 3601);
    t.market.resolve_market(&t.admin, &market_id, &false);

    assert!(t.market.try_claim(&alice, &market_id).is_err());

    advance_time(&t.env, 604_801);
    t.market.claim(&alice, &market_id);
}

/// Invariant 9: Fee conservation across cancel + withdraw
/// Tests that total fees = withdrawn + reclaimed + remaining
#[test]
fn test_inv_cross_fee_conservation() {
    let t = cross_setup();
    let market_id1 = create_market(&t.market, &t.admin);
    let market_id2 = create_market(&t.market, &t.admin);

    let alice = Address::generate(&t.env);
    t.xlm_admin.mint(&alice, &500_0000000);

    t.market
        .place_bet(&alice, &market_id1, &true, &100_0000000_i128);
    t.market
        .place_bet(&alice, &market_id2, &true, &100_0000000_i128);

    let total_fees = t.market.get_accumulated_fees();
    let market1_fees = t.market.get_market_fees(&market_id1);

    t.market.cancel_market(&t.admin, &market_id1);

    // Settle market 2 so fees are earned and withdrawable
    advance_time(&t.env, 3601);
    t.market.resolve_market(&t.admin, &market_id2, &true);

    let treasury = Address::generate(&t.env);
    t.market.add_fee_recipient(&t.admin, &treasury);

    let mut withdrawn = 0;
    while t.market.get_accumulated_fees() > 0 {
        withdrawn += t.market.withdraw_fees(&t.admin, &treasury);
    }

    assert_eq!(total_fees, withdrawn + market1_fees);
    assert_eq!(t.market.get_accumulated_fees(), 0);
}

/// Invariant 10: Referral depth does not affect accumulator
/// Tests that deep referral chains do not wipe the accumulator on cancel
#[test]
fn test_inv_cross_referral_depth_isolation() {
    let t = cross_setup();
    let market_id = create_market(&t.market, &t.admin);

    let referrer1 = Address::generate(&t.env);
    let referrer2 = Address::generate(&t.env);
    let referrer3 = Address::generate(&t.env);
    let user = Address::generate(&t.env);
    t.xlm_admin.mint(&user, &500_0000000);

    t.referral
        .register_referral(&referrer1, &String::from_str(&t.env, "R1"), &None);
    t.referral.register_referral(
        &referrer2,
        &String::from_str(&t.env, "R2"),
        &Some(referrer1),
    );
    t.referral.register_referral(
        &referrer3,
        &String::from_str(&t.env, "R3"),
        &Some(referrer2),
    );
    t.referral
        .register_referral(&user, &String::from_str(&t.env, "User"), &Some(referrer3));

    t.market
        .place_bet(&user, &market_id, &true, &100_0000000_i128);

    let platform_fee = 1_5000000_i128;
    assert_eq!(t.market.get_accumulated_fees(), platform_fee);

    t.market.cancel_market(&t.admin, &market_id);
    assert_eq!(t.market.get_accumulated_fees(), 0);
}

/// Invariant 11: Emergency circuit-breaker halting all four contracts
/// Tests that pausing contracts halts state mutation across the entire system
/// while preserving recovery (cancel_refund) and read-only paths.
#[test]
fn test_inv_cross_emergency_pause_circuit_breaker() {
    let t = cross_setup();
    let market_id = create_market(&t.market, &t.admin);

    let alice = Address::generate(&t.env);
    let bob = Address::generate(&t.env);
    t.xlm_admin.mint(&alice, &500_0000000);
    t.xlm_admin.mint(&bob, &500_0000000);

    // Initial setup before pause
    t.market
        .place_bet(&alice, &market_id, &true, &100_0000000_i128);

    // Trigger emergency circuit-breaker on all contracts
    t.market.set_paused(&t.admin, &true);
    t.token.set_paused(&t.admin, &true);
    t.referral.set_paused(&t.admin, &true);
    t.leaderboard.set_paused(&t.admin, &true);

    assert!(t.market.paused());
    assert!(t.token.paused());
    assert!(t.referral.paused());
    assert!(t.leaderboard.paused());

    // 1. Prediction market mutations blocked
    assert!(t
        .market
        .try_place_bet(&bob, &market_id, &false, &50_0000000_i128)
        .is_err());
    assert!(t
        .market
        .try_reduce_position(&alice, &market_id, &10_0000000_i128)
        .is_err());
    assert!(t
        .market
        .try_create_market(
            &t.admin,
            &String::from_str(&t.env, "New Market"),
            &String::from_str(&t.env, "https://x.png"),
            &Category::Crypto,
            &3600_u64
        )
        .is_err());

    // 2. Token mutations blocked
    assert!(t
        .token
        .try_transfer(&alice, &bob, &10_0000000_i128)
        .is_err());
    assert!(t
        .token
        .try_mint(&t.leaderboard_id, &bob, &10_0000000_i128)
        .is_err());

    // 3. Referral registrations and credits blocked
    assert!(t
        .referral
        .try_register_referral(&bob, &String::from_str(&t.env, "Bob"), &None)
        .is_err());
    assert!(t
        .referral
        .try_credit(&t.market_id, &alice, &10_0000000_i128)
        .is_err());

    // 4. Leaderboard mutations blocked
    assert!(t
        .leaderboard
        .try_reward(&t.market_id, &alice, &10_u64, &0_i128, &true)
        .is_err());
    assert!(t
        .leaderboard
        .try_add_pts(&t.market_id, &alice, &10_u64, &true)
        .is_err());

    // 5. Read-only views still function
    assert_eq!(t.token.balance(&alice), 0);
    assert_eq!(t.market.get_market_count(), 1);
    assert_eq!(t.leaderboard.get_points(&alice), 0);

    // Unpause system restores functionality
    t.market.set_paused(&t.admin, &false);
    t.token.set_paused(&t.admin, &false);
    t.referral.set_paused(&t.admin, &false);
    t.leaderboard.set_paused(&t.admin, &false);

    assert!(!t.market.paused());
    assert!(!t.token.paused());
    assert!(!t.referral.paused());
    assert!(!t.leaderboard.paused());

    // Operations succeed after unpause
    t.market
        .place_bet(&bob, &market_id, &false, &50_0000000_i128);
}
