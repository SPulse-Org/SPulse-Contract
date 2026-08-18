use crate::{PULSETokenContract, PULSETokenContractClient, TokenError};
use soroban_sdk::{testutils::Address as _, Address, Env, String};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Deploy a fresh PULSEToken contract and return its client.
fn setup(env: &Env) -> PULSETokenContractClient<'_> {
    let id = env.register(PULSETokenContract, ());
    PULSETokenContractClient::new(env, &id)
}

/// Generous cap so existing monetary tests never trip it.
const TEST_CAP: i128 = 1_000_000_000_000_000_000;

/// Initialize with standard PULSE metadata and a large default cap; returns the admin.
fn init(env: &Env, client: &PULSETokenContractClient<'_>) -> Address {
    init_with_cap(env, client, TEST_CAP)
}

/// Initialize with standard PULSE metadata and an explicit max supply cap.
fn init_with_cap(env: &Env, client: &PULSETokenContractClient<'_>, cap: i128) -> Address {
    let admin = Address::generate(env);
    client.initialize(
        &admin,
        &String::from_str(env, "PULSE"),
        &String::from_str(env, "PLSE"),
        &7,
        &cap,
    );
    admin
}

// ═══════════════════════════════════════════════════════════════════════════════
//  1. Initialize with metadata
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_initialize_with_metadata() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let _admin = init(&env, &client);

    assert_eq!(client.name(), String::from_str(&env, "PULSE"));
    assert_eq!(client.symbol(), String::from_str(&env, "PLSE"));
    assert_eq!(client.decimals(), 7);
    assert_eq!(client.total_supply(), 0_i128);
    assert_eq!(client.max_supply(), TEST_CAP);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  2. Add multiple authorized minters via set_minter
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_add_multiple_minters() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let _admin = init(&env, &client);

    let minter1 = Address::generate(&env); // e.g. PredictionMarket
    let minter2 = Address::generate(&env); // e.g. ReferralRegistry

    // Both succeed — no panic
    client.set_minter(&minter1);
    client.set_minter(&minter2);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  3. Mint by first authorized minter (PredictionMarket)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_mint_by_first_minter() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let _admin = init(&env, &client);

    let minter = Address::generate(&env);
    client.set_minter(&minter);

    let recipient = Address::generate(&env);
    client.mint(&minter, &recipient, &10_0000000_i128);

    assert_eq!(client.balance(&recipient), 10_0000000_i128);
    assert_eq!(client.total_supply(), 10_0000000_i128);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  4. Mint by second authorized minter (ReferralRegistry)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_mint_by_second_minter() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let _admin = init(&env, &client);

    let minter1 = Address::generate(&env);
    let minter2 = Address::generate(&env);
    client.set_minter(&minter1);
    client.set_minter(&minter2);

    let recipient = Address::generate(&env);
    client.mint(&minter1, &recipient, &10_0000000_i128);
    client.mint(&minter2, &recipient, &1_0000000_i128);

    assert_eq!(client.balance(&recipient), 11_0000000_i128);
    assert_eq!(client.total_supply(), 11_0000000_i128);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  5. Reject mint by non-minter
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_reject_mint_by_non_minter() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let _admin = init(&env, &client);

    let not_a_minter = Address::generate(&env);
    let recipient = Address::generate(&env);

    client.mint(&not_a_minter, &recipient, &10_0000000_i128); // panics
}

// ═══════════════════════════════════════════════════════════════════════════════
//  6. Remove minter via remove_minter and reject subsequent mint
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_remove_minter_then_reject() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let _admin = init(&env, &client);

    let minter = Address::generate(&env);
    client.set_minter(&minter);

    let recipient = Address::generate(&env);

    // Should work before removal
    client.mint(&minter, &recipient, &5_0000000_i128);
    assert_eq!(client.balance(&recipient), 5_0000000_i128);

    // Remove authorization
    client.remove_minter(&minter);

    // Should fail after removal
    client.mint(&minter, &recipient, &5_0000000_i128); // panics
}

