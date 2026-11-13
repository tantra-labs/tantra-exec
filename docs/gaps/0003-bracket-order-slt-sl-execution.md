# Gap 0003 — No Bracket-Order (SL / Target / Trailing-Stop) Execution Contract

**Status**: open — tracked by root master plan [`docs/plans/0005-elephant-edge-strategy-platform.md`](../../../docs/plans/0005-elephant-edge-strategy-platform.md)

## What's Missing

`tantra-exec`'s `BrokerExecutor` routes plain orders (market/limit/stop) with
rate limiting, freeze slicing, TOTP auth, and the audit journal — but there is
**no bracket-order contract**: an entry order with an attached **stop-loss**,
optional **target**, and a **trailing stop** that can be *modified* as price
moves. The strategy layer (`tantra-strategies` plan 0001) emits
`StopProtection` (initial SL + ATR/swing TSL); exec cannot currently express:

- placing an entry + stop as one bracket (CO/BO style, as supported by the
  Indian brokers: Zerodha BO/CO, Upstox, Dhan, Angel One),
- a resting **stop-market** child order for the SL,
- a **modify/re-arm** call to trail the stop (TSL updates),
- target-leg handling and stop-trigger → market-close semantics.

## Why

Intraday EE+ signals are only safely executable if the SL/TSL lives at the
execution layer (not the Python strategy thread): the strategy must be able to
hand `(entry, sl, trail)` to the router and forget, with the router + broker
enforcing the stop. Today a stop must be re-submitted and re-modified manually
per tick, which is non-deterministic, audit-incomplete, and slow. This is also
the prerequisite for `tantra-risk` gap 0005 (SL validation) and `tantra-ledger`
gap 0012 (bracket OMS lifecycle) to have real orders to validate/journal.

## How to Close It

1. **`BracketOrderSpec`** in `tantra-exec-core` (mirrors `tantra-core` plan
   0003 `StopProtection`): `{ entry: OrderRequest, stop_loss: Decimal,
   target: Option<Decimal>, trail: TrailMode, trail_distance: Option<Decimal>,
   activation_after_r: Option<Decimal> }` — all Decimal.
2. **`BrokerExecutor` contract addition**: `submit_bracket(spec) ->
   BracketTicket { entry_id, stop_id, target_id }` and
   `modify_stop(bracket_id, new_stop) -> StopUpdate`; adapters translate to
   broker-native BO/CO or emulated stop-child orders (honest per-broker
   capability flags: native BO vs emulated modify).
3. **Trailing runner**: a `TrailingStopManager` that recomputes the trail per
   bar/tick (`AtrTrail` / `SwingTrail` math lives in `tantra-market` gap 0005;
   exec owns the *call schedule*), issues `modify_stop`, and journals every
   change to the audit WAL.
4. Tests in `tests/` (TDD): bracket placement golden vectors, modify/re-arm,
   stop-trigger close, audit-journal entries per `modify_stop`; adapter-level
   contract tests for the native-vs-emulated capability matrix.

## Verification

`cargo test --workspace --profile prod` green in `tantra-exec`; a bracket
round-trip (entry fill → TSL modify → stop trigger → close) produces the
expected audit trail; `make prod` green end-of-turn (build-discipline rule).
