//! Paper exchange matching slices E1–E7 (see `docs/plans/0003`).
//!
//! Every slice is deterministic: fixed UUIDs, fixed prices, zero latency,
//! `ZeroFillCost`, and explicit `DateTime<Utc>` timestamps — no wall clock.

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tantra_core::{
    Exchange, InstrumentId, OrderRequest, OrderSide, OrderType, Price, Quantity, Tick, TimeInForce,
};
use tantra_exec_paper::{ExchangeEvent, FillReport, OrderState, PaperExchange};
use uuid::Uuid;

fn inst() -> InstrumentId {
    InstrumentId::try_new(Exchange::Nse, "RELIANCE").unwrap()
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

/// Fixed deterministic baseline timestamp.
fn ts() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(1_700_000_000_000).unwrap()
}

// ---------------------------------------------------------------------------
// E1: price priority across resting participants
// ---------------------------------------------------------------------------

#[test]
fn e1_price_priority_across_resting_participants() {
    let mut ex = PaperExchange::new(dec!(1_000_000));
    let t0 = ts();

    // Two resting buy limits: better price (101) submitted first.
    let b1 = ex
        .submit_order(
            order(
                OrderSide::Buy,
                OrderType::Limit(Price::try_new(dec!(101)).unwrap()),
                100,
                1,
            ),
            t0,
        )
        .unwrap();
    let b2 = ex
        .submit_order(
            order(
                OrderSide::Buy,
                OrderType::Limit(Price::try_new(dec!(100)).unwrap()),
                100,
                2,
            ),
            t0,
        )
        .unwrap();
    let s = ex
        .submit_order(order(OrderSide::Sell, OrderType::Market, 150, 3), t0)
        .unwrap();

    // First buy filled 100 @ 101 (price priority).
    let o1 = ex.order(&b1).unwrap();
    assert_eq!(o1.state, OrderState::Filled);
    assert_eq!(o1.filled_qty, Quantity::from_u64(100));
    assert_eq!(o1.avg_fill_price, Price::try_new(dec!(101)).unwrap());

    // Second buy filled 50 @ 100, still partially filled.
    let o2 = ex.order(&b2).unwrap();
    assert_eq!(o2.state, OrderState::PartiallyFilled);
    assert_eq!(o2.filled_qty, Quantity::from_u64(50));
    assert_eq!(o2.avg_fill_price, Price::try_new(dec!(100)).unwrap());

    // Sell fills 150 at VWAP = (100*101 + 50*100)/150 = 100.6667.
    let os = ex.order(&s).unwrap();
    assert_eq!(os.state, OrderState::Filled);
    assert_eq!(os.filled_qty, Quantity::from_u64(150));
    assert_eq!(os.avg_fill_price, Price::try_new(dec!(100.6667)).unwrap());

    // Net position: +100 +50 -150 = 0.
    assert_eq!(ex.positions().get(&inst()), Some(&Decimal::ZERO));
}

// ---------------------------------------------------------------------------
// E2: multi-level VWAP + IOC remainder cancellation
// ---------------------------------------------------------------------------

#[test]
fn e2_multi_level_vwap_with_ioc_cancel() {
    let mut ex = PaperExchange::new(dec!(1_000_000));
    let t0 = ts();

    let b1 = ex
        .submit_order(
            order(
                OrderSide::Buy,
                OrderType::Limit(Price::try_new(dec!(100)).unwrap()),
                40,
                1,
            ),
            t0,
        )
        .unwrap();
    let b2 = ex
        .submit_order(
            order(
                OrderSide::Buy,
                OrderType::Limit(Price::try_new(dec!(99)).unwrap()),
                20,
                2,
            ),
            t0,
        )
        .unwrap();

    let mut sell = order(OrderSide::Sell, OrderType::Market, 100, 3);
    sell.tif = TimeInForce::Ioc;
    let s = ex.submit_order(sell, t0).unwrap();

    // Sell fills 60 at VWAP = (4000 + 1980)/60 = 99.6667; the remaining 40
    // is cancelled (IOC).
    let os = ex.order(&s).unwrap();
    assert_eq!(os.state, OrderState::Cancelled);
    assert_eq!(os.filled_qty, Quantity::from_u64(60));
    assert_eq!(os.avg_fill_price, Price::try_new(dec!(99.6667)).unwrap());

    assert!(ex.events().iter().any(
        |e| matches!(e, ExchangeEvent::Cancelled { broker_order_id, .. } if broker_order_id == &s)
    ));

    // Both resting bids fully consumed.
    assert_eq!(ex.order(&b1).unwrap().state, OrderState::Filled);
    assert_eq!(ex.order(&b2).unwrap().state, OrderState::Filled);
}

// ---------------------------------------------------------------------------
// E3: IOC semantics — fill the available contra liquidity, cancel the rest
// ---------------------------------------------------------------------------

