# 0004 — tantra-exec design review (2026-11): idempotent submission, durable order state, secrets & audit integrity

**Status**: Proposed (2026-11-23)
**Scope**: `tantra-exec` · **Umbrella**: root `docs/plans/0021-workspace-design-review-2026-11.md`

## 0. Executive Summary & Objective

The 2026-11 design review of `tantra-exec` found that the order path is not safe against ambiguous outcomes: the client order id never reaches the broker, timeouts are reported as plain transport errors, and the SOR fails over with the same request. Order correlation, slicer progress and session tokens live only in process memory, secrets are printable and not zeroized, and the audit WAL neither binds order fields into its hash nor is wired into any runtime.

This plan makes submission idempotent with an explicit unknown-outcome state, makes order and parent state durable and reconcilable, hardens secret handling and the token lifecycle, and brings the audit journal to a tamper-evident, restart-safe state. Non-goals: bounded channels in the WS code (root plan 0019 P1.5), crates not linked into the runtime (root plan 0019 section 4.1), and the runtime-side wiring, which is tracked in `tantra-runtime` gaps 0022, 0028 and 0033.

## 1. Context & Gaps Covered

| Gap | Title | Kind | Severity |
|---|---|---|---|
| [0004](../gaps/0004-idempotent-submit-and-outcome-unknown.md) | Idempotent submit and unknown-outcome handling | design-flaw | critical |
| [0005](../gaps/0005-durable-order-id-map-and-reconcile.md) | Durable order-id map and reconcile on reconnect | design-flaw | high |
| [0006](../gaps/0006-slicer-durable-state-and-clock.md) | Slicer durable parent state and injected clock | design-flaw | high |
| [0007](../gaps/0007-secret-types-redaction-zeroize.md) | Secret types: redaction and zeroize | design-flaw | high |
| [0008](../gaps/0008-session-token-lifecycle.md) | Session token lifecycle and refresh | design-flaw | high |
| [0009](../gaps/0009-audit-wal-integrity.md) | Audit WAL integrity | design-flaw | high |
| [0010](../gaps/0010-rate-limiter-design.md) | Rate limiter design | design-flaw | medium |
| [0011](../gaps/0011-order-normalisation-tick-lot-validity.md) | Order normalisation: tick, lot, TIF, algo id | design-flaw | medium |
| [0012](../gaps/0012-credential-sourcing-and-legacy-env.md) | Credential sourcing and legacy env names | deployment | medium |

Plan-only items (no standalone gap doc):

- R1 — Broker metrics exposure and tracing export — `crates/tantra-exec-core/src/telemetry.rs:18-100` registers `broker_http_request_duration_seconds` and `rate_limit_*` on the default `prometheus` registry; `tantra-runtime/crates/tantra-runtime-actors/src/metrics.rs:11,269-299` serves `/metrics` from its own `Registry::gather()`, so broker metrics are invisible. No tracing subscriber or OTLP exporter is installed, and the module uses `.expect("register ...")` in a `Lazy`. Provider side: `tantra-runtime` gap 0022.
- R2 — Adapter kit for the three broker adapters — REST dispatch, span and metrics, error envelope, `rate_limited()`, client builder and the three WS reconnect loops are copied across `crates/tantra-exec-zerodha/src/lib.rs:176-227`, `crates/tantra-exec-upstox/src/lib.rs`, `crates/tantra-exec-angelone/src/lib.rs`, and `crates/tantra-exec-zerodha/src/ticker.rs:285-435`, `crates/tantra-exec-upstox/src/ws.rs:301-483`, `crates/tantra-exec-angelone/src/ws.rs:275-423`. Wire maps are per-crate `mapping.rs` plus `ProductType::from_zerodha/from_upstox` in `crates/tantra-exec-core/src/types.rs:75-86`.
- R3 — Duplicate Kite WS clients and Python/Rust adapter copies — `crates/tantra-exec-zerodha/src/ticker.rs` and `tantra-market/crates/tantra-market-zerodha/src/lib.rs` both connect to `wss://ws.kite.trade` with their own reconnect; `tantra-adapters/tantra_adapters/` and `tantra-ffi/python/tantra/adapters/` (`rate_limit.py`, `session.py`, `secrets.py`) re-implement Rust behaviour. Tracked at the workspace level in root gap 0017.
- R4 — Graceful drain, gateway singleton and static egress IP — no shutdown hook on `BrokerExecutor` or `Router` (`crates/tantra-exec-router/src/lib.rs`, `sor.rs`); WS read loops have no cancel tokens (`crates/tantra-exec-zerodha/src/ticker.rs:329-`); one session per API key forbids replica logins; `reqwest::Client` is built without a local address or proxy (`crates/tantra-exec-zerodha/src/lib.rs:84`, `crates/tantra-exec-upstox/src/lib.rs:77`); `tantra-runtime/crates/tantra-runtime-strategy/src/dispatch.rs:16,554` imports the concrete `ZerodhaExecutor` (provider: `tantra-runtime` gap 0028; root plan 0022).
- R5 — Typed `BrokerError` and capability discovery — `crates/tantra-exec-core/src/error.rs:8-35` carries `String` payloads; the SOR matches variant groups ad hoc (`sor.rs:326,439`); `crates/tantra-exec-core/src/executor.rs:134-158` has default impls instead of `Unsupported`; order ids are raw `&str` (`crates/tantra-exec-core/src/executor.rs:81`).
- R6 — Oversized modules — `crates/tantra-exec-paper/src/paper.rs` (1004 lines), `crates/tantra-exec-core/src/executor.rs` (742, with two forwarding impls at `crates/tantra-exec-core/src/executor.rs:260-400`), `crates/tantra-exec-upstox/src/lib.rs` (744), `crates/tantra-exec-auth/src/login.rs` (678), `crates/tantra-exec-core/src/ratelimit.rs` (681).

