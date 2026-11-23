# 0005 — Broker-order-id to client-order-id map is in memory only; no reconcile on reconnect

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: design-flaw · **Severity**: high
**Plan**: [0004](../plans/0004-design-review-2026-11.md)

## What's stubbed or hardcoded

- Stream correlation falls back to the nil UUID for unknown orders:
  - `crates/tantra-exec-zerodha/src/ticker.rs:209-210`
  - `crates/tantra-exec-upstox/src/ws.rs:229`
  - `crates/tantra-exec-angelone/src/ws.rs:202`
  - all use `(self.client_order_id_of)(&id).unwrap_or(uuid::Uuid::nil())`; the default lookup is `Arc::new(|_| None)` (`crates/tantra-exec-zerodha/src/ticker.rs:193`, `crates/tantra-exec-upstox/src/ws.rs:183`). Events for orders unknown after a restart, or placed from the broker app, silently carry the nil id.
- `crates/tantra-exec-router/src/sor.rs:131,146,315-318`: `ownership: Mutex<HashMap<String,String>>` is filled on every ack, never removed (no `.remove(` in `sor.rs`) and lost on restart. `owner()` (`sor.rs:405`) then fails cancel/modify for live orders.
- Reconnect loops (`crates/tantra-exec-zerodha/src/ticker.rs:329-`, config `max_attempts: 8` plus backoff) re-open the socket but never re-fetch `fetch_order_book` to replay missed fills. `fetch_order_book` is only a trait method (`crates/tantra-exec-core/src/executor.rs:134`).
- The adapter docs describe `get_order_status` as a polling fallback, but no code reconciles WS vs REST races.

## Why

Fills during a WebSocket gap are lost, so the OMS and ledger diverge from the broker. After a process restart, orders cannot be cancelled through the router and fills are attributed to the nil id. The ownership map also grows without bound over a long session.

## How to close it

1. Add an `OrderCorrelationStore` port keyed `(broker, account, broker_order_id)`: an in-memory impl here, a sqlite / `tantra-runtime-persistence` impl at L3. Replace the closure lookups.
2. Add a `ReconcilingStream` wrapper in `tantra-exec-core`: on every (re)connect call `fetch_order_book`, diff against the last-known status, and synthesize missing `BrokerExecutionEvent`s flagged `source: Reconciled`.
3. SOR: evict `ownership` entries on terminal status and rebuild from the order book on start.
4. Unknown broker orders surface as an explicit "unmatched" event, never the nil id.

**Verification**: in `tests/`, a scripted WS drop mid-fill, a restart rehydrate test, and a proptest on event-sequence idempotency. Gates as in plan 0004.

## Related

- Root gap 0010; [0004](0004-idempotent-submit-and-outcome-unknown.md).
- Root plan 0019 P1.5 touches the same WS channels (bounded channels only, a different issue).
- Plan 0004 R4 (graceful drain and gateway singleton) shares the reconnect loops.
