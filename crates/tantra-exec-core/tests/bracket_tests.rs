//! Bracket-order (SL / multi-target / trailing-stop) contract tests (gap 0003):
//! spec validation, the full-lifecycle golden vector, stop-widening guards on
//! both sides, the partial-exit short-flip guard, FIFO target matching, and the
//! deterministic stop-leg client order id.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use tantra_core::{
    Exchange, InstrumentId, OrderRequest, OrderSide, OrderType, PlannedTarget, Price, Quantity,
    TrailRule,
};
use tantra_exec_core::bracket::{
    BracketError, BracketEvent, BracketOrderSpec, BracketTracker, stop_client_order_id,
};

/// Fixed injected timestamps keep the golden vector byte-deterministic.
fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).expect("valid timestamp")
}

fn inst() -> InstrumentId {
    InstrumentId::try_new(Exchange::Nse, "NIFTY").unwrap()
}

fn entry_buy(price: &str, qty: u64) -> OrderRequest {
    OrderRequest::with_id(
        tantra_core::uuid::Uuid::new_v4(),
        inst(),
        OrderSide::Buy,
        OrderType::Limit(Price::from_rupees(price).unwrap()),
        Quantity::from_u64(qty),
        "algo-t12-5",
    )
}

fn entry_sell(price: &str, qty: u64) -> OrderRequest {
    OrderRequest::with_id(
        tantra_core::uuid::Uuid::new_v4(),
        inst(),
        OrderSide::Sell,
        OrderType::Limit(Price::from_rupees(price).unwrap()),
        Quantity::from_u64(qty),
        "algo-t12-5",
    )
}

fn target(price: &str, qty: u64, reason: &str) -> PlannedTarget {
    PlannedTarget {
        price: Price::from_rupees(price).unwrap(),
        qty: Quantity::from_u64(qty),
        reason: reason.into(),
    }
}

fn price(s: &str) -> Price {
    Price::from_rupees(s).unwrap()
}

fn qty(n: u64) -> Quantity {
    Quantity::from_u64(n)
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str_exact(s).unwrap()
}

// ---------------------------------------------------------------------------
// Spec validation
// ---------------------------------------------------------------------------

#[test]
fn spec_validation_refuses_wrong_side_stop() {
    // Buy: the stop must be strictly below the entry.
    for stop in ["100.00", "105.00"] {
        let err = BracketOrderSpec::new(entry_buy("100.00", 30), price(stop), Vec::new(), None)
            .unwrap_err();
        assert!(
            matches!(err, BracketError::InvalidSpec(_)),
            "buy stop {stop} should be refused"
        );
    }

    // Sell: the stop must be strictly above the entry.
    for stop in ["100.00", "95.00"] {
        let err = BracketOrderSpec::new(entry_sell("100.00", 30), price(stop), Vec::new(), None)
            .unwrap_err();
        assert!(
            matches!(err, BracketError::InvalidSpec(_)),
            "sell stop {stop} should be refused"
        );
    }

    // Correct sides pass.
    BracketOrderSpec::new(entry_buy("100.00", 30), price("95.00"), Vec::new(), None).unwrap();
    BracketOrderSpec::new(entry_sell("100.00", 30), price("105.00"), Vec::new(), None).unwrap();
}