// ═══════════════════════════════════════════════════════════════════════════════
//  7. Balance check after mint
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_balance_check_after_mint() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let _admin = init(&env, &client);

    let minter = Address::generate(&env);
    client.set_minter(&minter);

    let user_a = Address::generate(&env);
    let user_b = Address::generate(&env);

    // Zero before minting
    assert_eq!(client.balance(&user_a), 0_i128);
    assert_eq!(client.balance(&user_b), 0_i128);

    client.mint(&minter, &user_a, &100_0000000_i128);
    client.mint(&minter, &user_b, &50_0000000_i128);

    assert_eq!(client.balance(&user_a), 100_0000000_i128);
    assert_eq!(client.balance(&user_b), 50_0000000_i128);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  8. Transfer between accounts
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_transfer_between_accounts() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let _admin = init(&env, &client);

    let minter = Address::generate(&env);
    client.set_minter(&minter);

    let alice = Address::generate(&env);
    let bob = Address::generate(&env);

    client.mint(&minter, &alice, &100_0000000_i128);

    client.transfer(&alice, &bob, &30_0000000_i128);

    assert_eq!(client.balance(&alice), 70_0000000_i128);
    assert_eq!(client.balance(&bob), 30_0000000_i128);
    // Transfer does not change total supply
    assert_eq!(client.total_supply(), 100_0000000_i128);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  9. Reject transfer with insufficient balance
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_reject_transfer_insufficient_balance() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let _admin = init(&env, &client);

    let minter = Address::generate(&env);
    client.set_minter(&minter);

    let alice = Address::generate(&env);
    let bob = Address::generate(&env);

    client.mint(&minter, &alice, &10_0000000_i128);

    // Attempt to transfer more than balance
    client.transfer(&alice, &bob, &20_0000000_i128); // panics
}

// ═══════════════════════════════════════════════════════════════════════════════
//  10. Burn tokens
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_burn_tokens() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let _admin = init(&env, &client);

    let minter = Address::generate(&env);
    client.set_minter(&minter);

    let user = Address::generate(&env);
    client.mint(&minter, &user, &50_0000000_i128);

    client.burn(&user, &20_0000000_i128);

    assert_eq!(client.balance(&user), 30_0000000_i128);
    assert_eq!(client.total_supply(), 30_0000000_i128);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  11. Total supply tracking across mint, transfer, and burn
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_total_supply_tracking() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let _admin = init(&env, &client);

    let minter = Address::generate(&env);
    client.set_minter(&minter);

    let alice = Address::generate(&env);
    let bob = Address::generate(&env);

    // Mint 100 to Alice → supply = 100
    client.mint(&minter, &alice, &100_0000000_i128);
    assert_eq!(client.total_supply(), 100_0000000_i128);

    // Mint 50 to Bob → supply = 150
    client.mint(&minter, &bob, &50_0000000_i128);
    assert_eq!(client.total_supply(), 150_0000000_i128);

    // Transfer 20 from Alice to Bob → supply unchanged = 150
    client.transfer(&alice, &bob, &20_0000000_i128);
    assert_eq!(client.total_supply(), 150_0000000_i128);

    // Alice burns 30 → supply = 120
    client.burn(&alice, &30_0000000_i128);
    assert_eq!(client.total_supply(), 120_0000000_i128);

    // Bob burns 10 → supply = 110
    client.burn(&bob, &10_0000000_i128);
    assert_eq!(client.total_supply(), 110_0000000_i128);

    // Final: Alice = 100 - 20 - 30 = 50, Bob = 50 + 20 - 10 = 60
    assert_eq!(client.balance(&alice), 50_0000000_i128);
    assert_eq!(client.balance(&bob), 60_0000000_i128);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Supply cap — monetary policy enforced inside mint()
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_mint_below_cap_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = Address::generate(&env);
    client.initialize(
        &admin,
        &String::from_str(&env, "PULSE"),
        &String::from_str(&env, "PLSE"),
        &7,
        &100_i128,
    );

    let minter = Address::generate(&env);
    client.set_minter(&minter);

    let recipient = Address::generate(&env);
    client.mint(&minter, &recipient, &40_i128);
    assert_eq!(client.balance(&recipient), 40_i128);
    assert_eq!(client.total_supply(), 40_i128);
}

