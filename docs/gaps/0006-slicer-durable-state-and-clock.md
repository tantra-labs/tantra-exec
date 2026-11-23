# 0006 — TWAP slicer has no durable parent state and reads the wall clock

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: design-flaw · **Severity**: high
**Plan**: [0004](../plans/0004-design-review-2026-11.md)

## What's stubbed or hardcoded

- `crates/tantra-exec-slicer/src/twap.rs:206-236` `run_twap(executor, stream, parent, plan, chase)` takes a single `&dyn BrokerExecutor`. There is no parent-state object; child fill accounting lives in a local `TwapOutcome`, so a process crash mid-schedule loses which legs were sent.
- `crates/tantra-exec-slicer/src/twap.rs:216` calls `Utc::now()` directly: the schedule is not driven by an injected clock.
- `crates/tantra-exec-router/src/sor.rs:131` ownership is per broker order id; there is no parent/child linkage between slicer children and routed accounts.
- `crates/tantra-exec-router/src/sor.rs:326-331` failover on `Transport | Timeout` (see [0004](0004-idempotent-submit-and-outcome-unknown.md)); `sor.rs:439` counts a health failure for the same set, so one ambiguous timeout also trips the circuit.

## Why

After a crash or failover a TWAP can resubmit already-sent legs or exceed the parent quantity. The router cannot answer "how much of parent X is live across accounts". The wall-clock read also makes schedules non-deterministic in tests and backtests.

## How to close it

1. Add a persisted `ParentOrderState` (parent id, legs sent / acked / filled) behind a port; `run_twap` resumes from it.
2. Make leg client ids deterministic (`{parent}#{leg}`) so the `find_by_client_order_id` lookup from gap 0004 deduplicates resubmits.
3. Inject a `Clock` trait into the slicer and drive the schedule from it.
4. Record the parent/child linkage in the router so per-parent live quantity is queryable.

**Verification**: in `tests/`, a simulated crash between legs asserting no resubmit and no over-fill; a proptest that the sum of leg quantities never exceeds the parent; a fake-clock schedule test. Gates as in plan 0004.

## Related

- [0004](0004-idempotent-submit-and-outcome-unknown.md), [0005](0005-durable-order-id-map-and-reconcile.md).
- Plan 0004 R4 (drain policy for open slicer legs and bracket stops).
