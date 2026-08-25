# PULSE Supply Cap (issue #34)

## Cap value — source

The repository contains **no authoritative tokenomics value**: no `MAX_SUPPLY`
constant existed, no deployment configs/scripts exist, the removed
`ipredict_token` contract had none, and neither `ISSUE_DRAFT.md` nor
`issues/*.md` specify a number. Per the issue's guidance ("if no authoritative
cap exists, implement the smallest safe configuration mechanism... clearly
document the chosen migration/default semantics"), the cap is therefore a
**deployer-supplied parameter** passed to `PULSETokenContract::initialize(...)`
rather than a invented compile-time constant.

- The deployer chooses `max_supply` (in base units, i.e. scaled by `decimals`)
  at deployment time and that value becomes the monetary policy.
- Once declared it can only move **down** (admin one-way ratchet) — it can
  never be raised through the token contract.
- There is deliberately **no hard-coded default number** in the source.

## Where the cap is enforced

Enforcement lives **inside `pulse_token::mint()`** (`pulse_token/src/lib.rs`),
the single choke point all minting goes through:

- Every authorized minter — `leaderboard.reward`, `leaderboard.reward_bonus`
  (the two production mint paths, both mediated by the leaderboard), or any
  future minter granted `set_minter` — calls `mint()` and is subject to the
  same global ceiling. No caller-side enforcement is required.
- `mint()` computes `new_supply = total_supply.checked_add(amount)`,
  **before** writing either balance or supply; if `new_supply > max_supply`
  (or the addition would overflow), the whole mint rejects with
  `TokenError::MaxSupplyExceeded` (#7) and **no storage is modified**.
- `checked_add` guarantees arithmetic overflow cannot wrap around and bypass
  the ceiling.
- `burn()` decreases `total_supply`, so burned PULSE frees cap headroom.

Because reward/reward_bonus mint through the token, an over-cap reward fails
the entire cross-contract call atomically (the leaderboard's internal `mint`
invoke reverts the surrounding `reward`/`reward_bonus`).

## Behavior when a mint would exceed the cap

- The mint returns `TokenError::MaxSupplyExceeded` (#7).
- Recipient balance is unchanged; `total_supply` is unchanged.
- In the leaderboard paths this reverts `reward` / `reward_bonus` (and, via
  `prediction_market.claim`, the whole claim), keeping the supply invariant.

## Governance

- `max_supply(env) -> i128` read-only getter.
- `set_max_supply(env, admin, new_cap)` — admin only:
  - `new_cap < current total_supply` → `TokenError::CapBelowCurrentSupply` (#8).
  - raising an already-declared cap → `TokenError::CapTooHigh` (#9).
  - only lowering (or first-time establishing) is allowed.
  - non-admin callers → `TokenError::NotAdmin` (#6) or host auth failure.

## Deployment / migration implications

- **New deployments:** pass the desired `max_supply` to `initialize`. It is
  stored in instance storage (`DataKey::MaxSupply`) alongside `TotalSupply`.
- **Existing deployed instances** (pre-upgrade) have **no `MaxSupply` key**.
  `max_supply()` reads it as `0`, and `mint()` fails closed with
  `MaxSupplyExceeded` until the admin declares a cap. To migrate: call
  `set_max_supply(admin, cap)` once (any value ≥ current total_supply); after
  that the one-way ratchet applies. This is the explicit, documented migration
  step — no data migration is needed, and unrelated reads
  (`balance`/`total_supply`/`transfer`/`burn`) are unaffected.
- The token's cross-contract ABI used by the leaderboard (`mint`,
  `interface_version`) is unchanged, so `TOKEN_MINT_INTERFACE_VERSION` remains
  `1`; no redeploy/version bump of the leaderboard is required for the cap.

## All mint paths share the same cap

| Path | Mint caller on the token |
|---|---|
| `prediction_market.claim` | `leaderboard.reward` → `token.mint` |
| `referral_registry.register_referral` | `leaderboard.reward_bonus` → `token.mint` |
| `referral_registry.credit` | `leaderboard.add_bonus_pts` (no token mint) |
| any future authorized minter | direct or via leaderboard |

All route through `pulse_token::mint()`, so one global cap applies to every
path.

## Tests

See `pulse_token/src/tests.rs` and `leaderboard/src/tests.rs`:

- mint below / exactly at / above cap (typed `MaxSupplyExceeded`),
- over-cap mint leaves recipient balance and `total_supply` untouched,
- zero-cap (legacy/unset) fails closed until a cap is declared,
- multiple authorized minters share one global cap,
- `i128` overflow mint cannot bypass the cap (`checked_add`),
- burn frees cap headroom,
- cap ratchet: authorized lowering works; raising (`#9`), lowering below
  current supply (`#8`), and non-admin configuration (`#6`) are rejected,
- `reward`- and `reward_bonus`-triggered mints are capped (leaderboard tests).