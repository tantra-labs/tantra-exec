//! Sync `SimExecutionPort` (tantra-exec-core) tests for the paper exchange.
//!
//! These tests drive `PaperExchangeExecutor` exclusively through the
//! deterministic sync port: explicit timestamps, no wall clock, no async
//! (T14.4, root plan 0007 / gap 0008 — the sync port was lifted from the
//! runtime into `tantra-exec-core`).

use chrono::Utc;
use rust_decimal_macros::dec;
use tantra_core::{
    Exchange, InstrumentId, OrderRequest, OrderSide, OrderType, Price, Quantity, Tick,
};
use tantra_exec_core::SimExecutionPort;
use tantra_exec_paper::{PaperExchange, PaperExchangeExecutor};
use uuid::Uuid;

fn inst() -> InstrumentId {
    InstrumentId::new(Exchange::Nse, "RELIANCE")
}

fn order(side: OrderSide, otype: OrderType, qty: u64, n: u128) -> OrderRequest {
    OrderRequest::with_id(
        Uuid::from_u128(n),
        inst(),
        side,
        otype,
        Quantity::from_u64(qty),
        "test",
    )
}

/// Seed a resting book (real bid/ask depth) via a Quote tick so a market
/// order fills; also pins the market reference price.
fn seeded_exchange() -> PaperExchange {
    let mut ex = PaperExchange::new(dec!(1_000_000));
    let t = Utc::now();
    ex.feed_tick(&Tick::Quote {
        instrument: inst(),
        bid: Price::new(dec!(100)),
        ask: Price::new(dec!(105)),
        bid_qty: Quantity::from_u64(50),
        ask_qty: Quantity::from_u64(50),
        last_price: Some(Price::new(dec!(100))),
        exchange_ts: t,
        receive_ts: t,
        band: None,
    });
    ex.set_market_price(inst(), Price::new(dec!(100)));
    ex
}

#[test]
fn sync_market_buy_returns_paper_id_and_reflects_in_positions() {
    let exec = PaperExchangeExecutor::new(seeded_exchange());
    let now = Utc::now();

    // Market buy 50 against the seeded ask depth (50 @ 105) → fills.
    let req = order(OrderSide::Buy, OrderType::Market, 50, 7);
    let order_id = exec.submit_order(&req, now).unwrap();
    assert!(
        order_id.starts_with("PAPER-"),
        "expected a PAPER-* order id, got {order_id}"
    );

    // Positions reflect the buy (sync surface: paper exchange position map).
    let exchange = exec.exchange();
    let guard = exchange.lock().unwrap();
    let qty = guard.positions().get(&inst()).copied().unwrap_or_default();
    assert_eq!(qty, dec!(50), "long position of 50 expected");
}

#[test]
fn sync_cancel_resting_limit_then_second_cancel_errors() {
    let exec = PaperExchangeExecutor::new(PaperExchange::new(dec!(1_000_000)));
    let now = Utc::now();

    // A resting buy limit stays New while working (no ask depth to match).
    let req = order(
        OrderSide::Buy,
        OrderType::Limit(Price::new(dec!(90))),
        10,
        8,
    );
    let order_id = exec.submit_order(&req, now).unwrap();
    assert!(
        order_id.starts_with("PAPER-"),
        "expected a PAPER-* order id, got {order_id}"
    );

    // First cancel succeeds; a second cancel of the same id errors.
    exec.cancel_order(&order_id, now).unwrap();
    assert!(exec.cancel_order(&order_id, now).is_err());
}
