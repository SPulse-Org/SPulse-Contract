# Prediction Market — Claimable-State TTL Lifecycle (issue #9)

## What the issue is

Markets, bet entries, bettor indexes, and resolution-time payout keys all live
in **persistent storage**, which Soroban deletes once an entry's live-until
ledger is reached unless the contract keeps re-arming it. Every user payment
that ends up in a market pool can only be recovered through `claim` /
`cancel_refund`, and both paths depend on those persistent entries still
existing. If they expire first, funds are **permanently stuck in the contract**.

Soroban cannot resurrect an already-expired (deleted) entry: once gone, reads
return the same not-found result as for an entry that never existed. The fix is
therefore a **read-time TTL refresh strategy**: every lifecycle operation that
may still be needed later re-arms the TTL of the entries it reads/depends on,
so the recovery window keeps sliding forward as long as anyone is interacting
with the contract.

## Storage keys that must stay alive for fund recovery

| Key | Written by | Read by (recovery) | Why it must stay alive |
|---|---|---|---|
| `Market(market_id)` | `create_market`, `place_bet`, `resolve_market`, `cancel_market` | `claim`, `cancel_refund`, `get_market` | `resolved`/`cancelled`/`outcome` gate the recovery paths; a missing market bricks the whole market's claims/refunds |
| `Bet(market_id, user)` | `place_bet`, updated by `claim`/`cancel_refund` | `claim`, `cancel_refund`, `get_bet` | determines stake, winning side, and `claimed`/refunded idempotency; a missing bet = `NoBetFound` |
| `Payout(market_id, user)` | `resolve_market` (winners only) | `claim`, `get_payout` | the exact XLM payout; a missing payout silently pays 0 to a winner |
| `BettorCount(market_id)` | `place_bet` (lazily, first bet) | `resolve_market`, `get_market_bettors_page` | drives winner enumeration at resolve; a missing index drops bettors from payout computation |
| `BettorAt(market_id, i)` | `place_bet` (lazily, first bet) | `resolve_market`, `get_market_bettors_page` | the index slots that enumerate bettors at resolve |

## Where TTL is extended

All bumps use the repository's existing constants `TTL_BUMP` / `TTL_HIGH`
(`prediction_market/src/lib.rs`) and the existing inline
`extend_ttl(key, TTL_BUMP, TTL_HIGH)` convention. A tiny helper
`PredictionMarketContract::bump_ttl(env, key)` applies the same call; callers
only invoke it on keys they have already confirmed to exist (the host errors
when extending a deleted/non-existent key).

### Read-time extension (the primary mechanism)

- **`claim`** — bumps `Bet` (already done), `Market` (already done), and now
  also `Payout(market_id, user)` **if present** (winners). The three entries a
  winner needs stay alive together for the whole claim path.
- **`cancel_refund`** — bumps `Bet` and `Market` (already done): the refund
  path on a cancelled market stays open while the user still interacts.
- **`resolve_market`** — bumps `BettorCount`, every `BettorAt` slot, and every
  `Bet` entry it walks while computing payouts, and each new `Payout` key it
  writes. A long-lived market (bets placed near the start of a multi-year
  window) otherwise risks its index expiring before resolution, silently
  dropping bettors from the enumeration and locking their stake.
- **`get_market`** — bumps `Market`.
- **`get_bet`** — bumps `Bet`.
- **`get_payout`** — bumps `Payout` when present.
- **`get_market_bettors_page`** — bumps `BettorCount` and each `BettorAt` slot.

View functions are a deliberate part of the strategy: a user checking their bet,
payout, or the market is the cheapest possible "keep-alive" interaction, and it
works without a keeper.

## Design decisions

- **Read-time extension over a keeper/off-chain refresher.** The repository has
  no keeper infrastructure, and a keeper would be an external trust dependency.
  Read-time refresh needs no extra moving parts.
- **Both read-time AND resolution-time extension are used.** Resolution-time
  bumps guarantee newly created `Payout` keys plus the already-written bet/index
  entries start (and re-start) with a full `TTL_HIGH` window; read-time bumps on
  claim/refund/views then keep sliding that window while the state is still
  relevant.
- **`Market` and `BetEntry` are kept alive together.** The claim path bumps the
  whole triple `Market` + `Bet` + `Payout`; the refund path bumps `Market` +
  `Bet`. No key that a recovery path needs can outlive its partner on its own.
- **Missing/expired state semantics are preserved.** `extend_ttl` is only ever
  called on keys proven to exist in the same call, so genuinely absent entries
  (e.g. no bet placed) still produce the existing `MarketNotFound` /
  `NoBetFound` errors, and a stale `Payout` read still yields `0`.
- **Terminal behavior is unchanged.** `claim` still marks `claimed` and pays
  out via the settlement-time payout ledger; `cancel_refund` still zeroes
  `gross`. The TTL changes touch only expiry windows, not money movement.
- **No TTL constants were increased.** The existing `TTL_BUMP` / `TTL_HIGH`
  (~1yr/~2yr at mainnet ~5s ledgers) are reused; extending them would only
  postpone, not solve, expiry, and the fix here already slides them on read.
- **Scope.** This change touches `prediction_market` only. Leaderboard
  (`issue #21`) and token-balance (`issue #36`) TTL are separate issue tracks
  and were not modified.

## Remaining limitation (must be stated explicitly)

Read-time TTL refresh **cannot resurrect an entry that has already expired**.
If no one interacts with the contract — no claim, no refund, no view call —
for the full TTL window after the last bump, the entries are deleted by the
ledger and the funds behind them are unrecoverable. This fix changes the
recovery guarantee from "the TTL from the last **write**" to "the TTL from the
**last interaction**", which is strictly stronger but still bounded.

Permanent recoverability would require either a persistent on-chain keeper
(repeatedly bumping keys on a schedule) or a storage-rental redesign — neither
exists in this repository, so the contract-level fix cannot honestly promise it.

## Tests

See `prediction_market/src/tests.rs` (SECURITY REGRESSION SUITE — issue #9):

- `test_claim_rebumps_ttl_entries` — claim bumps `Bet` + `Market` (existing).
- `test_claim_rebumps_payout_ttl` — **new**: claim bumps the winner's `Payout`.
- `test_cancel_refund_rebumps_ttl_entries` — refund bumps `Bet` + `Market`.
- `test_resolve_extends_claimable_state_ttl` — **new**: resolve bumps the
  bettor index, `BettorCount`, both `Bet` entries, and creates fresh-`TTL`
  `Payout` keys for multiple winners.
- `test_get_bet_extends_ttl`, `test_get_market_extends_ttl`,
  `test_get_payout_extends_ttl`, `test_get_market_bettors_page_extends_index_ttl`
  — **new**: view interactions keep claimable state alive.
- `test_missing_state_preserves_not_found_semantics` — **new**: no writer
  resurrects genuinely absent keys; not-found errors and zero payouts preserved.
- Full existing suite (market lifecycle, claim/refund payout math, upgrade
  coordination) unchanged and passing.