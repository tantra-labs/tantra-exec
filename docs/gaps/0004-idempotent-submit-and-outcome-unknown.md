# 0004 — `client_order_id` never reaches the broker; no idempotent submit or unknown-outcome handling

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: design-flaw · **Severity**: critical
**Plan**: [0004](../plans/0004-design-review-2026-11.md)

## What's stubbed or hardcoded

- The broker `tag` slot carries the SEBI algo id, not the client order id:
  - Zerodha `crates/tantra-exec-zerodha/src/lib.rs:162` `form.insert("tag", req.algo_id.clone())`
  - Upstox `crates/tantra-exec-upstox/src/lib.rs:148` `tag: Some(req.algo_id.clone())`
  - AngelOne `crates/tantra-exec-angelone/src/lib.rs:113`
- `client_order_id` appears only in the returned `OrderAck` (zerodha `crates/tantra-exec-zerodha/src/lib.rs:332`, angelone `crates/tantra-exec-angelone/src/lib.rs:463`); it is never on the wire.
- `crates/tantra-exec-zerodha/src/lib.rs:184-196`: any `reqwest` send error, including the 15 s timeout (`crates/tantra-exec-zerodha/src/lib.rs:85`), becomes `BrokerError::Transport(..)`. `BrokerError::Timeout` is never constructed by an adapter (only by `crates/tantra-exec-core/src/ratelimit.rs`).
- `crates/tantra-exec-core/src/error.rs:6-35` has no "outcome unknown / maybe accepted" variant.
- `crates/tantra-exec-router/src/sor.rs:326-329` treats `Transport | Timeout` as a health failure and fails over to the next broker with `req.clone()` (same client id), although a timeout can follow a successful broker accept.
- Nothing calls `fetch_order_book` to look up a prior submission by tag or client id.

## Why

A submit that times out after Kite accepted it is reported as `Transport`. The SOR then places the same order on another account (double exposure), or the strategy retries on the same account (duplicate order). The SEBI audit trail also cannot join a broker order to its client order id. This is the exec-side half of root gap 0010.

## How to close it

1. Add `BrokerError::OutcomeUnknown { client_order_id }`, non-retryable without reconciliation. Adapters map `reqwest::Error::is_timeout()` / `is_request()` after the body was sent to it, and `is_connect()` to `Transport`.
2. Define one shared `OrderTag` type in `tantra-exec-core` carrying both ids (Kite tag is max 20 chars: e.g. base32 of the client order id; the algo id moves to the exchange-specific algo field or a separate registry).
3. Add `BrokerExecutor::find_by_client_order_id` (default via `fetch_order_book` plus tag match). The SOR must call it before any failover on `OutcomeUnknown`.
4. Stop counting an ambiguous timeout as a circuit-breaker health failure.

**Verification**: in `tests/`, a wiremock submit-then-drop-connection test; a proptest on `OrderTag` encode/decode round trip; a router test asserting no failover on unknown outcome; per-broker golden wire vectors. Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `make check`, targeted `cargo test -p <crate> --test <bin> --profile prod`, `make prod`.

## Related

- Root gap 0010 (end-to-end order idempotency and unknown-outcome handling).
- [0005](0005-durable-order-id-map-and-reconcile.md) (order-id map and reconcile), [0006](0006-slicer-durable-state-and-clock.md) (deterministic leg ids rely on this lookup), [0011](0011-order-normalisation-tick-lot-validity.md) (algo id validation).
- Plan 0004 R5 (typed `BrokerError` classification, `BrokerOrderId`) builds on the new variant.
