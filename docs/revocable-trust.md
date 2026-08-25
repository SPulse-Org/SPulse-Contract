# Revocable Trust / Capability Model (issue #40)

## Problem

The cross-contract trust relationships were **static and one-way**: each
contract stored the addresses it trusts and enforced them at call time, but
there was no way to **sever** a relationship without knowing a replacement
address. Once a trusted contract (market / referral / leaderboard / token /
XLM SAC) was compromised or found malicious, the admin could only re-point the
address via `set_config` / `set_contracts` (issues #5/#6), which requires
(a) knowing a safe replacement and (b) coordinating a coordinated upgrade
(issue #39). During that window the compromised contract kept its privileges.

## What changed

Each contract now models every external dependency it trusts as a **role**.
The admin can revoke a role at any time via `revoke_trust` — an immediate,
replacement-free severing of the relationship. Until the admin explicitly calls
`restore_trust`, every operation that depends on the role **fails closed** with
a typed `TrustRevoked` error.

Key properties:

- **Revocation ≠ replacement.** `set_config` / `set_token_contract` /
  `set_xlm_sac` change the configured *address*; they do **not** clear a role's
  revocation. A replacement can therefore never silently bypass an emergency
  revocation — the role stays severed until `restore_trust`.
- **Restore is explicit and admin-only.** Only `restore_trust` re-enables a
  revoked role, and it is gated on the same admin auth as every other config
  mutation.
- **Self-revocation and self-restoration are impossible.** Only the stored
  admin address passes `require_admin`; a trusted contract (which is not the
  admin) can neither revoke the trust others placed in it nor re-enable itself.
  Note: a contract can still revoke trust in *its own dependency* (e.g. the
  leaderboard revoking its token), which is the intended emergency operation.
- **Fail-closed at the point of authorization.** The trust check runs inside
  the same guarded path that confirms the caller/address, before any state
  change or external call, so a compromised dependency cannot partially
  execute. Read-only views that do not need the dependency keep working.
- **Persistent, TTL-managed state.** The revocation flag is stored in
  persistent storage (`DataKey::TrustRevoked(Symbol)` → `bool`, absent =
  trusted) and is `extend_ttl`'d on both `revoke_trust` and `restore_trust`
  with the repository-standard `TTL_BUMP`/`TTL_HIGH`, so it follows the same
  lifecycle as every other long-lived flag (resolver, fee recipient).

## Roles per contract

| Contract | Roles (Symbols) | Operations that fail closed when revoked |
|---|---|---|
| `prediction_market` | `"referral"`, `"leaderboard"` | `place_bet` (referral fee routing + `credit` call), `claim` (leaderboard `reward` call) |
| `leaderboard` | `"market"`, `"referral"`, `"token"` | `add_pts`, `reward`, `record_bet` (market); `add_bonus_pts`, `reward_bonus` (referral); PULSE `mint` inside `reward`/`reward_bonus` (token) |
| `referral_registry` | `"market"`, `"leaderboard"`, `"token"`, `"xlm_sac"` | `credit` (market caller); `register_referral`, `credit` bonus path (leaderboard); fee payouts (xlm_sac). `"token"` is tracked for audit completeness — the registry does not invoke the token (the leaderboard mints internally), so it currently has no operational gate |

`pulse_token` is intentionally **unchanged**: mint authority there is already
an admin-managed role list (`set_minter` / `remove_minter`), i.e. a revocable
capability. Revoking the leaderboard's `"token"` role stops the leaderboard
from *calling* `mint`; the token's own minter list is the second, independent
layer of control. The two layers are complementary: sever one or both as the
situation requires.

## Entry points

Each contract exposes:

- `revoke_trust(admin, role: Symbol) -> Result<(), Err>` — admin only; `Admin`/
  `NotAdmin` checked, then `role` validated against the known set
  (`InvalidRole` for unknown symbols).
- `restore_trust(admin, role: Symbol) -> Result<(), Err>` — admin only; clears
  the revocation. This is the ONLY way to re-enable a revoked role.
- `is_trust_revoked(role: Symbol) -> bool` — public, read-only, no auth; anyone
  can observe revocation state.

## Where the guards are

The check `require_trust_not_revoked(env, role)` is invoked at the exact point
where the trust relationship is exercised:

- `prediction_market`: before reading the referral cache / transferring the
  referral fee in `place_bet`; before the leaderboard `require_interface_version`
  in `claim`.
- `leaderboard`: inside the caller-authorization helpers
  `require_market_contract` / `require_referral_contract` (therefore covering
  `add_pts`, `reward`, `record_bet`, `add_bonus_pts`, `reward_bonus`), and in
  the pre-state token block of `reward`/`reward_bonus` when `tokens > 0`.
- `referral_registry`: inside `require_market_contract` (covering `credit`),
  before the leaderboard `require_interface_version` in both
  `register_referral` and `credit`, and before the XLM SAC transfers.

Because these are all pre-effect checks, a revoked dependency causes a
**typed, atomic rejection** — no partial points, no partial mint, no partial
fee transfer, no consumed bet.

## Roles and the interface-versioning scheme

The trust check and the existing `require_interface_version` upgrades check are
orthogonal and both run:

| Layer | Question | Error |
|---|---|---|
| Trust | "is this dependency's role revoked?" | `TrustRevoked` |
| Upgrade coordination | "does the dependency expose a compatible ABI?" | `InterfaceVersionMissing` / `IncompatibleInterface` |

A contract that is revoked but on a compatible interface is rejected by the
trust layer; a contract that is trusted but upgraded incompatibly is rejected by
the version layer. Both run inside the same guarded block before any effects.

## Error codes

| Contract | `TrustRevoked` | `InvalidRole` |
|---|---|---|
| `leaderboard` | `LeaderboardError::8` | `LeaderboardError::9` |
| `prediction_market` | `MarketError::28` | `MarketError::29` |
| `referral_registry` | `ReferralError::9` | `ReferralError::10` |

## Emergency procedure

1. **Identify** the compromised trusted contract (e.g. a rogue leaderboard).
2. **Revoke** its role(s) with `revoke_trust(admin, "leaderboard")` on every
   contract that trusts it (the market, the referral registry, etc.). This is a
   single admin call, no replacement address needed.
3. **Verify** with `is_trust_revoked(role)` that the relationship is severed.
4. Dependent operations now fail closed with `TrustRevoked`; all funds and
   non-dependent views remain intact.
5. **Recover at leisure** — audit the breach, deploy a corrected contract
   (issue #5/upgrade), re-point the address via the existing setter
   (`set_config` / `set_token_contract` / `set_xlm_sac`), then explicitly
   `restore_trust(admin, role)` to re-enable. The replacement never bypasses
   the revocation.

## Behavior while revoked

- `prediction_market`: `place_bet` and `claim` are rejected. Market creation,
  resolution, cancellation, refunds (gross-fee flows via XLM SAC, which is not
  a market role), and the XLM SAC transfer paths do **not** depend on either
  role and keep working.
- `leaderboard`: `add_pts`/`reward`/`record_bet` (market revoked),
  `add_bonus_pts`/`reward_bonus` (referral revoked), and PULSE minting (token
  revoked) are rejected. `get_points`/`get_stats`/`get_top_players`/`get_rank`
  keep working.
- `referral_registry`: `credit` (market revoked), `register_referral` and the
  credit bonus leg (leaderboard revoked), and fee payouts (xlm_sac revoked) are
  rejected. Profile reads (`get_referrer`, `get_display_name`, `is_registered`)
  keep working.

## Tests

Each contract's suite covers: enabled-success, admin revoke → dependent paths
fail closed with the typed error (`TrustRevoked`) and no partial effects,
non-admin revoke/restore rejected (`NotAdmin`), unknown role rejected
(`InvalidRole`), admin restore → paths succeed again, `is_trust_revoked`
defaults to `false`, read-only views unaffected, and — critically — that
re-pointing the address (set_config / set_token_contract) does **not** clear a
revocation.

## Migration / deployment

No data migration is required:

- New storage key `TrustRevoked(Symbol)` is **absent** means "trusted", so
  every existing deployment behaves exactly as before until an admin revokes a
  role. Adding the variant to `DataKey` does not disturb existing keys.
- The three new entry points (`revoke_trust`, `restore_trust`,
  `is_trust_revoked`) are additive; no existing ABI changes.
- Existing callers that never invoke these functions are unaffected.

## Relationships intentionally left unchanged

- **`pulse_token` minters** — already an admin-managed, revocable capability
  list; the revocation model does not duplicate it.
- **XLM SAC in `prediction_market`** — filtered through `cfg.xlm_sac` for every
  on-chain transfer (bet intake, refunds, payouts, fee withdrawals). It is the
  network native-asset contract, not a Game-controller trusted peer, and is not
  part of the issue's trust list; it remains managed via `set_config`.