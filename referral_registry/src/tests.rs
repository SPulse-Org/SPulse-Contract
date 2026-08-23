use crate::{ReferralRegistryContract, ReferralRegistryError, DataKey};
use soroban_sdk::{
    testutils::Address as _,
    Address, Env, String,
};

fn make_env() -> Env {
    let env = Env::default();
    env.mock_all_auths();
    env
}

fn register_contract(env: &Env) -> soroban_sdk::Address {
    let id = env.register(ReferralRegistryContract, ());
    id
}

#[test]
fn test_successful_registration_and_get_display_name() {
    let env = make_env();
    let contract_id = register_contract(&env);
    let client = soroban_sdk::Address::generate(&env); // placeholder - will use direct calls
    let admin = soroban_sdk::Address::generate(&env);
    env.mock_all_auths();

    // Initialize the contract
    ReferralRegistryContract::new(&env, &contract_id).initialize(&admin).unwrap();

    let caller = soroban_sdk::Address::generate(&env);
    let name = String::from_str(&env, "CryptoKing");

    let result = ReferralRegistryContract::new(&env, &contract_id).register(&caller, &name);
    assert!(result.is_ok(), "register should succeed for valid name");

    let display = ReferralRegistryContract::new(&env, &contract_id).get_display_name(&caller);
    assert_eq!(display, Some(name));
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_reject_name_too_long() {
    let env = make_env();
    let contract_id = register_contract(&env);
    let admin = soroban_sdk::Address::generate(&env);
    env.mock_all_auths();

    ReferralRegistryContract::new(&env, &contract_id).initialize(&admin).unwrap();

    let caller = soroban_sdk::Address::generate(&env);

    let long_name = "a".repeat(65);
    let long_name_str = String::from_str(&env, &long_name);

    let result = ReferralRegistryContract::new(&env, &contract_id).register(&caller, &long_name_str);
    panic!("register should fail for name > 64 bytes, but got: {:?}", result);
}

#[test]
fn test_accept_64_bytes() {
    let env = make_env();
    let contract_id = register_contract(&env);
    let admin = soroban_sdk::Address::generate(&env);
    env.mock_all_auths();

    ReferralRegistryContract::new(&env, &contract_id).initialize(&admin).unwrap();

    let caller = soroban_sdk::Address::generate(&env);

    let exactly_64 = "a".repeat(64);
    let name_64 = String::from_str(&env, &exactly_64);

    let result = ReferralRegistryContract::new(&env, &contract_id).register(&caller, &name_64);
    assert!(result.is_ok(), "register should accept name of exactly 64 bytes");

    let display = ReferralRegistryContract::new(&env, &contract_id).get_display_name(&caller);
    assert_eq!(display, Some(name_64));
}

#[test]
fn test_multi_byte_utf8_boundary() {
    let env = make_env();
    let contract_id = register_contract(&env);
    let admin = soroban_sdk::Address::generate(&env);
    env.mock_all_auths();

    ReferralRegistryContract::new(&env, &contract_id).initialize(&admin).unwrap();

    let caller = soroban_sdk::Address::generate(&env);

    // 21 emoji * 3 bytes = 63 bytes - should be accepted
    let arabic_63_bytes: String = (0..21)
        .map(|_| String::from_str(&env, "🔥"))
        .collect();
    // 22 emoji * 3 bytes = 66 bytes - should be rejected
    let arabic_66_bytes: String = (0..22)
        .map(|_| String::from_str(&env, "🔥"))
        .collect();

    let result_63 = ReferralRegistryContract::new(&env, &contract_id).register(&caller, &arabic_63_bytes);
    assert!(result_63.is_ok(), "register should accept 63-byte UTF-8 string");

    let result_66 = ReferralRegistryContract::new(&env, &contract_id).register(&caller, &arabic_66_bytes);
    assert!(result_66.is_err(), "register should reject 66-byte UTF-8 string");

    let display_63 = ReferralRegistryContract::new(&env, &contract_id).get_display_name(&caller);
    assert_eq!(display_63, Some(arabic_63_bytes));
}