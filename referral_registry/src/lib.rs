#![no_std]

use soroban_sdk {
    contract, contracterror, contractimpl, contracttype, Env, String, Address, Symbol, Vec,
};

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ReferralRegistryError {
    DisplayNameTooLong = 1,
    AlreadyInitialized = 2,
    NotInitialized = 3,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    Profile(Address),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserProfile {
    pub display_name: String,
}

pub const INTERFACE_VERSION: u32 = 1;

#[contract]
pub struct ReferralRegistryContract;

#[contractimpl]
impl ReferralRegistryContract {
    pub fn initialize(env: Env, admin: Address) -> Result<(), ReferralRegistryError> {
        admin.require_auth();
        if env.storage().instance().has(&DataKey::Profile(admin.clone())) {
            return Err(ReferralRegistryError::AlreadyInitialized);
        }
        env.storage()
            .instance()
            .set(&DataKey::Profile(admin), &UserProfile {
                display_name: String::from_str(&env, ""),
            });
        Ok(())
    }

    pub fn register(env: Env, caller: Address, name: String) -> Result<(), ReferralRegistryError> {
        caller.require_auth();

        if name.len() > 64 {
            return Err(ReferralRegistryError::DisplayNameTooLong);
        }

        if env.storage().instance().has(&DataKey::Profile(caller.clone())) {
            return Err(ReferralRegistryError::AlreadyInitialized);
        }

        env.storage()
            .instance()
            .set(&DataKey::Profile(caller.clone()), &UserProfile {
                display_name: name.clone(),
            });

        Ok(())
    }

    pub fn get_display_name(env: Env, user: Address) -> Option<String> {
        let profile: UserProfile = env
            .storage()
            .instance()
            .get(&DataKey::Profile(user))
            .ok()?;
        Some(profile.display_name)
    }
}