#[test]
fn e3_ioc_fills_available_and_cancels_remainder() {
    let mut ex = PaperExchange::new(dec!(1_000_000));
    let t0 = ts();

    // Resting contra liquidity: sell 150 @ 100.
    let s1 = ex
        .submit_order(
            order(
                OrderSide::Sell,
                OrderType::Limit(Price::try_new(dec!(100)).unwrap()),
                150,
                1,
            ),
            t0,
        )
        .unwrap();

    let mut buy = order(OrderSide::Buy, OrderType::Market, 200, 2);
    buy.tif = TimeInForce::Ioc;
    let b = ex.submit_order(buy, t0).unwrap();

    // Fills the 150 available, cancels the remaining 50.
    let ob = ex.order(&b).unwrap();
    assert_eq!(ob.state, OrderState::Cancelled);
    assert_eq!(ob.filled_qty, Quantity::from_u64(150));
    assert_eq!(ob.avg_fill_price, Price::try_new(dec!(100)).unwrap());

    assert_eq!(ex.order(&s1).unwrap().state, OrderState::Filled);
    assert!(ex.events().iter().any(
        |e| matches!(e, ExchangeEvent::Cancelled { broker_order_id, .. } if broker_order_id == &b)
    ));
}

// ---------------------------------------------------------------------------
// E4: GTD expiry
// ---------------------------------------------------------------------------

#[test]
fn e4_gtd_expiry_cancels_unfilled_order() {
    let mut ex = PaperExchange::new(dec!(1_000_000));
    let t0 = ts();
    let gtd_until = t0 + Duration::hours(1);

    // A non-marketable buy limit (no asks at or below 90).
    let id = ex
        .submit_order_gtd(
            order(
                OrderSide::Buy,
                OrderType::Limit(Price::try_new(dec!(90)).unwrap()),
                100,
                1,
            ),
            gtd_until,
            t0,
        )
        .unwrap();
    assert_eq!(ex.order(&id).unwrap().state, OrderState::Working);

    // Well after the GTD end: cancelled with an event, never filled.
    ex.advance_to(t0 + Duration::hours(2));
    let ord = ex.order(&id).unwrap();
    assert_eq!(ord.state, OrderState::Cancelled);
    assert_eq!(ord.filled_qty, Quantity::ZERO);
    assert!(ex.events().iter().any(
        |e| matches!(e, ExchangeEvent::Cancelled { broker_order_id, .. } if broker_order_id == &id)
    ));
}

// ---------------------------------------------------------------------------
// E5: stop-order trigger
// ---------------------------------------------------------------------------

#[test]
fn e5_stop_limit_arms_then_triggers_and_fills() {
    let mut ex = PaperExchange::new(dec!(1_000_000));
    let t0 = ts();
    let i = inst();
    ex.set_market_price(i.clone(), Price::try_new(dec!(100)).unwrap());

    // Resting sell provides the ask at the stop's limit.
    let s1 = ex
        .submit_order(
            order(
                OrderSide::Sell,
                OrderType::Limit(Price::try_new(dec!(106)).unwrap()),
                10,
                1,
            ),
            t0,
        )
        .unwrap();

    // Buy StopLimit{trigger: 105, limit: 106}: armed, no fill while last < trigger.
    let stop = ex
        .submit_order(
            order(
                OrderSide::Buy,
                OrderType::StopLimit {
                    trigger: Price::try_new(dec!(105)).unwrap(),
                    limit: Price::try_new(dec!(106)).unwrap(),
                },
                10,
                2,
            ),
            t0,
        )
        .unwrap();
    let os = ex.order(&stop).unwrap();
    assert_eq!(os.state, OrderState::New);
    assert_eq!(os.filled_qty, Quantity::ZERO);

    // Feed a tick whose last price (107) crosses the trigger.
    let tick = Tick::Ltp {
        instrument: i.clone(),
        price: Price::try_new(dec!(107)).unwrap(),
        volume: Quantity::from_u64(0),
        exchange_ts: t0,
        receive_ts: t0,
    };
    ex.feed_tick(&tick);

    let os = ex.order(&stop).unwrap();
    assert_eq!(os.state, OrderState::Filled);
    assert_eq!(os.avg_fill_price, Price::try_new(dec!(106)).unwrap());
    assert!(ex
        .events()
        .iter()
        .any(|e| matches!(e, ExchangeEvent::StopTriggered { broker_order_id, .. } if broker_order_id == &stop)));
    assert_eq!(ex.order(&s1).unwrap().state, OrderState::Filled);
}

// ---------------------------------------------------------------------------
// E6: cancel + amend
// ---------------------------------------------------------------------------

