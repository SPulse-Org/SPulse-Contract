# Cross-Contract Upgrade Coordination (issue #39)

Every contract in this workspace can be upgraded **independently** via its own
`upgrade()` (issue #5): `prediction_market`, `referral_registry`, `leaderboard`,
and `pulse_token`. They are tightly coupled through cross-contract calls, so an
uncoordinated upgrade that changes any called ABI breaks the whole system.

This document describes the interface-versioning scheme added to solve that.

## Dependency graph protected by this scheme

| Caller | Callee | Called function | Guard |
|---|---|---|---|
| `prediction_market.place_bet` | `referral_registry` | `credit` | `REFERRAL_CREDIT_INTERFACE_VERSION` |
| `prediction_market.claim` | `leaderboard` | `reward` | `LEADERBOARD_REWARD_INTERFACE_VERSION` |
| `referral_registry.register_referral` | `leaderboard` | `reward_bonus` | `LEADERBOARD_BONUS_INTERFACE_VERSION` |
| `referral_registry.credit` | `leaderboard` | `add_bonus_pts` | `LEADERBOARD_BONUS_INTERFACE_VERSION` |
| `leaderboard.reward` / `reward_bonus` | `pulse_token` | `mint` | `TOKEN_MINT_INTERFACE_VERSION` |

Every guard is executed **before any state change or cross-contract invoke** in
the caller, so a failed check reverts the whole transaction without partial
effects.

## Versioning model

- Each contract stores a single `u32` **interface version** in its own instance
  storage (`DataKey::InterfaceVersion`), committed at `initialize()` time to the
  crate-level `INTERFACE_VERSION` constant.
- Every contract exposes `interface_version() -> u32`, a read-only getter that
  returns `0` when the key is absent (an un-migrated/legacy deployment).
- A caller that invokes a dependency first calls
  `require_interface_version(env, dependency_address, required_version)`:
  - `0` / missing getter (host error) → typed `InterfaceVersionMissing`.
  - reported version `< required_version` → typed `IncompatibleInterface`.
  - otherwise the call proceeds.
- Versions are **monotonically increasing integers**. Bump the relevant
  `*_INTERFACE_VERSION` requirement constant only when the ABI *you call*
  (signature, argument order, return type) changes on the dependency.
- Fail-closed semantics: an unverifiable dependency is treated as incompatible,
  never as "probably fine".

## Upgrade procedure (operational)

A safe upgrade is **coordinated**, even though each contract can be upgraded
independently:

1. **Prepare**: write the new WASM, bump the `INTERFACE_VERSION` constant in the
   upgraded contract's source to the next integer, and keep the *callee-facing*
   ABI stable unless you are intentionally breaking it.
2. **Deploy the new WASM** via the upgraded contract's `upgrade()`.
   `upgrade()` only swaps the code — it does **not** change storage or versions.
3. **Committing a changed ABI**: if the upgrade changed any function that other
   contracts call (`credit`, `reward`, `reward_bonus`, `add_bonus_pts`, `mint`,
   `interface_version` itself), the admin must call
   `set_interface_version(admin, <new_version>)` on the upgraded contract.
4. **Upgrade callers** to require the new minimum where relevant (update the
   appropriate `*_INTERFACE_VERSION` constant in their source), then deploy them
   with `upgrade()`. Since the version lives in storage, existing callers keep
   running against the old contract until their own upgrade lands.
5. **Deploy order that minimizes downtime** (each step is additive):
   - `pulse_token` first (its `interface_version` getter is the leaf dependency).
   - `leaderboard` second (it is *called by* market and referral, and *calls*
     the token).
   - `referral_registry` and `prediction_market` last (they only call others).
   With all four at version `1` (the initial value), no setter call is needed at
   all — `initialize()` already stored it.

## Migration note for existing deployments

Contracts deployed **before** this scheme have no `InterfaceVersion` key.
`interface_version()` returns `0` for them, so any caller requiring version `1`
will fail closed with `InterfaceVersionMissing`. To migrate:

- If the deployed contract was logged with the new code and you want it to serve
  version `1`, call `set_interface_version(admin, 1)` on it — no redeploy needed.
- `pulse_token.set_interface_version` takes only `(env, version)` and
  authorizes via its stored admin; the other contracts take
  `(env, admin, version)`.

There is no on-chain migration task; this is a single admin call per contract.

## Failure mode and recovery

- **Symptom**: `Error(Contract, #27)`/`#26` (market), `#8`/`#7` (referral),
  `#7`/`#6` (leaderboard) — `IncompatibleInterface` / `InterfaceVersionMissing`.
- **Cause**: a dependency's declared interface version is below the caller's
  required minimum, or the dependency never exposed the version getter.
- **Effect**: the specific cross-contract call (and therefore the whole
  transaction: bet / claim / registration / referral credit / reward) fails
  closed. No partial state is written.
- **Recovery**: either
  1. declare the correct version on the dependency via `set_interface_version`
     (coordinate so the version matches the deploy), or
  2. upgrade/roll back the contract to an ABI-consistent version, then
     re-declare the version.
  No data migration or redeployment of unaffected contracts is required.

## Tests

See `*/src/tests.rs`:

- compatible path (full call succeeds with the dependency at version `1`),
- `IncompatibleInterface` when the dependency is downgraded to `0`,
- `InterfaceVersionMissing` when the dependency has no version getter,
- upgrade-path tests proving a unilateral incompatible upgrade fails closed and
  recovers after `set_interface_version` without touching user data.