#[test]
fn test_mint_to_cap_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = Address::generate(&env);
    client.initialize(
        &admin,
        &String::from_str(&env, "PULSE"),
        &String::from_str(&env, "PLSE"),
        &7,
        &100_i128,
    );

    let minter = Address::generate(&env);
    client.set_minter(&minter);

    let recipient = Address::generate(&env);
    client.mint(&minter, &recipient, &40_i128);
    client.mint(&minter, &recipient, &60_i128);
    assert_eq!(client.balance(&recipient), 100_i128);
    assert_eq!(client.total_supply(), 100_i128);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_mint_above_cap_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = Address::generate(&env);
    client.initialize(
        &admin,
        &String::from_str(&env, "PULSE"),
        &String::from_str(&env, "PLSE"),
        &7,
        &100_i128,
    );

    let minter = Address::generate(&env);
    client.set_minter(&minter);

    let recipient = Address::generate(&env);
    client.mint(&minter, &recipient, &40_i128);
    client.mint(&minter, &recipient, &100_i128); // 40 + 100 > 100: panics
}

#[test]
fn test_mint_at_zero_cap_fails_closed() {
    // A legacy deployment with no declared cap (max_supply == 0) must fail
    // closed: no amount can be minted until the admin declares a cap.
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = Address::generate(&env);
    client.initialize(
        &admin,
        &String::from_str(&env, "PULSE"),
        &String::from_str(&env, "PLSE"),
        &7,
        &0_i128,
    );

    let minter = Address::generate(&env);
    client.set_minter(&minter);
    let recipient = Address::generate(&env);

    let res = client.try_mint(&minter, &recipient, &1_i128);
    match res {
        Err(Ok(e)) => assert_eq!(e, TokenError::MaxSupplyExceeded),
        other => panic!("expected MaxSupplyExceeded, got {other:?}"),
    }
    assert_eq!(client.balance(&recipient), 0_i128);
    assert_eq!(client.total_supply(), 0_i128);
}

#[test]
fn test_over_cap_mint_no_partial_update() {
    // A rejected mint must not touch recipient balance or total_supply.
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = Address::generate(&env);
    client.initialize(
        &admin,
        &String::from_str(&env, "PULSE"),
        &String::from_str(&env, "PLSE"),
        &7,
        &100_i128,
    );

    let minter = Address::generate(&env);
    client.set_minter(&minter);
    let recipient = Address::generate(&env);

    // Pre-existing balance on the recipient (from a prior mint) must survive.
    client.mint(&minter, &recipient, &20_i128);
    assert_eq!(client.balance(&recipient), 20_i128);

    let res = client.try_mint(&minter, &recipient, &100_i128);
    match res {
        Err(Ok(e)) => assert_eq!(e, TokenError::MaxSupplyExceeded),
        other => panic!("expected MaxSupplyExceeded, got {other:?}"),
    }
    assert_eq!(client.balance(&recipient), 20_i128);
    assert_eq!(client.total_supply(), 20_i128);
}

#[test]
fn test_shared_cap_across_minters() {
    // All authorized minters share one global cap.
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = Address::generate(&env);
    client.initialize(
        &admin,
        &String::from_str(&env, "PULSE"),
        &String::from_str(&env, "PLSE"),
        &7,
        &100_i128,
    );

    let minter_a = Address::generate(&env);
    let minter_b = Address::generate(&env);
    client.set_minter(&minter_a);
    client.set_minter(&minter_b);

    let recipient = Address::generate(&env);
    client.mint(&minter_a, &recipient, &40_i128);
    client.mint(&minter_b, &recipient, &40_i128);
    assert_eq!(client.total_supply(), 80_i128);

    // Third mint pushes 80 + 30 > 100: rejected for any minter.
    let res = client.try_mint(&minter_a, &recipient, &30_i128);
    match res {
        Err(Ok(e)) => assert_eq!(e, TokenError::MaxSupplyExceeded),
        other => panic!("expected MaxSupplyExceeded, got {other:?}"),
    }
    assert_eq!(client.total_supply(), 80_i128);
    assert_eq!(client.balance(&recipient), 80_i128);
}

