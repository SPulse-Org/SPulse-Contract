use crate::{PULSETokenContract, PULSETokenContractClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger, LedgerInfo},
    Address, BytesN, Env, String,
};
use super::*;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Deploy a fresh PULSEToken contract and return its client.
fn setup(env: &Env) -> PULSETokenContractClient<'_> {
    let id = env.register(PULSETokenContract, ());
    PULSETokenContractClient::new(env, &id)
}

/// Initialize with standard PULSE metadata and return the admin address.
fn init(env: &Env, client: &PULSETokenContractClient<'_>) -> Address {
    let admin = Address::generate(env);
    client.initialize(
        &admin,
        &String::from_str(env, "PULSE"),
        &String::from_str(env, "PLSE"),
        &7,
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

// ═══════════════════════════════════════════════════════════════════════════
// SECURITY REGRESSION SUITE — issue #5 (timelocked upgrades)
// ═══════════════════════════════════════════════════════════════════════════

fn advance_ledgers(env: &Env, n: u32) {
    let current_seq = env.ledger().sequence();
    env.ledger().set(LedgerInfo {
        timestamp: env.ledger().timestamp() + 1,
        protocol_version: 26,
        sequence_number: current_seq + n,
        network_id: Default::default(),
        base_reserve: 10,
        min_temp_entry_ttl: 100,
        min_persistent_entry_ttl: 100,
        max_entry_ttl: 10_000_000,
    });
}

// A real, deployable WASM blob so upgrade tests exercise the actual host
// round-trip (upload + update_current_contract_wasm).
const TEST_WASM: &[u8] = include_bytes!("../../testdata/upgrade_test_wasm.wasm");

fn valid_wasm_hash(env: &Env) -> BytesN<32> {
    env.deployer().upload_contract_wasm(TEST_WASM)
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_upgrade_propose_non_admin_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    init(&env, &client);
    let rando = Address::generate(&env);
    let hash = valid_wasm_hash(&env);
    client.propose_upgrade(&rando, &hash);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_upgrade_execute_without_proposal_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = init(&env, &client);
    client.execute_upgrade(&admin);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_upgrade_execute_before_delay_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = init(&env, &client);
    let hash = valid_wasm_hash(&env);
    client.propose_upgrade(&admin, &hash);
    client.execute_upgrade(&admin); // too early
}

#[test]
fn test_upgrade_full_lifecycle_with_replay_protection() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = init(&env, &client);
    let contract_id = client.address.clone();
    let hash = valid_wasm_hash(&env);
    client.propose_upgrade(&admin, &hash);
    assert!(client.get_pending_upgrade().is_some());

    advance_ledgers(&env, UPGRADE_DELAY_LEDGERS + 1);
    client.execute_upgrade(&admin);

    // Replay is impossible — the proposal record is consumed on execution.
    // (After the swap the fixture wasm runs, so probe raw storage.)
    let pending: Option<PendingUpgrade> = env.as_contract(&contract_id, || {
        env.storage().instance().get(&DataKey::PendingUpgrade)
    });
    assert!(pending.is_none());
}

#[test]
fn test_upgrade_cancel() {
    let env = Env::default();
    env.mock_all_auths();
    let client = setup(&env);
    let admin = init(&env, &client);
    let hash = valid_wasm_hash(&env);
    client.propose_upgrade(&admin, &hash);
    client.cancel_upgrade(&admin);
    let res = client.try_execute_upgrade(&admin);
    assert!(res.is_err()); // NoPendingUpgrade (#7)
}