#[test]
fn e6_cancel_then_amend_then_fill() {
    let mut ex = PaperExchange::new(dec!(1_000_000));
    let t0 = ts();
    let i = inst();

    // Cancel a resting order: no position is opened.
    let b1 = ex
        .submit_order(
            order(
                OrderSide::Buy,
                OrderType::Limit(Price::try_new(dec!(90)).unwrap()),
                50,
                1,
            ),
            t0,
        )
        .unwrap();
    ex.cancel_order(&b1, t0).unwrap();
    assert_eq!(ex.order(&b1).unwrap().state, OrderState::Cancelled);
    assert_eq!(ex.positions().get(&i), None);
    assert!(ex.events().iter().any(
        |e| matches!(e, ExchangeEvent::Cancelled { broker_order_id, .. } if broker_order_id == &b1)
    ));

    // Cancelling a terminal order is an error.
    assert!(ex.cancel_order(&b1, t0).is_err());

    // Rest a fresh buy limit, amend its price up to 95, then feed a sell
    // that crosses at 95.
    let b2 = ex
        .submit_order(
            order(
                OrderSide::Buy,
                OrderType::Limit(Price::try_new(dec!(90)).unwrap()),
                40,
                2,
            ),
            t0,
        )
        .unwrap();
    ex.amend_order(&b2, Some(Price::try_new(dec!(95)).unwrap()), None, t0)
        .unwrap();
    let ob = ex.order(&b2).unwrap();
    assert_eq!(
        ob.order_type,
        OrderType::Limit(Price::try_new(dec!(95)).unwrap())
    );
    assert_eq!(ob.state, OrderState::Working);

    let tick = Tick::Quote {
        instrument: i.clone(),
        bid: Price::try_new(dec!(95)).unwrap(),
        ask: Price::try_new(dec!(95)).unwrap(),
        bid_qty: Quantity::from_u64(40),
        ask_qty: Quantity::from_u64(40),
        last_price: Some(Price::try_new(dec!(95)).unwrap()),
        exchange_ts: t0,
        receive_ts: t0,
        band: None,
    };
    ex.feed_tick(&tick);

    let ob = ex.order(&b2).unwrap();
    assert_eq!(ob.state, OrderState::Filled);
    assert_eq!(ob.avg_fill_price, Price::try_new(dec!(95)).unwrap());
    assert_eq!(ex.positions().get(&i), Some(&Decimal::from(40)));
}

// ---------------------------------------------------------------------------
// E7: determinism — the same tape replayed twice yields byte-identical
// fills and events.
// ---------------------------------------------------------------------------

#[test]
fn e7_determinism_golden_replay() {
    let (fills_a, events_a) = run_tape();
    let (fills_b, events_b) = run_tape();
    assert_eq!(fills_a, fills_b);
    assert_eq!(events_a, events_b);
    // The tape actually produces fills and events (sanity).
    assert!(!fills_a.is_empty());
    assert!(!events_a.is_empty());
}

type Tape = (Vec<FillReport>, Vec<ExchangeEvent>);

fn run_tape() -> Tape {
    let mut ex = PaperExchange::new(dec!(1_000_000));
    let t0 = ts();
    let i = inst();
    ex.set_market_price(i.clone(), Price::try_new(dec!(100)).unwrap());

    ex.submit_order(
        order(
            OrderSide::Buy,
            OrderType::Limit(Price::try_new(dec!(101)).unwrap()),
            100,
            1,
        ),
        t0,
    )
    .unwrap();
    ex.submit_order(
        order(
            OrderSide::Buy,
            OrderType::Limit(Price::try_new(dec!(100)).unwrap()),
            100,
            2,
        ),
        t0,
    )
    .unwrap();
    ex.submit_order(order(OrderSide::Sell, OrderType::Market, 150, 3), t0)
        .unwrap();
    // A later Ltp tick feeds synthetic depth and re-matches the leftover
    // partial buy (which is not marketable at 100 against an ask of 105).
    ex.feed_tick(&Tick::Ltp {
        instrument: i,
        price: Price::try_new(dec!(105)).unwrap(),
        volume: Quantity::from_u64(10),
        exchange_ts: t0,
        receive_ts: t0,
    });

    (ex.fills().to_vec(), ex.events().to_vec())
}

// ---------------------------------------------------------------------------
// Extra slice: zero-floats + no-wall-clock invariants are compile-time here,
// but assert the accounting math directly for one crossing.
// ---------------------------------------------------------------------------

#[test]
fn cash_and_positions_accounting_on_cross() {
    let mut ex = PaperExchange::new(dec!(1_000_000));
    let t0 = ts();
    let i = inst();

    let s = ex
        .submit_order(
            order(
                OrderSide::Sell,
                OrderType::Limit(Price::try_new(dec!(100)).unwrap()),
                10,
                1,
            ),
            t0,
        )
        .unwrap();
    ex.submit_order(order(OrderSide::Buy, OrderType::Market, 10, 2), t0)
        .unwrap();

    // The exchange keeps one aggregate cash balance: the buyer pays 10 @ 100
    // and the seller receives 10 @ 100, so cash nets to zero with
    // `ZeroFillCost`, and the net position returns to flat.
    assert_eq!(ex.cash(), dec!(1_000_000));
    assert_eq!(ex.positions().get(&i), Some(&Decimal::ZERO));
    assert_eq!(ex.order(&s).unwrap().state, OrderState::Filled);
}
