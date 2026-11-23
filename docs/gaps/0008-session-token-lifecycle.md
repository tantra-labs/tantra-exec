# 0008 — Executors hold a static access token; no refresh path or daily expiry handling

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: design-flaw · **Severity**: high
**Plan**: [0004](../plans/0004-design-review-2026-11.md)

## What's stubbed or hardcoded

- `crates/tantra-exec-zerodha/src/lib.rs:53,92`: the token is fixed at construction and read by `auth_headers()`. `map_kite_error` (`crates/tantra-exec-zerodha/src/lib.rs:262-270`) turns 401 / `TokenException` into `BrokerError::Auth`, but nothing re-logs in.
- `crates/tantra-exec-auth/src/tokens.rs:224-280`: the scheduler refreshes at 08:30 IST into a `TokenCache`, but no adapter reads `TokenCache`. `tantra-runtime` depends on `tantra-exec-auth` (`tantra-runtime/Cargo.toml:55`) yet has no non-test use of `tantra_exec_auth` under `tantra-runtime/crates`.
- Kite sessions expire around 06:00 IST. The comment in `tokens.rs` references 03:30 for SmartAPI only; Zerodha and Upstox anchors are not encoded, and `SessionTokens::expires_at` is trusted from login.
- The Kite WS URL carries `access_token` in the query string (`crates/tantra-exec-zerodha/src/ticker.rs:33`), which can end up in proxy or tracing logs.
- The reconnecting ticker keeps retrying with the dead token, then gives up silently after 8 attempts.

## Why

After the daily roll-off every REST and WS call fails until a process restart. Order placement, cancels and the fill stream all go dark during market hours if the process outlives the session.

## How to close it

1. Define a `TokenProvider` port (`async fn current(&self) -> SessionTokens`, single-flight refresh on invalidate) injected into all three executors and all WS connectors.
2. Add a per-broker expiry-anchor table in `tantra-exec-auth` (Zerodha 06:00 IST, Upstox 03:30 IST, AngelOne 05:00 IST).
3. On a `BrokerError::Auth` response, force one refresh and retry the call once; surface a hard auth failure if the retry fails.
4. WS connectors fetch the token from the provider on every reconnect instead of capturing it at start.

**Verification**: in `tests/`, a fake-clock test of the expiry anchors; wiremock 403 then refresh then 200; a single-flight test with concurrent callers; a reconnect test that picks up a rotated token. Gates as in plan 0004.

## Related

- [0007](0007-secret-types-redaction-zeroize.md) (secret types), [0012](0012-credential-sourcing-and-legacy-env.md) (shared session store).
- Plan 0004 R4 (one session per API key implies a singleton login owner).
