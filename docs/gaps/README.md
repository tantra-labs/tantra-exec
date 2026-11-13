# Gaps Index — `tantra-exec`

Per workspace rule `.agents/rules/gap-tracking.md`:
- **Gaps** (`docs/gaps/XXXX-*.md`) are individual missing features, endpoints, or stubs.
- **Plans** (`docs/plans/XXXX-*.md`) are collections of gaps or multi-phase architectural features.
- When a gap is implemented and verified, remove the document from this
  directory and record it in the README "Closed" table below.

## Active Gaps

| # | Title | Scope | Status |
|---|-------|-------|--------|
| [0003](0003-bracket-order-slt-sl-execution.md) | No bracket-order (SL/target/trailing-stop) execution contract — EE+ needs `submit_bracket`/`modify_stop` | `tantra-exec-core`, `BrokerExecutor` adapters | Open — tracked by root plan 0005 |

## Closed Gaps

| # | Title | Closed In |
|---|-------|-----------|
| 0001 | Angel One SmartAPI 2.0 execution adapter | Plan 0002 Phase 1 — `feat(angelone): SmartAPI 2.0 BrokerExecutor adapter with order stream and exact-decimal wire payloads` (`2258bbb`): REST executor (place/modify/cancel/order book/positions/margins/health), SmartStream order feed → `ExecutionStream`, `BrokerEnumMap` wire tables, error mapping, `rate_limited()` (10/s preset), `SessionTokens` expiry wiring, exact-decimal payloads. 40 tests green, clippy `-D warnings` clean. `2026-11-13` |
| 0002 | Audit journal storage reconciliation (SQLite WAL naming vs. JSONL segment WAL) | Closed — JSONL segment WAL is canonical for `tantra-exec` (tamper-evident journal + SEBI Annexure export); the runtime's `SqliteAuditLedger` is the complementary operational SQLite store. See `docs/design/code-audit.md` |
