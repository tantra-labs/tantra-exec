# 0007 — Secret types derive `Debug`/`Serialize`, lack `Zeroize`, and use plain `String`

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: design-flaw · **Severity**: high
**Plan**: [0004](../plans/0004-design-review-2026-11.md)

## What's stubbed or hardcoded

- `crates/tantra-exec-auth/src/credentials.rs:56` `#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)] pub struct BrokerCredentials` holds `api_secret`, `password`, `totp_seed`, `tpin` as `String`.
- `crates/tantra-exec-auth/src/tokens.rs:17-18` `#[derive(Debug, Clone, PartialEq, Eq)] pub struct SessionTokens`: the access token is printable via `{:?}`.
- `crates/tantra-exec-auth/src/totp.rs:55` `Totp` derives `Debug` with a raw `key: Vec<u8>`.
- No `zeroize` dependency in any `Cargo.toml` in the workspace.
- `crates/tantra-exec-zerodha/src/lib.rs:53` and `crates/tantra-exec-upstox/src/lib.rs:46` expose `pub access_token: String` on the executors; `crates/tantra-exec-upstox/src/ws.rs:324` repeats it on the connect config. The Kite WS URL also carries `access_token=` in the query string.

## Why

Any `tracing::debug!(?creds)` or panic dump leaks the TOTP seed, which is full account takeover (orders can be placed). `.agents/rules/security-and-secrets.md` section 3.3 requires a manually redacted `Debug` and `ZeroizeOnDrop` on these structs. This is the exec-side part of root gap 0011.

## How to close it

1. Introduce a `SecretString` / `SecretBytes` newtype in `tantra-exec-auth`: redacting `Debug` (`[REDACTED]`), no `Serialize` by default, `ZeroizeOnDrop`.
2. Use it in `BrokerCredentials`, `SessionTokens`, `Totp`, and the executor / WS connect configs; expose the value only through an explicit `expose_secret()` accessor.
3. Replace the derived `Debug` on every struct above with the redacted form.
4. Keep the Kite query-string token out of logs and span fields (redact the URL before it reaches `tracing`).

**Verification**: golden tests in `tests/` asserting `format!("{:?}", ..)` contains no secret for each type; `static_assertions` that the secret types are not `Serialize`; a test that the redacted URL helper strips `access_token`. Gates as in plan 0004.

## Related

- Root gap 0011 (credential and token leaks across Rust and Python).
- [0008](0008-session-token-lifecycle.md) (token provider will hold the new secret types), [0012](0012-credential-sourcing-and-legacy-env.md).
