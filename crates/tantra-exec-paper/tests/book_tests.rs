//! E8 (part 1): ported L2 order book sanity + price-time priority.
//!
//! Golden behavior of `tantra-exec-paper::book::OrderBook` must match the
//! source `tantra-runtime-backtest::sim::l2_book` byte-for-byte (parity
//! surface for `tantra-runtime/docs/plans/0005`).

use rust_decimal_macros::dec;
use tantra_core::{OrderSide, Price};
use tantra_exec_paper::book::OrderBook;

#[test]
fn sell_market_walks_bid_levels_with_vwap() {
    let mut book = OrderBook::new(0, dec!(1));
    book.add_bid(Price::new(dec!(100)), 40);
    book.add_bid(Price::new(dec!(99)), 20);

    let (filled, vwap, remaining, worst) = book.execute_market_order(OrderSide::Sell, 60);

    assert_eq!(filled, 60);
    // VWAP = (40*100 + 20*99) / 60 = 5980/60 = 99.6667 (rounded to 4dp).
    assert_eq!(vwap, Price::new(dec!(99.6667)));
    assert_eq!(remaining, 0);
    // Worst (lowest) price received for a sell.
    assert_eq!(worst, Some(Price::new(dec!(99))));
}

#[test]
fn sell_market_respects_price_time_priority() {
    let mut book = OrderBook::new(0, dec!(1));
    book.add_bid(Price::new(dec!(101)), 10);
    book.add_bid(Price::new(dec!(100)), 10);

    let (filled, vwap, remaining, worst) = book.execute_market_order(OrderSide::Sell, 15);

    // 10 @ 101 then 5 @ 100; VWAP = (1010 + 500)/15 = 1510/15 = 100.6667.
    assert_eq!(filled, 15);
    assert_eq!(vwap, Price::new(dec!(100.6667)));
    assert_eq!(remaining, 0);
    assert_eq!(worst, Some(Price::new(dec!(100))));
}

#[test]
fn buy_market_walks_ask_levels_with_vwap() {
    let mut book = OrderBook::new(0, dec!(1));
    book.add_ask(Price::new(dec!(98)), 30);
    book.add_ask(Price::new(dec!(99)), 30);

    let (filled, vwap, remaining, worst) = book.execute_market_order(OrderSide::Buy, 45);

    assert_eq!(filled, 45);
    // VWAP = (30*98 + 15*99) / 45 = (2940 + 1485)/45 = 4425/45 = 98.3333.
    assert_eq!(vwap, Price::new(dec!(98.3333)));
    assert_eq!(remaining, 0);
    // Worst (highest) price paid for a buy.
    assert_eq!(worst, Some(Price::new(dec!(99))));
}

#[test]
fn limit_buy_only_fills_at_or_below_limit() {
    let mut book = OrderBook::new(0, dec!(1));
    book.add_ask(Price::new(dec!(100)), 50);
    book.add_ask(Price::new(dec!(102)), 50);

    // Limit 101: fills the 100 level, leaves the 102 level.
    let (filled, vwap, remaining) =
        book.execute_limit_order(OrderSide::Buy, 100, Price::new(dec!(101)));
    assert_eq!(filled, 50);
    assert_eq!(vwap, Price::new(dec!(100)));
    assert_eq!(remaining, 50);

    // Spread/mid sanity after the partial consumption.
    let (bid, ask) = book.get_spread();
    assert_eq!(bid, None);
    assert_eq!(ask, Some(Price::new(dec!(102))));
}

#[test]
fn ready_ts_is_pure_arithmetic_on_the_submitted_timestamp() {
    use chrono::{DateTime, Duration};
    let book = OrderBook::new(250, dec!(1));
    let ts = DateTime::from_timestamp_millis(1_700_000_000_000).unwrap();
    // 250 µs of configured latency, no clock read.
    assert_eq!(book.ready_ts(ts), ts + Duration::microseconds(250));
}
