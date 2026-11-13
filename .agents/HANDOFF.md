# HANDOFF — tantra-exec

**Status**: All 8 crates migrated and green. `tantra-exec-angelone` SmartAPI
2.0 adapter implemented (gap 0001 closed, plan 0002 Phase 1 done). Audit
storage gap 0002 closed (JSONL journal canonical; runtime's
`SqliteAuditLedger` complementary).
**Date**: 2026-11-13 · Repo: `tantra-exec` · Branch: `main`

## Phase / Slice Status (current batch — Angel One adapter, plan 0002 complete)

- `tantra-exec-angelone` implemented (`feat(angelone): SmartAPI 2.0
  BrokerExecutor adapter with order stream and exact-decimal wire payloads`,
  `2258bbb`): REST executor (place/modify/cancel, order book, positions with
  signed-quantity → Long/Short/Flat, margins via RMS, health via profile,
  `get_order_status`), SmartStream order feed → `ExecutionStream`
  (`AngelOneOrderStream`, delta-tracked fills), `BrokerEnumMap` wire tables in
  `src/mapping.rs`, `map_angelone_error`, `rate_limited()` (10/s preset),
  `SessionTokens` expiry wiring (`feed_token`, 03:30 IST roll-off via
  `next_ist_run`). Cargo.toml mirrors upstox + `tantra-exec-auth`.
- Tests: `tests/{wire_mapping,angelone_payload,angelone_rest,angelone_ws}_tests.rs`
  + smoke `integration_tests.rs` — **40 angelone tests** green.
- Verification (from repo root): `cargo fmt --check` clean; `cargo clippy
  --workspace --all-targets --profile prod -- -D warnings` clean; `cargo test
  --workspace --profile prod` green; `make prod` refresh.
- Gap 0001 closed (doc removed, Closed table updated); plan 0002 fully
  implemented and retired to the Implemented table; gap 0002 already closed
  (prior batch).

## Decisions & Discoveries

- `PositionSide` (with `Flat`) lives in `tantra_exec_core::types` (root
  re-export), NOT `tantra_core::PositionSide` (older, no `Flat`). Zero-quantity
  positions map to `Flat`.
- Rate limiting is in `tantra_exec_core::ratelimit` (root re-exports
  `BrokerRateCatalog`, `RateLimitedBroker`); `BrokerRateCatalog::for_broker("angelone")`
  = 10/s preset; `RateLimitedBroker::with_config(inner, config)` wrapper.
- `wire::*` decode helpers take `&serde_json::Value` (`decimal_from_json`,
  `price_from_json`, `quantity_from_json`, `integer_from_json`,
  `parse_broker_ist_datetime`, `parse_ist_time_of_day`, `ist_today`).
- Audit journal is a **fsynced JSONL segment WAL**; the runtime owns the
  operational SQLite audit store (`SqliteAuditLedger`) — complementary roles.
- `RiskGatedExecutor` trait `cancel_order` delegates without OTR recording
  (trait surface carries no `algo_id`); use the inherent
  `cancel_order(order_id, algo_id)` when OTR recording is required.

## Immediate Next Step

- Cross-repo doc cleanup: `tantra-docs` still describes exec's journal as
  "SQLite WAL" (architecture/adapter_interface/overview/event_loop/tradebook).
- `tantra-adapters` plan 0003 (cross-repo integration) will wire the Python
  adapters ↔ exec/ledger once approved.

## Operating Invariants

- Canonical profile `prod`: `cargo build/check/test --profile prod`.
- Tests live in `tests/` parallel to `src/`; no in-file `#[test]` blocks.
- Zero floats in financial paths; `rust_decimal::Decimal` only.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace --profile prod` green.
