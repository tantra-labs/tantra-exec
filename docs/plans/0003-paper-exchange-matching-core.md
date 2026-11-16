# 0003 — Paper Exchange Matching Core (`tantra-exec-paper`)

**Status**: P0 implemented (2026-11-16); T14.1–T14.3 + T14.4 done; Wave 2 closed runtime gap 0008 (sync port lifted).
E1–E8 green (18 tests in `crates/tantra-exec-paper/tests/`, `wasm32-unknown-unknown` check passes).
E9 (backtest/paper parity, cross-repo) landed in `tantra-runtime-backtest/tests/paper_parity_tests.rs` (4 tests, byte-identical fills on a golden tape).
T14.14 (root plan 0007 P4): canonical golden vector + parity test landed in `crates/tantra-exec-paper/tests/golden/` + `tests/golden_parity_tests.rs` (byte-identical across backtest / paper / Python wheel / WASM).
**Track**: T14 (P0) · **Phase**: 3 (execution)
**Owning repo**: `tantra-exec` · **Master plan**: root `docs/plans/0007-*.md`

## Why

The best matching engine Tantra has is `tantra_runtime_backtest::sim::l2_book::OrderBook`
(price-time priority, market + limit execution, participation caps, latency, worst-price
slippage) — but it lives inside the *backtester* and has no order lifecycle, no account
isolation, and no event stream. Meanwhile the paper paths (`PaperAdapter`, `NextOpenSim`) fill
at a single price with no book. The Execution bounded context is the natural home for the
exchange: `tantra_exec_core::BrokerExecutor`'s docs already name "Paper/Sim" as a first-class
implementor, and runtime gap `0008`'s close event is "lift the sync `BrokerAdapter` port into
`tantra-exec-core`" (option 2).

This plan promotes the existing L2 book into a real, event-driven, real-time-capable matching
engine in `tantra-exec`, and makes it the deterministic venue every other consumer drives.

## Deliverables (in `tantra-exec`)

New crate **`tantra-exec-paper`** (pure matching engine; zero-I/O domain, Decimal-only,
`wasm32`-targetable). It owns:

1. **Per-instrument `OrderBook`** — migrated from `tantra-runtime-backtest::sim::l2_book`
   (keep behavior byte-identical during extraction; the backtest crate re-exports or the
   runtime plan re-wires it — see `tantra-runtime/docs/plans/0005`).
2. **Order lifecycle** on the book:
   - states `Working` / `PartialFill` / `Filled` / `Cancelled` / `Rejected` /
     `StopTriggered` (stop orders become working after a stop-price breach);
   - `submit_order` / `cancel_order` / `amend_order` (price/qty), both `OrderSide`s,
     `TimeInForce` `Day` / `IOC` / `GTD`, `OrderType` `Market` / `Limit` / `StopLimit` /
     `StopMarket`;
   - resting depth: a working limit order sits on the book and can be matched by a later
     contra order, not only by feed depth.
3. **Event stream** — every state transition emits an immutable event (`OrderAccepted`,
   `Fill(partial|full)`, `Cancelled`, `Rejected`, `StopTriggered`, `DepthChanged`) with the
   caller-supplied timestamp; no wall clock.
4. **Account isolation** — per-`AccountId` cash, positions, and orders; fills flow into
   `tantra-market-finance::india` statutory fees and `tantra-ledger-portfolio` FIFO settlement
   (the same aggregates the backtester and REST already use).
5. **Market microstructure reuse** — port `sim/{latency,slippage,mis}` (seeded latency
   profiles, adverse-fill slippage, MIS 15:20 square-off) into the engine's input processing.

## Execution ports (two surfaces, one engine)

- **Async**: `PaperExchangeExecutor` implements `tantra_exec_core::BrokerExecutor`
  (`place_order` / `cancel_order` / `modify_order` / `fetch_order_book` / `fetch_positions` /
  `fetch_margins` / `health_check` + push via `ExecutionStream`).
- **Sync deterministic**: `PaperExchangeExecutor` implements
  `tantra_exec_core::sim::SimExecutionPort` (sync, via exec-core — T14.4, done):
  the `now`-stamped `submit_order(&order, now)` / `cancel_order(&id, now)` port
  lifted from `tantra-runtime-strategy::dispatch` into `tantra-exec-core`.
  This executed runtime gap 0008 option 2 — **closed**; the runtime's
  `BrokerAdapter` is now a thin re-export of `SimExecutionPort`.
- Both surfaces drive the same `PaperExchange` engine with explicit `now`
  timestamps (zero wall clock in the sim path) — research/live parity preserved.

## TDD / test slices (all in `tests/`, parallel to `src/`)

| # | Red test (failing first) | Green deliverable |
|---|--------------------------|-------------------|
| E1 | Two working buy limits at prices P1>P2 + a sell market: fills execute at P1 (price priority), remaining at P2 | Price-time priority across resting orders |
| E2 | Sell 100 against resting bids 40@100 + 60@99: partial fill 40@100 then 60@99, VWAP fill, remaining 0 | Multi-level partial fills + VWAP |
| E3 | IOC buy 200 with only 150 resting: fills 150, cancels the rest | IOC semantics |
| E4 | GTD order expires at its end-time without fill | GTD expiry |
| E5 | StopLimit buy: no fill while last < stop; fills once last ≥ stop | Stop order trigger |
| E6 | Cancel a working order → `Cancelled` event, depth restored | Cancel + amend |
| E7 | Replay the same order+tape sequence twice → byte-identical fills/events | Determinism (golden vector) |
| E8 | `wasm32-unknown-unknown` check passes for the crate | WASM targetable |
| E9 | `test_identical_fills_paper_vs_backtest` (with `tantra-runtime-backtest`) — same tape, same fills | Backtest/paper parity (cross-repo gate) |

## Verification

```bash
cd tantra-exec && cargo check --workspace --profile prod
cargo test -p tantra-exec-paper --profile prod
cargo clippy --all-targets -- -D warnings && cargo fmt --check
# E8: cargo check -p tantra-exec-paper --target wasm32-unknown-unknown --profile prod
```

## Cross-repo dependencies

- Depends on `tantra-runtime-backtest::sim` extraction source being available (read-only
  migration, then re-wired by `tantra-runtime/docs/plans/0005`).
- Fills depend on `tantra-market-finance` (costs) and `tantra-ledger-portfolio`
  (FIFO/settlement) — already workspace deps of the runtime; add as deps of this crate.
- Closes `tantra-runtime/docs/gaps/0008` when the sync port is lifted.
