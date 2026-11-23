# Gaps Index — `tantra-exec`

Per workspace rule `.agents/rules/gap-tracking.md`:
- **Gaps** (`docs/gaps/XXXX-*.md`) are individual missing features, endpoints, or stubs.
- **Plans** (`docs/plans/XXXX-*.md`) are collections of gaps or multi-phase architectural features.
- When a gap is implemented and verified, remove the document from this
  directory and record it in the README "Closed" table below.

## Active Gaps

| # | Title | Scope | Status |
|---|-------|-------|--------|
| [0004](0004-idempotent-submit-and-outcome-unknown.md) | `client_order_id` never reaches the broker; no idempotent submit or unknown-outcome handling | critical — design review 2026-11 | open |
| [0005](0005-durable-order-id-map-and-reconcile.md) | Broker-order-id map in memory only; no reconcile on reconnect | high — design review 2026-11 | open |
| [0006](0006-slicer-durable-state-and-clock.md) | TWAP slicer has no durable parent state and reads the wall clock | high — design review 2026-11 | open |
| [0007](0007-secret-types-redaction-zeroize.md) | Secret types derive `Debug`/`Serialize`, lack `Zeroize` | high — design review 2026-11 | open |
| [0008](0008-session-token-lifecycle.md) | Executors hold a static access token; no refresh or daily expiry handling | high — design review 2026-11 | open |
| [0009](0009-audit-wal-integrity.md) | Audit WAL hash does not bind order fields; torn tail blocks reopen; not wired into runtime | high — design review 2026-11 | open |
| [0010](0010-rate-limiter-design.md) | Rate limiter: one bucket per broker, per process, hard-wired clock | medium — design review 2026-11 | open |
| [0011](0011-order-normalisation-tick-lot-validity.md) | Order mapping: no tick/lot rounding, GTC mis-mapped, algo id unvalidated | medium — design review 2026-11 | open |
| [0012](0012-credential-sourcing-and-legacy-env.md) | Credential store: legacy `HONBA_*` env, no vault/file backend, in-memory sessions | medium — design review 2026-11 | open |

## Closed Gaps

| # | Title | Closed In |
|---|-------|-----------|
| 0003 | No bracket-order (SL/target/trailing-stop) execution contract — EE+ needs `submit_bracket`/`modify_stop` | Closed 2026-11-17 (T12.5, `292185b`): `crates/tantra-exec-core/src/bracket.rs` — `BracketOrderSpec`/`BracketTicket`/`StopUpdate`/`BracketCapabilities` (`native_bo`/`emulated_multileg`/`atomic_modify` matrix) + pure `BracketTracker` (entry fill, never-widen `modify_stop`, FIFO partial-exit, stop-trigger close with exact realized PnL, deterministic `{bracket}#stop` child-order ids); `BrokerExecutor::submit_bracket`/`modify_stop`/`bracket_capabilities` (non-breaking defaults) + `MockBrokerExecutor` impl. Tests `bracket_tests` (10) + `bracket_contract_tests` (5) + `bracket_proptest` (3). Follow-on: runtime synthetic-OCO wiring + real broker BO/CO adapters. |
| 0001 | Angel One SmartAPI 2.0 execution adapter | Plan 0002 Phase 1 — `feat(angelone): SmartAPI 2.0 BrokerExecutor adapter with order stream and exact-decimal wire payloads` (`2258bbb`): REST executor (place/modify/cancel/order book/positions/margins/health), SmartStream order feed → `ExecutionStream`, `BrokerEnumMap` wire tables, error mapping, `rate_limited()` (10/s preset), `SessionTokens` expiry wiring, exact-decimal payloads. 40 tests green, clippy `-D warnings` clean. `2026-11-13` |
| 0002 | Audit journal storage reconciliation (SQLite WAL naming vs. JSONL segment WAL) | Closed — JSONL segment WAL is canonical for `tantra-exec` (tamper-evident journal + SEBI Annexure export); the runtime's `SqliteAuditLedger` is the complementary operational SQLite store. See `docs/design/code-audit.md` |