#[test]
fn spec_validation_refuses_malformed_target_ladders() {
    // Non-positive target quantity.
    let err = BracketOrderSpec::new(
        entry_buy("100.00", 30),
        price("95.00"),
        vec![target("110.00", 0, "zero leg")],
        None,
    )
    .unwrap_err();
    assert!(matches!(err, BracketError::InvalidSpec(_)));

    // Target ladder summing beyond the entry quantity.
    let err = BracketOrderSpec::new(
        entry_buy("100.00", 30),
        price("95.00"),
        vec![target("110.00", 20, "t1"), target("120.00", 15, "t2")], // 35 > 30
        None,
    )
    .unwrap_err();
    assert!(matches!(err, BracketError::InvalidSpec(_)));

    // A ladder within the entry quantity passes.
    BracketOrderSpec::new(
        entry_buy("100.00", 30),
        price("95.00"),
        vec![target("110.00", 10, "t1")],
        Some(TrailRule::Off),
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Full-lifecycle golden vector
// ---------------------------------------------------------------------------

#[test]
fn full_lifecycle_golden_entry_partial_exit_trail_stop_trigger() {
    let spec = BracketOrderSpec::new(
        entry_buy("100.00", 30),
        price("95.00"),
        vec![target("110.00", 10, "target 1 at 1R")],
        Some(TrailRule::Percent(dec("2"))),
    )
    .unwrap();
    let mut tracker = BracketTracker::new(spec, at(1000)).unwrap();
    assert!(!tracker.is_closed());
    assert_eq!(tracker.open_qty(), Quantity::ZERO);
    assert_eq!(tracker.resting_stop(), price("95.00"));

    // 1. Entry fills 30 @ 100.
    tracker
        .on_entry_fill(price("100.00"), qty(30), at(1001))
        .unwrap();
    assert_eq!(tracker.open_qty(), qty(30));
    assert_eq!(
        tracker.events(),
        &[BracketEvent::EntryFilled {
            price: price("100.00"),
            qty: qty(30),
            ts: at(1001),
        }]
    );

    // 2. Target 1 fills: partial exit 10 @ 110 (matches the front leg exactly).
    tracker
        .partial_exit(qty(10), price("110.00"), "target 1 at 1R", at(1002))
        .unwrap();
    assert_eq!(tracker.open_qty(), qty(20));
    assert_eq!(tracker.filled_target_qty(), qty(10));
    assert_eq!(tracker.remaining_targets(), &[] as &[PlannedTarget]);

    // 3. Trail the stop to 98 and resize the stop to the remaining 20 (TSL).
    tracker
        .modify_stop(price("98.00"), Some(qty(20)), at(1003))
        .unwrap();
    assert_eq!(tracker.resting_stop(), price("98.00"));
    match &tracker.events()[tracker.events().len() - 1] {
        BracketEvent::StopMoved {
            old,
            new,
            qty: stop_qty,
            ts,
        } => {
            assert_eq!(*old, price("95.00"));
            assert_eq!(*new, price("98.00"));
            assert_eq!(*stop_qty, qty(20));
            assert_eq!(*ts, at(1003));
        }
        other => panic!("expected StopMoved, got {other:?}"),
    }

    // 4. Stop triggers at 98: realized PnL = (98 - 100) * 20 = -40 (exact).
    tracker.on_stop_trigger(price("98.00"), at(1004)).unwrap();
    assert!(tracker.is_closed());
    assert_eq!(tracker.open_qty(), Quantity::ZERO);
    assert_eq!(tracker.events().len(), 5);
    assert_eq!(
        tracker.events()[tracker.events().len() - 2],
        BracketEvent::StopTriggered {
            price: price("98.00"),
            qty: qty(20),
            ts: at(1004),
        }
    );
    match &tracker.events()[tracker.events().len() - 1] {
        BracketEvent::Closed {
            reason,
            realized_pnl,
        } => {
            assert_eq!(reason, "stop_trigger");
            assert_eq!(*realized_pnl, dec("-40.00"));
        }
        other => panic!("expected Closed, got {other:?}"),
    }
}

#[test]
fn stop_trigger_realized_pnl_is_exact_for_shorts() {
    // Short entry 100, stop 105, trailed down to 103:
    // realized PnL = (entry - stop) * qty = (100 - 103) * 20 = -60 (exact).
    let spec =
        BracketOrderSpec::new(entry_sell("100.00", 20), price("105.00"), Vec::new(), None).unwrap();
    let mut tracker = BracketTracker::new(spec, at(1000)).unwrap();
    tracker
        .on_entry_fill(price("100.00"), qty(20), at(1001))
        .unwrap();
    tracker
        .modify_stop(price("103.00"), None, at(1002))
        .unwrap();
    tracker.on_stop_trigger(price("103.00"), at(1003)).unwrap();
    assert!(tracker.is_closed());
    match &tracker.events()[tracker.events().len() - 1] {
        BracketEvent::Closed {
            reason,
            realized_pnl,
        } => {
            assert_eq!(reason, "stop_trigger");
            assert_eq!(*realized_pnl, dec("-60.00"));
        }
        other => panic!("expected Closed, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Stop-widening guards
// ---------------------------------------------------------------------------

#[test]
fn modify_stop_refuses_widening_on_both_sides() {
    // Long: stop at 95; only >= 95 is legal (trail up).
    let mut long = BracketTracker::new(
        BracketOrderSpec::new(entry_buy("100.00", 30), price("95.00"), Vec::new(), None).unwrap(),
        at(1000),
    )
    .unwrap();
    long.on_entry_fill(price("100.00"), qty(30), at(1001))
        .unwrap();
    let err = long
        .modify_stop(price("93.00"), None, at(1002))
        .unwrap_err();
    assert_eq!(
        err,
        BracketError::StopWidening {
            current: price("95.00"),
            new: price("93.00"),
        }
    );
    assert_eq!(long.resting_stop(), price("95.00"));
    long.modify_stop(price("96.00"), None, at(1003)).unwrap();
    assert_eq!(long.resting_stop(), price("96.00"));

    // Short: stop at 105; only <= 105 is legal (trail down).
    let mut short = BracketTracker::new(
        BracketOrderSpec::new(entry_sell("100.00", 30), price("105.00"), Vec::new(), None).unwrap(),
        at(1000),
    )
    .unwrap();
    short
        .on_entry_fill(price("100.00"), qty(30), at(1001))
        .unwrap();
    let err = short
        .modify_stop(price("107.00"), None, at(1002))
        .unwrap_err();
    assert_eq!(
        err,
        BracketError::StopWidening {
            current: price("105.00"),
            new: price("107.00"),
        }
    );
    assert_eq!(short.resting_stop(), price("105.00"));
    short.modify_stop(price("104.00"), None, at(1003)).unwrap();
    assert_eq!(short.resting_stop(), price("104.00"));
}

#[test]
fn modify_stop_refuses_stop_quantity_exceeding_open() {
    let mut tracker = BracketTracker::new(
        BracketOrderSpec::new(entry_buy("100.00", 30), price("95.00"), Vec::new(), None).unwrap(),
        at(1000),
    )
    .unwrap();
    tracker
        .on_entry_fill(price("100.00"), qty(30), at(1001))
        .unwrap();

    let err = tracker
        .modify_stop(price("96.00"), Some(qty(31)), at(1002))
        .unwrap_err();
    assert_eq!(
        err,
        BracketError::QuantityMismatch {
            open_qty: qty(30),
            stop_qty: qty(31),
        }
    );

    // After a partial exit to 20, the stop may be resized to 20 but not 25.
    tracker
        .partial_exit(qty(10), price("110.00"), "t1", at(1003))
        .unwrap();
    let err = tracker
        .modify_stop(price("96.00"), Some(qty(25)), at(1004))
        .unwrap_err();
    assert!(matches!(err, BracketError::QuantityMismatch { .. }));
    tracker
        .modify_stop(price("96.00"), Some(qty(20)), at(1005))
        .unwrap();
}

// ---------------------------------------------------------------------------
// Partial-exit guards and FIFO target matching
// ---------------------------------------------------------------------------

#[test]
fn partial_exit_short_flip_guard_never_goes_negative() {
    let mut tracker = BracketTracker::new(
        BracketOrderSpec::new(
            entry_buy("100.00", 30),
            price("95.00"),
            vec![target("110.00", 10, "t1")],
            None,
        )
        .unwrap(),
        at(1000),
    )
    .unwrap();
    tracker
        .on_entry_fill(price("100.00"), qty(30), at(1001))
        .unwrap();

    // Exiting more than the open position is refused; open qty is untouched.
    let err = tracker
        .partial_exit(qty(40), price("110.00"), "over-exit", at(1002))
        .unwrap_err();
    assert!(matches!(err, BracketError::InvalidSpec(_)));
    assert_eq!(tracker.open_qty(), qty(30));

    // Zero-quantity exits are refused too.
    let err = tracker
        .partial_exit(Quantity::ZERO, price("110.00"), "no-op", at(1003))
        .unwrap_err();
    assert!(matches!(err, BracketError::InvalidSpec(_)));

    // Exiting exactly the position flattens it; further exits are refused.
    tracker
        .partial_exit(qty(30), price("110.00"), "flat", at(1004))
        .unwrap();
    assert_eq!(tracker.open_qty(), Quantity::ZERO);
    assert!(
        tracker
            .partial_exit(qty(1), price("110.00"), "flip", at(1005))
            .is_err(),
        "open qty must never go negative"
    );
}

#[test]
fn partial_exit_matches_targets_fifo_front_only() {
    let mut tracker = BracketTracker::new(
        BracketOrderSpec::new(
            entry_buy("100.00", 40),
            price("95.00"),
            vec![target("110.00", 10, "t1"), target("120.00", 5, "t2")],
            None,
        )
        .unwrap(),
        at(1000),
    )
    .unwrap();
    tracker
        .on_entry_fill(price("100.00"), qty(40), at(1001))
        .unwrap();

    // A partial matching a NON-front leg fills no target (free partial).
    tracker
        .partial_exit(qty(5), price("120.00"), "t2-sized partial", at(1002))
        .unwrap();
    assert_eq!(tracker.filled_targets().len(), 0);
    assert_eq!(tracker.remaining_targets().len(), 2);
    assert_eq!(tracker.open_qty(), qty(35));

    // The front leg (10) fills when a partial matches it exactly.
    tracker
        .partial_exit(qty(10), price("110.00"), "t1", at(1003))
        .unwrap();
    assert_eq!(tracker.filled_targets().len(), 1);
    assert_eq!(tracker.filled_targets()[0].qty, qty(10));
    assert_eq!(tracker.remaining_targets().len(), 1);
    assert_eq!(tracker.remaining_targets()[0].qty, qty(5));
    assert_eq!(tracker.open_qty(), qty(25));

    // Now the front is t2 (5); a matching partial fills it.
    tracker
        .partial_exit(qty(5), price("120.00"), "t2", at(1004))
        .unwrap();
    assert_eq!(tracker.filled_targets().len(), 2);
    assert_eq!(tracker.remaining_targets().len(), 0);
    assert_eq!(tracker.open_qty(), qty(20));
    assert_eq!(tracker.filled_target_qty(), qty(15));
}

// ---------------------------------------------------------------------------
// Lifecycle guards and deterministic stop client order id
// ---------------------------------------------------------------------------

#[test]
fn tracker_guards_operations_until_entry_fills() {
    let mut tracker = BracketTracker::new(
        BracketOrderSpec::new(entry_buy("100.00", 30), price("95.00"), Vec::new(), None).unwrap(),
        at(1000),
    )
    .unwrap();

    assert!(matches!(
        tracker
            .modify_stop(price("96.00"), None, at(1001))
            .unwrap_err(),
        BracketError::NotActive(_)
    ));
    assert!(matches!(
        tracker
            .partial_exit(qty(5), price("110.00"), "x", at(1002))
            .unwrap_err(),
        BracketError::NotActive(_)
    ));
    assert!(matches!(
        tracker
            .on_stop_trigger(price("95.00"), at(1003))
            .unwrap_err(),
        BracketError::NotActive(_)
    ));

    // The entry fill is refused twice.
    tracker
        .on_entry_fill(price("100.00"), qty(30), at(1004))
        .unwrap();
    assert!(matches!(
        tracker
            .on_entry_fill(price("100.00"), qty(30), at(1005))
            .unwrap_err(),
        BracketError::NotActive(_)
    ));

    // A fill qty not equal to the bracket entry qty is refused.
    let mut mismatched = BracketTracker::new(
        BracketOrderSpec::new(entry_buy("100.00", 30), price("95.00"), Vec::new(), None).unwrap(),
        at(1000),
    )
    .unwrap();
    assert!(matches!(
        mismatched
            .on_entry_fill(price("100.00"), qty(20), at(1001))
            .unwrap_err(),
        BracketError::InvalidSpec(_)
    ));

    // A market entry with no reference price is validated at fill time.
    let market = OrderRequest::with_id(
        tantra_core::uuid::Uuid::new_v4(),
        inst(),
        OrderSide::Buy,
        OrderType::Market,
        qty(30),
        "algo-t12-5",
    );
    let mut market_tracker = BracketTracker::new(
        BracketOrderSpec::new(market, price("95.00"), Vec::new(), None).unwrap(),
        at(1000),
    )
    .unwrap();
    let err = market_tracker
        .on_entry_fill(price("90.00"), qty(30), at(1001))
        .unwrap_err();
    assert!(
        matches!(err, BracketError::InvalidSpec(_)),
        "fill below the stop must be refused for a long"
    );
}

#[test]
fn stop_client_order_id_is_deterministic() {
    assert_eq!(stop_client_order_id("BRK-42"), "BRK-42#stop");
    assert_eq!(
        stop_client_order_id("BRK-42"),
        stop_client_order_id("BRK-42")
    );
    assert_eq!(stop_client_order_id("bracket-1"), "bracket-1#stop");
}
