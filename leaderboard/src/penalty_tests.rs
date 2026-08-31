// ── Issue #24: an explicit way to reduce points ───────────────────────────────
//
// Decay (issue #69, see decay_tests.rs) fixes the *staleness* half of #24: an
// idle leader's score shrinks over time, so an active newcomer can catch up
// without ever out-earning them in absolute terms. It does not touch the
// other half of the complaint — "no penalty for losses (losers still gain
// LOSE_POINTS)" — because decay only ever erodes a score passively, on a
// schedule; nothing in that model lets a caller take points away for a
// specific event (a loss) the moment it happens.
//
// penalize() is that missing primitive. These tests prove it does the one
// thing it's supposed to (move a score down, decay-aware, saturating at
// zero) and, just as importantly, prove it *doesn't* do anything it isn't
// supposed to: it must never be the reason an unranked player enters the top
// list, and it must never touch the activity counters (won/lost/bonus) that
// a separate add_pts/reward call already recorded.

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger as _},
    Env, Symbol, TryFromVal, Val,
};

fn setup() -> (
    Env,
    LeaderboardContractClient<'static>,
    Address,
    Address,
    Address,
) {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let contract_id = env.register(LeaderboardContract, ());
    let client = LeaderboardContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let market = Address::generate(&env);
    let referral = Address::generate(&env);

    client.initialize(&admin, &market, &referral);
    (env, client, admin, market, referral)
}

/// Move the ledger forward by whole decay periods (same helper as
/// decay_tests.rs — kept local so this file has no cross-module dependency).
fn advance_periods(env: &Env, periods: u32) {
    let seq = env.ledger().sequence();
    env.ledger()
        .set_sequence_number(seq + periods * DECAY_PERIOD_LEDGERS);
}

#[test]
fn test_penalize_reduces_points() {
    let (env, client, _admin, market, _referral) = setup();
    let user = Address::generate(&env);
    client.add_pts(&market, &user, &100_u64, &true);
    client.penalize(&market, &user, &30_u64);
    assert_eq!(client.get_points(&user), 70);
}

#[test]
fn test_penalize_saturates_at_zero_instead_of_underflowing() {
    let (env, client, _admin, market, _referral) = setup();
    let user = Address::generate(&env);
    client.add_pts(&market, &user, &10_u64, &true);
    // Deducting more than the balance must floor to 0, not panic or wrap.
    client.penalize(&market, &user, &1_000_u64);
    assert_eq!(client.get_points(&user), 0);
}

#[test]
fn test_penalize_a_never_credited_player_stays_at_zero() {
    let (env, client, _admin, market, _referral) = setup();
    let user = Address::generate(&env);
    // Never called add_pts/reward for this user at all.
    client.penalize(&market, &user, &50_u64);
    assert_eq!(client.get_points(&user), 0);
}

#[test]
fn test_penalize_rejects_non_market_caller() {
    let (env, client, _admin, market, _referral) = setup();
    let user = Address::generate(&env);
    let rando = Address::generate(&env);
    client.add_pts(&market, &user, &50_u64, &true);
    let result = client.try_penalize(&rando, &user, &10_u64);
    assert!(
        result.is_err(),
        "penalize should reject a non-market caller"
    );
    // The player's balance must be untouched by the rejected call.
    assert_eq!(client.get_points(&user), 50);
}

#[test]
fn test_penalize_rejects_zero_points() {
    let (env, client, _admin, market, _referral) = setup();
    let user = Address::generate(&env);
    client.add_pts(&market, &user, &50_u64, &true);
    let result = client.try_penalize(&market, &user, &0_u64);
    assert!(
        result.is_err(),
        "penalize should reject a zero-point deduction"
    );
}

#[test]
fn test_penalize_rejects_banned_player() {
    let (env, client, admin, market, _referral) = setup();
    let user = Address::generate(&env);
    client.add_pts(&market, &user, &50_u64, &true);
    client.ban_player(&admin, &user);
    let result = client.try_penalize(&market, &user, &10_u64);
    assert!(
        result.is_err(),
        "penalize must reject a banned player like every other accrual path"
    );
}

#[test]
fn test_penalize_rejects_while_paused() {
    let (env, client, admin, market, _referral) = setup();
    let user = Address::generate(&env);
    client.add_pts(&market, &user, &50_u64, &true);
    client.pause(&admin);
    let result = client.try_penalize(&market, &user, &10_u64);
    assert!(result.is_err(), "penalize must respect the pause switch");
}

#[test]
fn test_penalize_does_not_touch_activity_counters() {
    let (env, client, _admin, market, _referral) = setup();
    let user = Address::generate(&env);
    client.add_pts(&market, &user, &100_u64, &true); // 1 win
    client.add_pts(&market, &user, &20_u64, &false); // 1 loss
    let before = client.get_stats(&user);

    client.penalize(&market, &user, &15_u64);

    let after = client.get_stats(&user);
    assert_eq!(after.points, before.points - 15);
    assert_eq!(
        after.won_bets, before.won_bets,
        "penalize must not touch won_bets"
    );
    assert_eq!(
        after.lost_bets, before.lost_bets,
        "penalize must not touch lost_bets"
    );
    assert_eq!(
        after.total_bets, before.total_bets,
        "penalize must not touch total_bets"
    );
}

