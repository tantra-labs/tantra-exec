# 0012 — Credential store: legacy `HONBA_*` env names, no vault or file backend, in-memory sessions only

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: deployment · **Severity**: medium
**Plan**: [0004](../plans/0004-design-review-2026-11.md)

## What's stubbed or hardcoded

- `crates/tantra-exec-auth/src/credentials.rs:5,34-36,106-109,144,151`: the env prefix is `HONBA_{BROKER}_*`, which violates the naming rule in `.agents/rules/coding-and-ddd.md` section 3. `tests/credentials_tests.rs:69` pins it.
- Stores available: env, memory, and `keyring` (feature-gated, `crates/tantra-exec-auth/src/credentials.rs:193-`, desktop Secret Service only). There is no Vault, cloud secret-manager or mounted-secret-file implementation; `save()` on the env store errors (read-only).
- Session tokens are cached only in memory (`crates/tantra-exec-auth/src/tokens.rs:50-60`), so every pod restart performs a fresh TOTP login. Brokers throttle repeated logins, and a new login invalidates the previous session (one session per API key).

## Why

Containers can supply secrets only through env vars (visible via `/proc` and `docker inspect`). Restart loops trigger repeated headless logins and lock-outs. This is the exec-side part of root gap 0011.

## How to close it

1. Read `TANTRA_{BROKER}_*` first, keep `HONBA_*` as a deprecated alias that logs a warning.
2. Add a `FileCredentialStore` (k8s mounted secret, `_FILE` suffix convention) and a Vault-backed `CredentialStore` implementation.
3. Add a `SessionStore` port for encrypted session-token persistence shared across restarts, consumed by the `TokenProvider` from [0008](0008-session-token-lifecycle.md).
4. Update `tests/credentials_tests.rs` to cover both prefixes and the deprecation warning.

**Verification**: tempdir-based tests for the file store (including `_FILE` precedence); alias-precedence and warning tests; encrypted session round trip. Gates as in plan 0004.

## Related

- Root gap 0011; [0007](0007-secret-types-redaction-zeroize.md), [0008](0008-session-token-lifecycle.md).
- Root plan 0022 (secrets in deployment); plan 0004 R4 (gateway singleton).
