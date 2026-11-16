# HANDOFF — tantra-exec

**Status**: 9 crates green. `tantra-exec-paper` (plan 0003) P0 implemented —
E1–E8 verified + T14.4 sync port lift (18 paper tests + 40 core tests,
`wasm32-unknown-unknown` check passes, clippy `-D warnings` clean). Angel One
adapter (gap 0001, plan 0002) remains closed/complete from the prior batch.
**Date**: 2026-11-16 · Repo: `tantra-exec` · Branch: `main`

## Latest Batch — T14 Wave 2 (sync port lift)

Date: 2026-11-16. Plan 0003 deliverable 6 / runtime gap 0008 option 2 landed:
`SimExecutionPort` added to `tantra-exec-core` (new `src/sim.rs` + lib
re-export, `pub mod sim` + `pub use sim::SimExecutionPort`); the sync trait
mirrors the runtime `BrokerAdapter` shape verbatim (`submit_order(&order, now)`
/ `cancel_order(&id, now)` / `broker_name`, `Arc<T>` blanket impl, no wall
clock). `PaperExchangeExecutor` implements it (delegates to
`submit_order_sync` / `cancel_order_sync`). New `tests/sim_port_tests.rs` (+2
tests: market-buy fill + positions, cancel-once-then-error). Runtime
`dispatch.rs::BrokerAdapter` is now a thin re-export; gap 0008 closed.
Verification green: `cargo check --workspace --profile prod`, `cargo test
-p tantra-exec-core -p tantra-exec-paper --profile prod`, clippy `-D warnings`,
wasm check, `make prod`.

## Latest Batch — T14 P0 IMPLEMENTED (plan 0003)

Date: 2026-11-16. Plan `docs/plans/0003-paper-exchange-matching-core.md`: new crate
`tantra-exec-paper` (pure matching engine) — ported `book` (L2 price-time priority,
market/limit, participation cap), `latency`, `slippage`, `mis` from
`tantra-runtime-backtest::sim` (verbatim; `OrderBook::ready_ts` made `pub` for the
`tantra-runtime/docs/plans/0005` parity surface) + new `lifecycle`, `paper`
(`PaperExchange` aggregate), `executor` (`PaperExchangeExecutor` implements
`BrokerExecutor`). Wired into the workspace (member + dep + facade re-export) and
`rust_decimal_macros` added to workspace deps. E-slices E1–E8 green.

## Phase / Slice Status (current batch — plan 0003 P0)

- `tantra-exec-paper` (E1–E8): price-time priority across resting participants;
  multi-level VWAP; IOC remainder cancel; GTD expiry (`submit_order_gtd` +
  `advance_to`); stop trigger (StopLoss→Market / StopLimit→Limit); cancel+amend;
  determinism golden replay (identical tape → byte-identical fills/events); wasm.
- Tests: `tests/{book_tests,paper_exchange_tests,executor_tests,sim_port_tests}.rs` — **18 tests
  green** (`cargo test -p tantra-exec-paper --profile prod`).
- Verification: `cargo check --workspace --profile prod`; `cargo clippy
  --all-targets -- -D warnings` (clean, incl. all other members); `cargo check
  -p tantra-exec-paper --target wasm32-unknown-unknown --profile prod` (E8);
  `make prod` refresh.
- Plan 0003 Implemented; E9 parity landed in
  `tantra-runtime-backtest/tests/paper_parity_tests.rs`; T14.14 golden vector
  + parity test landed in `crates/tantra-exec-paper/tests/golden/` +
  `tests/golden_parity_tests.rs` (index row: "P0 implemented").

## Decisions & Discoveries

- **WASM E8 required dependency surgery**: tokio supports only
  `sync,macros,io-util,rt,time` on `wasm32-unknown-unknown`, and `uuid` v4 /
  `rand_core`/`ahash` need `getrandom` `wasm_js`/`js` backends. `tantra-exec-core`
  now declares a direct trimmed tokio (workspace `tokio` keeps `net`/
  `rt-multi-thread` for the broker adapters; native builds unify features back);
  `tantra-exec-paper` adds wasm-target `getrandom` features.
- **Paper matching**: resting participants match directly (not via feed book);
  aggressor gets price improvement (fills at the resting price); partial-fill
  market orders rest at no price and re-match only against feed depth (degenerate
  for P0, untested).
- **Cash is aggregate per `AccountId`**: a matched buy+sell pair nets to zero
  cash (buy pays `notional+fee`, sell receives `notional-fee`) — documented in
  `cash_and_positions_accounting_on_cross`.
- `WorkingOrder` carries an extra `submitted_ts` field (spec shape +1) so
  `fetch_order_book` snapshots are deterministic (no `Utc::now()` in the paper
  path). `FillReport.algo_id` = `"paper"` (WorkingOrder does not store the request
  `algo_id`). E3 interpreted as resting contra *Sell* 150@100 + IOC Buy 200
  (the spec text's same-side "Buy 150@100" cannot fill a buy).
- Executor `BrokerExecutor` impl uses `std::sync::Mutex` (wasm-safe), not tokio's.

## Immediate Next Step

- Root plan 0007 P4 (T14.14): canonical golden vector landed at
  `crates/tantra-exec-paper/tests/golden/paper_golden.json` +
  `tests/golden_parity_tests.rs` — byte-identical fills across backtest
  (E9), this engine, the Python wheel, and WASM. Next: paper-trading user
  guide (T14.15, coordinated with `tantra-docs`).

## Operating Invariants

- Canonical profile `prod`: `cargo build/check/test --profile prod`; `make prod`.
- Tests live in `tests/` parallel to `src/`; no in-file `#[test]` blocks.
- Zero floats in financial paths; `rust_decimal::Decimal` only; no wall clock in
  the `PaperExchange` (all transitions take explicit `DateTime<Utc>`; the
  executor's `BrokerExecutor` wire port may read `Utc::now()`).
- WASM: `tantra-exec-paper` has no `tokio`/`std::fs`/OS threads in `[dependencies]`;
  keep tokio features on the wasm-safe set.
