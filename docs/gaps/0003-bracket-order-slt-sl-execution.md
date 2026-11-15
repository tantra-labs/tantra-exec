# Gap 0003 — No Bracket-Order (SL / Multi-Target / Trailing-Stop) Execution Contract

**Status**: open — tracked by root master plans [`docs/plans/0005-elephant-edge-strategy-platform.md`](../../../docs/plans/0005-elephant-edge-strategy-platform.md) and [`docs/plans/0008-trade-signal-generator.md`](../../../docs/plans/0008-trade-signal-generator.md)

## What's Missing

`tantra-exec`'s `BrokerExecutor` routes plain orders (market/limit/stop) with
rate limiting, freeze slicing, TOTP auth, and the audit journal — but there is
**no bracket-order contract**: an entry order with an attached **stop-loss**,
one or more **partial-exit target legs** (`TargetLadder`), and a **trailing stop**
that can be *modified* in both price and remaining quantity as price moves or
partials fill. `tantra-core` plan `0005` produces `TradePlan` (entry, SL, trail
rule, target ladder); until this gap closes, `tantra-runtime` plan `0006`
emulates synthetic brackets via `TradeManagerActor`. Exec cannot currently express:

- placing an entry + initial stop (+ optional single target on CO/BO brokers:
  Zerodha BO/CO, Upstox, Dhan, Angel One),
- a resting **stop-market** child order whose quantity resizes atomically when a
  `PartialExit` leg fills,
- a **modify/re-arm** call to trail the stop (TSL updates) without leaving an
  unprotected window or double-fill race,
- multi-leg target handling (`PlannedTarget` ladder) and stop-trigger → close semantics.

## Why

Intraday EE+ and pattern-breakout signals are only safely executable if SL/TSL
and partial-exit quantity synchronization live in deterministic execution/runtime
paths (not in a Python strategy thread): the strategy hands a `TradePlan` to
the router, and the router/broker (or runtime synthetic OCO until native brackets
land) enforces the stop and keeps remaining stop qty equal to open position qty.
Today a stop must be re-submitted and re-modified manually, which is
non-deterministic, audit-incomplete, and slow. This is also the prerequisite for
`tantra-risk` gap 0005 and `tantra-ledger` gap 0012 to share native bracket state.

## How to Close It

1. **`BracketOrderSpec`** in `tantra-exec-core` consuming `tantra-core` plan
   `0005` canonical types: `{ entry: OrderRequest, initial_stop: Price,
   targets: Vec<PlannedTarget>, trail: TrailRule }` — all `Decimal`.
2. **`BrokerExecutor` contract addition**: `submit_bracket(spec) ->
   BracketTicket { entry_id, stop_id, target_ids }` and
   `modify_stop(bracket_id, new_stop, new_qty?) -> StopUpdate` (supporting both
   trailing price tightens and partial-exit quantity shrinks); adapters translate
   to broker-native BO/CO or emulated stop-child orders with honest per-broker
   capability flags (`native_bo`, `emulated_multileg`, `atomic_modify`).
3. **Trailing & synthetic-OCO runner**: coordinates with `tantra-core` `TradeManager`
   (`tantra-market` indicators supply inputs via caller/runtime), issues
   `modify_stop` / `partial_exit` with idempotent `client_order_id`s, and journals
   every change to the audit WAL.
4. Tests in `tests/` (TDD): bracket placement golden vectors, modify/re-arm,
   partial-exit SL qty resize, stop-trigger close, audit-journal entries per
   `modify_stop`; adapter-level contract tests for the native-vs-emulated capability matrix.

## Verification

`cargo test --workspace --profile prod` green in `tantra-exec`; a bracket
round-trip (entry fill → partial exit + SL qty shrink → TSL modify → stop
trigger → close) produces the expected audit trail; `make prod` green
end-of-turn (build-discipline rule).
