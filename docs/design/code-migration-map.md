# `tantra-exec` — Historical Migration Map (honba-te → tantra-exec)

> **Tier**: Tier 2B (depends on `tantra-core` and `tantra-risk`)
> **Source**: `honba-workspace/honba-te`
> **Target**: `tantra-exec`
> **Status**: Migration complete for 7 of 8 crates. `tantra-exec-angelone` is
> a scaffold stub and remains open (see `docs/gaps/0001-angelone-adapter.md`).

## Sub-Crate Mapping Table

| Source Crate (`honba-te/crates/`) | Target Crate (`tantra-exec/crates/`) | Action | Status |
| :--- | :--- | :--- | :--- |
| `honba-te-core` | `tantra-exec-core` | Port `BrokerExecutor`, `ExecutionStream`, rate limiter, and tests. | Complete |
| `honba-te-auth` | `tantra-exec-auth` | Port TOTP, session manager, eDIS, and tests. | Complete |
| `honba-te-slicer` | `tantra-exec-slicer` | Port freeze quantity slicer, Iceberg/TWAP/VWAP slicers, and tests. | Complete |
| `honba-te-router` | `tantra-exec-router` | Port `SmartOrderRouter` wired to `tantra_risk::RiskEngine`. | Complete |
| `honba-te-audit` | `tantra-exec-audit` | Port the SHA-256 hash-chained audit journal and tests (JSONL segment WAL — gap `0002` closed; the runtime's `SqliteAuditLedger` is the complementary operational store). | Complete |
| `honba-te-zerodha` | `tantra-exec-zerodha` | Port Zerodha Kite v3 executor and webhook postback handler. | Complete |
| `honba-te-upstox` | `tantra-exec-upstox` | Port Upstox v2 executor. | Complete |
| `honba-te-angelone` | `tantra-exec-angelone` | Port Angel One SmartAPI 2.0 executor. | **Stub** — placeholder only (`docs/gaps/0001-angelone-adapter.md`) |

## Mandatory DDD Fixes Applied During Migration

1. `[workspace.dependencies]` consolidated in `tantra-exec/Cargo.toml` with
   canonical Tier 0/1A pins (`tantra-core`, `tantra-risk`) and the ported
   external manifest (`tokio`, `serde`, `serde_json`, `thiserror`, `tracing`,
   `rust_decimal`, `async-trait`, `futures`, `chrono`, `reqwest`, `uuid`,
   `proptest`, `axum`, `clap`, `sha2`, `hex`, `parquet`/`arrow`,
   `prometheus`, `opentelemetry`, `schemars`, `tokio-tungstenite`).
2. `honba_core` → `tantra_core`, `honba_mon` → `tantra_risk`,
   `honba_te_*` → `tantra_exec_*`; wire-token mapping normalized onto
   `BrokerEnumMap` tables (`2675c07`).
3. Inline `#[cfg(test)]` modules extracted from `crates/*/src/*.rs` into
   `crates/*/tests/`.
4. Crypto exchange execution (plan 0001 Phase 2) resolved **NO-GO** (`d529bc9`).

## Verification Commands

Run from the repository root:

```bash
grep -rn "#\[cfg(test)\]" crates/*/src/ && exit 1 || true
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace --profile prod
```

## Migration Milestone Status

| Milestone | Status |
|-----------|--------|
| `tantra-exec-core` (ports + rate limiter) | Complete |
| `tantra-exec-auth` (TOTP + session + eDIS) | Complete |
| `tantra-exec-slicer` (freeze/Iceberg/TWAP) | Complete |
| `tantra-exec-router` (SOR + RiskEngine gate) | Complete |
| `tantra-exec-audit` (JSONL WAL hash chain) | Complete |
| `tantra-exec-zerodha` adapter | Complete |
| `tantra-exec-upstox` adapter | Complete |
| `tantra-exec-angelone` adapter | **Open** — `docs/gaps/0001-angelone-adapter.md` |
| Plan 0001 wire-mapping verified (`2675c07`) | Complete |
| Plan 0001 crypto resolved NO-GO (`d529bc9`) | Complete |
| Zero inline `#[cfg(test)]` in `src/` | Complete |
| `cargo fmt --check` | Clean |
| `cargo clippy --all-targets -- -D warnings` | Clean |
| `cargo test --workspace --profile prod` | Passing |