#[test]
fn test_overflow_cannot_bypass_cap() {
    // checked_add must reject even an amount that would overflow i128.
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = Address::generate(&env);
    client.initialize(
        &admin,
        &String::from_str(&env, "PULSE"),
        &String::from_str(&env, "PLSE"),
        &7,
        &i128::MAX,
    );

    let minter = Address::generate(&env);
    client.set_minter(&minter);
    let recipient = Address::generate(&env);

    // Fill to the (i128::MAX) cap first, so the next add genuinely overflows.
    client.mint(&minter, &recipient, &i128::MAX);
    assert_eq!(client.total_supply(), i128::MAX);

    // supply(MAX) + amount(MAX) would wrap past i128::MAX: checked_add must
    // fail with MaxSupplyExceeded, never wrap around the cap.
    let res = client.try_mint(&minter, &recipient, &i128::MAX);
    match res {
        Err(Ok(e)) => assert_eq!(e, TokenError::MaxSupplyExceeded),
        other => panic!("expected MaxSupplyExceeded, got {other:?}"),
    }
    assert_eq!(client.total_supply(), i128::MAX);
}

#[test]
fn test_burn_frees_cap_headroom() {
    // Burning reduces total_supply, so the cap is re-usable.
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = Address::generate(&env);
    client.initialize(
        &admin,
        &String::from_str(&env, "PULSE"),
        &String::from_str(&env, "PLSE"),
        &7,
        &100_i128,
    );

    let minter = Address::generate(&env);
    client.set_minter(&minter);

    let recipient = Address::generate(&env);
    client.mint(&minter, &recipient, &60_i128);
    client.burn(&recipient, &30_i128);
    assert_eq!(client.total_supply(), 30_i128);

    // 30 + 70 == 100: fits again inside the cap.
    client.mint(&minter, &recipient, &70_i128);
    assert_eq!(client.total_supply(), 100_i128);
}

#[test]
fn test_lower_cap_ratchet() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = Address::generate(&env);
    client.initialize(
        &admin,
        &String::from_str(&env, "PULSE"),
        &String::from_str(&env, "PLSE"),
        &7,
        &100_i128,
    );

    // Authorized admin lowers the cap.
    client.set_max_supply(&admin, &50_i128);
    assert_eq!(client.max_supply(), 50_i128);

    // New mints bounded by the lowered cap.
    let minter = Address::generate(&env);
    client.set_minter(&minter);
    let recipient = Address::generate(&env);
    client.mint(&minter, &recipient, &50_i128);
    let res = client.try_mint(&minter, &recipient, &1_i128);
    match res {
        Err(Ok(e)) => assert_eq!(e, TokenError::MaxSupplyExceeded),
        other => panic!("expected MaxSupplyExceeded, got {other:?}"),
    }
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_raise_cap_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = Address::generate(&env);
    client.initialize(
        &admin,
        &String::from_str(&env, "PULSE"),
        &String::from_str(&env, "PLSE"),
        &7,
        &100_i128,
    );

    client.set_max_supply(&admin, &200_i128); // 200 > 100: panics
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_cap_below_current_supply_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = Address::generate(&env);
    client.initialize(
        &admin,
        &String::from_str(&env, "PULSE"),
        &String::from_str(&env, "PLSE"),
        &7,
        &100_i128,
    );

    let minter = Address::generate(&env);
    client.set_minter(&minter);
    let recipient = Address::generate(&env);
    client.mint(&minter, &recipient, &60_i128);

    client.set_max_supply(&admin, &50_i128); // 50 < 60: panics
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_set_cap_unauthorized_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = Address::generate(&env);
    client.initialize(
        &admin,
        &String::from_str(&env, "PULSE"),
        &String::from_str(&env, "PLSE"),
        &7,
        &100_i128,
    );

    // Non-admin cannot configure the cap.
    let attacker = Address::generate(&env);
    client.set_max_supply(&attacker, &50_i128); // panics
}

#[test]
fn test_set_cap_establishes_legacy_mint() {
    // Simulates a migrated legacy instance: admin declares a cap for the
    // first time via set_max_supply (the only way to go from 0/unset).
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = Address::generate(&env);
    client.initialize(
        &admin,
        &String::from_str(&env, "PULSE"),
        &String::from_str(&env, "PLSE"),
        &7,
        &0_i128,
    );
    assert_eq!(client.max_supply(), 0_i128);

    client.set_max_supply(&admin, &100_i128);
    assert_eq!(client.max_supply(), 100_i128);

    let minter = Address::generate(&env);
    client.set_minter(&minter);
    let recipient = Address::generate(&env);
    client.mint(&minter, &recipient, &100_i128);
    assert_eq!(client.total_supply(), 100_i128);
}
