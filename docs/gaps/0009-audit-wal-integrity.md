# 0009 — Audit WAL: hash does not bind order fields, torn tail blocks reopen, not wired into any runtime

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: design-flaw · **Severity**: high
**Plan**: [0004](../plans/0004-design-review-2026-11.md)

## What's stubbed or hardcoded

- `crates/tantra-exec-audit/src/entry.rs:198-205` `chain_link()` hashes only `prev_hash`, timestamp, `client_order_id` and `raw_payload`. `seq`, `algo_id`, `client_pan`, `side`, `quantity`, `price`, `event` and `exchange_order_id` (struct fields at `crates/tantra-exec-audit/src/journal.rs:164-180`) are not covered, so they can be edited without breaking `verify()`. The doc in `crates/tantra-exec-audit/src/hash_chain.rs` (line 3) states `H_n = SHA-256(H_{n-1} | Timestamp | ClientOrdId | Payload)`.
- `crates/tantra-exec-audit/src/journal.rs:96-107` `open()` parses every line with `?`. A torn final line after a crash (expected by the module doc, `journal.rs:3-10`) makes `AuditJournal::open` fail permanently, with no truncate/repair path. `open()` also does not verify the chain and reads all segments (O(total) startup).
- `journal.rs:148` `append` is synchronous: `state.lock().unwrap()` plus `sync_data()` per record (`journal.rs:229`). The parent directory is never fsynced after `create_new` of a segment (`journal.rs:211-217`). Called from async code this blocks a Tokio worker (`.agents/rules/scalability-and-concurrency.md` section 2.1).
- `journal.rs:372-384`: no exclusive lock file, so two processes (N replicas) can open the directory and fork the chain; `create_new` only catches same-seq collisions.
- The journal is declared in `tantra-runtime/Cargo.toml:58` but nothing in `tantra-runtime/crates`, `tantra-ffi` or `tantra-agent` uses `tantra_exec_audit` / `AuditJournal` outside tests: live orders are not audited.
- `crates/tantra-exec-audit/src/entry.rs:124` stores `client_pan` in plaintext in the WAL and in Parquet (`crates/tantra-exec-audit/src/exporter.rs:57`).

## Why

The SEBI audit trail can be rewritten undetected (change quantity, price or side), a crash can leave a journal that cannot restart, and live orders are not audited at all.

## How to close it

1. Digest a canonical serialization of all record fields (`seq`, `algo_id`, side, quantity, price, event, ...) under a new `chain_version` field; keep a v1 verifier for existing segments.
2. `open()` tail repair: truncate a partial last line (and log it), verify the chain of the last segment only.
3. Advisory lock file per directory (`fs2` / `fcntl`); fail open on a second process.
4. Async `AuditWriter` facade: bounded mpsc plus group-commit fsync on a dedicated thread; fsync the directory on segment rotation.
5. Hash or tokenize `client_pan`; add a retention/rotation policy (8-year SEBI) via `compact_parquet` and a sealed manifest.
6. Wire the writer into the runtime execution path (consumer side tracked in `tantra-runtime/docs/gaps/0033-audit-ledger-unification.md`).

**Verification**: in `tests/`, golden chain vectors (v1 and v2); a proptest that tampering any field fails `verify()`; a crash simulation truncating the tail; a concurrent-open lock test. Gates as in plan 0004.

## Related

- Root gap 0023 (tamper-evident audit trail unification); `tantra-runtime/docs/gaps/0033-audit-ledger-unification.md`.
- Closed gap 0002 only settled the storage format (JSONL segment WAL).