## 2. Component Specifications & Phases

### Phase A — Order-path safety and secrets (small, high-value)

1. Gap 0004: add `BrokerError::OutcomeUnknown`, the shared `OrderTag`, `find_by_client_order_id`, and make the SOR look up before any failover. Do not count unknown outcomes as health failures.
2. Gap 0007: introduce `SecretString` / `SecretBytes` in `tantra-exec-auth`; replace derived `Debug` on `BrokerCredentials`, `SessionTokens` and `Totp`; redact the Kite query-string token in logs.
3. Gap 0011: add `ValidatedOrder` and `InstrumentSpec`; reject unsupported TIF and invalid algo ids at the port.
4. Gap 0010 (first half): inject a `Clock`, add `RateClass`, cap `max_wait`, error on unknown broker.

**Gate**: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `make check`, `cargo test -p tantra-exec-core --profile prod`, `cargo test -p tantra-exec-router --profile prod`, `cargo test -p tantra-exec-auth --profile prod`, `make prod`.

### Phase B — Durable state, tokens and audit

1. Gap 0005: `OrderCorrelationStore` port, `ReconcilingStream`, ownership eviction and rehydrate.
2. Gap 0006: `ParentOrderState` port, deterministic leg ids, slicer `Clock`.
3. Gap 0008: `TokenProvider` port, per-broker expiry anchors, single refresh-and-retry on `Auth`.
4. Gap 0009: audit chain v2 covering all fields, tail repair, lock file, async `AuditWriter`, PAN tokenization, retention.
5. Gap 0012: `TANTRA_` prefix with `HONBA_` alias, `FileCredentialStore`, `SessionStore`.
6. R5: `ErrorClass`, structured `Rejected`, `BrokerCapabilities`, `BrokerOrderId` (builds on gap 0004).

**Gate**: Phase A gate plus `cargo test -p tantra-exec-audit --profile prod`, `cargo test -p tantra-exec-slicer --profile prod`, crash-simulation and wiremock suites.

### Phase C — Deduplication and deployment readiness

1. R2 then R3: extract the adapter kit (`BrokerHttp`, generic reconnecting WS, `WireMap`), then share the Kite wire connect with `tantra-market` and decide Rust as the source of truth for the Python adapters.
2. R4: `BrokerExecutor::shutdown(DrainPolicy)`, `CancellationToken` in WS loops, `HttpClientConfig { local_address, proxy, timeout }`, gateway singleton with lease.
3. Gap 0010 (second half): `RateLimitStore` port with a shared implementation.
4. R1: own `Registry` in `telemetry`, order-outcome / reconnect / session-age counters, `BrokerHealthProbe` for readiness.
5. R6: split `executor.rs` into `port.rs`, `risk_gated.rs`, `mock.rs`; deref-forwarding macro; split `paper.rs`.

**Gate**: Phase B gate plus `cargo test --workspace --profile prod` (if the LTO link is OOM-killed, run `make check` plus targeted test binaries and record the exact command), `make prod`.

## 3. Relationship to Other Plans

- Root plan 0019: P1.5 (bounded channels in WS code) and section 4.1 (unlinked crates) are not duplicated here; the Phase C adapter-kit work should land after P1.5 to avoid conflicts in the same WS files. P3.1 may list some of R6; check before starting it.
- Root gaps: 0010 (idempotency), 0011 (secrets), 0017 (broker integration consolidation), 0023 (audit unification).
- Root plans: 0022 (HA, secrets, observability, egress IP), 0023 (CI feeds the gates above).
- Provider-side work in `tantra-runtime`: gaps 0022 (metrics and readiness), 0028 (concrete executor in strategy dispatch), 0033 (audit ledger unification).

## 4. Exit Criteria

1. A timed-out submit never triggers failover or resubmit without a successful `find_by_client_order_id` lookup; tests cover the drop-connection case.
2. `client_order_id` is recoverable from the wire tag for all three brokers (round-trip proptest).
3. No secret type in `tantra-exec-auth` or the executors prints its value via `Debug` or implements `Serialize` by default; all are zeroized on drop.
4. After a simulated restart the router can cancel a live order and fills are attributed to the correct client id; missed fills are replayed on reconnect.
5. A TWAP resumed after a simulated crash sends no duplicate legs and never exceeds the parent quantity.
6. Tampering with any audit record field fails `verify()`; a torn tail no longer blocks `open()`; a second opener is refused.
7. Executors survive the daily session roll-off without a process restart.
8. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `make check`, `make test` (or the recorded OOM fallback) and `make prod` pass.
