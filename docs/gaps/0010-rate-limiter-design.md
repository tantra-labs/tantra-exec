# 0010 — Rate limiter: one bucket per broker, per process, hard-wired `Instant::now()`, waits up to 24 h

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: design-flaw · **Severity**: medium
**Plan**: [0004](../plans/0004-design-review-2026-11.md)

## What's stubbed or hardcoded

- `crates/tantra-exec-core/src/ratelimit.rs:178-185` `BrokerRateCatalog::for_broker(name)` returns one `RateLimiterConfig` per broker. `RateLimitedBroker` (`ratelimit.rs:553-598`) charges `Decimal::ONE` from the same bucket for every call (orders, book polls, health). The Zerodha order limit (3/s) and general limit (10/s) are distinct per the docs at `crates/tantra-exec-zerodha/src/lib.rs:96-98`.
- `ratelimit.rs:261,301,309,340,372,383` call `Instant::now()` directly: no injectable clock, so tests need real sleeps.
- `ratelimit.rs:457-463` `acquire` waits up to `24*60*60` s.
- Keyed by broker name only: not per API key or account, and state is process-local. N replicas multiply the effective rate, and the SEBI OPS threshold (10 orders/s) is not enforced across them.
- `for_broker` silently falls back to `RateLimiterConfig::default()` for unknown names (`ratelimit.rs:183`).

## Why

A health-check or order-book poll loop can starve order placement; two accounts on the same broker share one bucket; scaling to replicas breaks broker rate limits and the OPS threshold.

## How to close it

1. Add `RateClass { Order, Read, Modify }` to `BrokerRateCatalog` with per-class buckets.
2. Key limiters by `(broker, account_id)`.
3. Inject a `Clock` (tokio `Instant`, using `tokio::time::pause` in tests) instead of `Instant::now()`.
4. Add a `RateLimitStore` port with an in-process implementation and a shared (Redis/DB) implementation for replicas.
5. Cap the default `max_wait` to a small bound; return an error for unknown broker names.

**Verification**: proptest on token-bucket refill invariants (never exceeds capacity, never negative); paused-clock tests for per-class isolation and `max_wait`; a test that unknown broker is an error. Gates as in plan 0004.

## Related

- Plan 0004 R4 (gateway singleton and shared state across replicas); root plan 0022.
- Plan 0004 R3 (the Python `rate_limit.py` copy in `tantra-ffi/python/tantra/adapters/`).
