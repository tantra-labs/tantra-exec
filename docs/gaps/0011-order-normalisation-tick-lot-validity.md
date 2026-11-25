# 0011 — Order mapping: no tick/lot rounding, GTC mis-mapped, algo id not validated

**Status**: open
**Filed**: 2026-11-23 — workspace design review (root plan 0021) · **Kind**: design-flaw · **Severity**: medium
**Plan**: [0004](../plans/0004-design-review-2026-11.md)

## What's stubbed or hardcoded

- `crates/tantra-exec-zerodha/src/lib.rs:158` `TimeInForce::Gtc => "TTL"`: Kite `TTL` requires `validity_ttl` minutes, which is never set.
- `crates/tantra-exec-upstox/src/lib.rs:129` `Gtc => "DAY"` silently downgrades a GTC order to a day order.
- Prices go through `format_fixed_exact(p.0, 2)` (`crates/tantra-exec-zerodha/src/lib.rs:140-141`, `crates/tantra-exec-core/src/decimal.rs`): it rejects more than 2 decimal places but never rounds to the instrument tick (0.05, 0.01, 0.0001 for currency) and never checks lot size. There are no `tick_size` / `lot_size` matches in adapter `src`.
- `req.algo_id` is inserted as `tag` without checks. Kite tags are max 20 alphanumeric characters. An empty algo id is silently omitted on AngelOne (`crates/tantra-exec-angelone/src/lib.rs:112`) while the other two send empty tags.
- SEBI tagging is therefore not enforced at the port: `RiskGatedExecutor` is optional wrapping (`crates/tantra-exec-core/src/executor.rs:211`).

## Why

Best case the broker rejects the order; worst case it carries an unexpected validity. Live orders can leave without the SEBI algo tag.

## How to close it

1. Add `InstrumentSpec { tick, lot }`, passed through `OrderRequest` or the port.
2. Introduce a `ValidatedOrder` newtype produced by a single validation function (tick, lot, algo id format, non-empty tag) and required by `BrokerExecutor::place_order`.
3. Adapters return `Err(InvalidRequest)` on an unsupported time-in-force instead of remapping it; implement Kite `TTL` only with a `validity_ttl`.
4. Reuse the shared `OrderTag` from [0004 (closed)](README.md#closed-gaps) for tag length and charset rules.

**Verification**: proptest on tick rounding and lot multiples; golden wire vectors per broker; negative tests for empty/oversized algo id and unsupported GTC. Gates as in plan 0004.

## Related

- [0004 (closed)](README.md#closed-gaps) (tag scheme).
- Plan 0004 R2 and R3 (duplicated wire mapping across adapters and Python).