#[test]
fn test_penalize_is_decay_aware() {
    // The deduction must land on the player's *current* (decayed) score, not
    // the stale value that was last written — otherwise a penalty applied
    // long after the fact would double-count decay that already happened.
    let (env, client, _admin, market, _referral) = setup();
    let user = Address::generate(&env);
    client.add_pts(&market, &user, &1_000_u64, &true);

    advance_periods(&env, 3);
    let decayed_before_penalty = client.get_points(&user);
    assert!(
        decayed_before_penalty < 1_000,
        "score should have decayed by now"
    );

    client.penalize(&market, &user, &50_u64);

    assert_eq!(client.get_points(&user), decayed_before_penalty - 50);
}

#[test]
fn test_penalize_drops_a_leader_below_a_weaker_ranked_player() {
    let (env, client, _admin, market, _referral) = setup();
    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    client.add_pts(&market, &alice, &100_u64, &true);
    client.add_pts(&market, &bob, &60_u64, &true);
    assert_eq!(client.get_rank(&alice), 1);
    assert_eq!(client.get_rank(&bob), 2);

    client.penalize(&market, &alice, &50_u64); // alice: 100 -> 50, now behind bob's 60

    assert_eq!(client.get_points(&alice), 50);
    assert_eq!(client.get_rank(&bob), 1, "bob should now lead");
    assert_eq!(client.get_rank(&alice), 2);

    let top = client.get_top_players(&0_u32, &2_u32);
    assert_eq!(top.get(0).unwrap().address, bob);
    assert_eq!(top.get(1).unwrap().address, alice);
}

#[test]
fn test_penalize_min_cache_stays_correct_after_reordering() {
    let (env, client, _admin, market, _referral) = setup();
    let a = Address::generate(&env);
    let b = Address::generate(&env);
    let c = Address::generate(&env);
    client.add_pts(&market, &a, &300_u64, &true);
    client.add_pts(&market, &b, &200_u64, &true);
    client.add_pts(&market, &c, &100_u64, &true);
    // The min cache is only rigorously maintained once the list has been
    // full at least once (or an existing entry has been updated) — with 3
    // of 50 slots filled nothing has forced a recompute yet, so prime it via
    // the permissionless keeper, same as a real integrator would.
    client.reconcile_top_slots();
    assert_eq!(client.get_min_points(), 100);

    // Penalize the current leader below the current minimum. The min cache
    // must be recomputed to reflect the new weakest entry, not left stale.
    client.penalize(&market, &a, &250_u64); // a: 300 -> 50, now the weakest

    assert_eq!(client.get_points(&a), 50);
    assert_eq!(
        client.get_min_points(),
        50,
        "min cache must track the new weakest entry"
    );
}

#[test]
fn test_penalize_never_inserts_an_unranked_player_into_the_top_list() {
    let (env, client, _admin, market, _referral) = setup();
    let ranked = Address::generate(&env);
    let never_ranked = Address::generate(&env);
    client.add_pts(&market, &ranked, &50_u64, &true);
    let count_before = client.get_top_player_count();

    // never_ranked has no Stats record and is not in the top list. Penalizing
    // them must not be the reason they newly appear in it — there is plenty
    // of room (the list is nowhere near MAX_TOP_PLAYERS), so the ordinary
    // "insert if there's room" path in update_top_players would happily add
    // them if it ever ran for this call, which is exactly what must not
    // happen for a penalty.
    client.penalize(&market, &never_ranked, &10_u64);

    assert_eq!(client.get_top_player_count(), count_before);
    assert_eq!(client.get_rank(&never_ranked), UNRANKED_RANK);
}

#[test]
fn test_penalize_never_evicts_a_ranked_player_on_an_unranked_players_behalf() {
    // Same guarantee as above, but with the list full: penalizing an
    // unranked player must not trigger the "full list, evict the weakest"
    // branch either.
    let (env, client, _admin, market, _referral) = setup();
    for i in 0u64..MAX_TOP_PLAYERS as u64 {
        let user = Address::generate(&env);
        client.add_pts(&market, &user, &(1000 - i), &true);
    }
    assert_eq!(client.get_top_player_count(), MAX_TOP_PLAYERS);
    let min_before = client.get_min_points();

    let never_ranked = Address::generate(&env);
    client.penalize(&market, &never_ranked, &10_u64);

    assert_eq!(client.get_top_player_count(), MAX_TOP_PLAYERS);
    assert_eq!(
        client.get_min_points(),
        min_before,
        "a penalty on an outsider must not touch the list"
    );
    assert_eq!(client.get_rank(&never_ranked), UNRANKED_RANK);
}

#[test]
fn test_penalize_emits_leaderboard_penalized_event() {
    let (env, client, _admin, market, _referral) = setup();
    let user = Address::generate(&env);
    client.add_pts(&market, &user, &100_u64, &true);
    client.penalize(&market, &user, &40_u64);

    // Same event-inspection pattern as
    // tests::test_add_pts_emits_leaderboard_updated — `env.events().all()`
    // returns a `ContractEvents` in soroban-sdk 26, exposed as an XDR slice
    // rather than an indexable Vec of (address, topics, data) tuples.
    let events = env.events().all();
    let emitted = events.events();
    assert!(!emitted.is_empty(), "penalize emitted no event");
    let soroban_sdk::xdr::ContractEventBody::V0(body) = &emitted.last().unwrap().body;
    let topic0 = Val::try_from_val(&env, &body.topics[0]).unwrap();
    let name = Symbol::try_from_val(&env, &topic0).unwrap();
    assert_eq!(name, Symbol::new(&env, "leaderboard_penalized"));
}
