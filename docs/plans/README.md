# Plans Index — `tantra-exec`

Per workspace rule `.agents/rules/gap-tracking.md`:
- **Gaps** are individual features (`docs/gaps/XXXX-*.md`).
- **Plans** are collections of gaps or multi-phase architectural features
  (`docs/plans/XXXX-*.md`).
- When implemented, plan documents are cleaned up from `docs/plans/` and
  recorded in this index.

## Active Plans

| # | Title | Target Scope | Status |
|---|-------|--------------|--------|
| [0004](0004-design-review-2026-11.md) | tantra-exec design review (2026-11): idempotent submission, durable order state, secrets & audit integrity (gaps 0004–0012, R1–R6) | `tantra-exec-*` | Proposed |

## Implemented Plans

| # | Title | Target Scope | Implemented In |
|---|-------|--------------|----------------|
| 0001 | Crypto Execution Adapters & Broker Wire-Format Mapping Verification | `tantra-exec-core`, `tantra-exec-zerodha`, `tantra-exec-upstox` | Phase 1 wire-mapping verified in `2675c07`; Phase 2 crypto resolved NO-GO in `d529bc9` |
| 0002 | Milestone 3 Adapter Completion (Angel One adapter, audit storage reconciliation) | `tantra-exec-angelone`, `tantra-exec-audit` | Phase 2 (gap 0002) closed earlier; Phase 1 (gap 0001, Angel One SmartAPI 2.0 adapter) + Phase 3 (verification + indexes) landed `2026-11-13` (`2258bbb`). All 3 phases complete. |
| 0003 | Paper Exchange Matching Core — promote the backtest L2 book into `tantra-exec-paper`: price-time priority, order lifecycle (partial/stop/IOC/GTD), account isolation, `PaperExchangeExecutor` on `BrokerExecutor` + lifted sync port (closes runtime gap 0008 option 2) | new `tantra-exec-paper` crate + `tantra-exec-core` | **P0 implemented (`2026-11-16`)**: E1–E8 green (18 tests), E8 wasm check passes; T14.4 Wave 2 lifted the sync port (`SimExecutionPort`) into `tantra-exec-core` and closed runtime gap 0008; E9 cross-repo parity landed in `tantra-runtime-backtest/tests/paper_parity_tests.rs` (4 tests, byte-identical fills); T14.14 canonical golden vector + parity test landed (`tests/golden_parity_tests.rs`, byte-identical across backtest / paper / Python wheel / WASM). Master plan root `docs/plans/0007` (implemented — see the root plans index). |
