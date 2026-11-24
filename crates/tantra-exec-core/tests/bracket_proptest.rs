//! Property tests for the bracket tracker (gap 0003):
//! - `modify_stop` never widens risk (longs may only move up, shorts down);
//! - after any sequence of partial exits, `open_qty + filled_target_qty`
//!   always equals the entry quantity (no double-counting, no leakage).

use chrono::{DateTime, Utc};
use proptest::prelude::*;
use rust_decimal::Decimal;
use tantra_core::{
    Exchange, InstrumentId, OrderRequest, OrderSide, OrderType, PlannedTarget, Price, Quantity,
};
use tantra_exec_core::bracket::{BracketError, BracketOrderSpec, BracketTracker};

fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).expect("valid timestamp")
}

fn inst() -> InstrumentId {
    InstrumentId::try_new(Exchange::Nse, "NIFTY").unwrap()
}

fn spec_long(entry_qty: u64, targets: Vec<PlannedTarget>) -> BracketOrderSpec {
    BracketOrderSpec::new(
        OrderRequest::with_id(
            tantra_core::uuid::Uuid::new_v4(),
            inst(),
            OrderSide::Buy,
            OrderType::Limit(Price::from_rupees("100.00").unwrap()),
            Quantity::from_u64(entry_qty),
            "algo-t12-5",
        ),
        Price::from_rupees("90.00").unwrap(),
        targets,
        None,
    )
    .unwrap()
}

fn spec_short(entry_qty: u64, targets: Vec<PlannedTarget>) -> BracketOrderSpec {
    BracketOrderSpec::new(
        OrderRequest::with_id(
            tantra_core::uuid::Uuid::new_v4(),
            inst(),
            OrderSide::Sell,
            OrderType::Limit(Price::from_rupees("100.00").unwrap()),
            Quantity::from_u64(entry_qty),
            "algo-t12-5",
        ),
        Price::from_rupees("110.00").unwrap(),
        targets,
        None,
    )
    .unwrap()
}

proptest! {
    /// For a long, a valid stop move is never below the resting stop: any
    /// `new_stop < resting` is refused with `StopWidening` and leaves the
    /// resting stop untouched; anything else succeeds and becomes the new rest.
    #[test]
    fn long_modify_stop_never_widens_risk(new_stop in 1u64..5000u64) {
        let mut tracker = BracketTracker::new(spec_long(100, Vec::new()), at(1000)).unwrap();
        tracker
            .on_entry_fill(Price::from_rupees("100.00").unwrap(), Quantity::from_u64(100), at(1001))
            .unwrap();
        let resting = tracker.resting_stop().value();
        let new_stop = Price::try_new(Decimal::from(new_stop)).unwrap();

        match tracker.modify_stop(new_stop, None, at(1002)) {
            Ok(()) => {
                assert!(new_stop.value() >= resting, "long stop must never move down");
                assert_eq!(tracker.resting_stop(), new_stop);
            }
            Err(BracketError::StopWidening { current, new }) => {
                assert_eq!(current.value(), resting);
                assert_eq!(new, new_stop);
                assert!(new_stop.value() < resting, "StopWidening only below the resting stop");
                assert_eq!(tracker.resting_stop().value(), resting);
            }
            Err(other) => panic!("unexpected error {other:?}"),
        }
    }

    /// For a short, a valid stop move is never above the resting stop: any
    /// `new_stop > resting` is refused with `StopWidening`; anything else
    /// succeeds and becomes the new rest.
    #[test]
    fn short_modify_stop_never_widens_risk(new_stop in 1u64..5000u64) {
        let mut tracker = BracketTracker::new(spec_short(100, Vec::new()), at(1000)).unwrap();
        tracker
            .on_entry_fill(Price::from_rupees("100.00").unwrap(), Quantity::from_u64(100), at(1001))
            .unwrap();
        let resting = tracker.resting_stop().value();
        let new_stop = Price::try_new(Decimal::from(new_stop)).unwrap();

        match tracker.modify_stop(new_stop, None, at(1002)) {
            Ok(()) => {
                assert!(new_stop.value() <= resting, "short stop must never move up");
                assert_eq!(tracker.resting_stop(), new_stop);
            }
            Err(BracketError::StopWidening { current, new }) => {
                assert_eq!(current.value(), resting);
                assert_eq!(new, new_stop);
                assert!(new_stop.value() > resting, "StopWidening only above the resting stop");
                assert_eq!(tracker.resting_stop().value(), resting);
            }
            Err(other) => panic!("unexpected error {other:?}"),
        }
    }

    /// After any FIFO partial-exit sequence (each exit matches the front
    /// remaining target exactly), the open quantity plus the quantity exited
    /// via filled targets always equals the entry quantity.
    #[test]
    fn partial_exits_keep_open_plus_filled_target_qty_equal(
        q1 in 1u64..30u64,
        q2 in 1u64..30u64,
        q3 in 1u64..30u64,
    ) {
        let entry_qty = q1 + q2 + q3 + 10; // a runner keeps 10 open at the end
        let targets = vec![
            PlannedTarget {
                price: Price::from_rupees("110.00").unwrap(),
                qty: Quantity::from_u64(q1),
                reason: "t1".into(),
            },
            PlannedTarget {
                price: Price::from_rupees("120.00").unwrap(),
                qty: Quantity::from_u64(q2),
                reason: "t2".into(),
            },
            PlannedTarget {
                price: Price::from_rupees("130.00").unwrap(),
                qty: Quantity::from_u64(q3),
                reason: "t3".into(),
            },
        ];
        let mut tracker = BracketTracker::new(spec_long(entry_qty, targets), at(1000)).unwrap();
        tracker
            .on_entry_fill(
                Price::from_rupees("100.00").unwrap(),
                Quantity::from_u64(entry_qty),
                at(1001),
            )
            .unwrap();

        for leg in [q1, q2, q3] {
            tracker
                .partial_exit(
                    Quantity::from_u64(leg),
                    Price::from_rupees("110.00").unwrap(),
                    "leg",
                    at(1002),
                )
                .unwrap();
            prop_assert_eq!(
                tracker.open_qty().value() + tracker.filled_target_qty().value(),
                Decimal::from(entry_qty),
                "open + filled-target qty must equal the entry qty"
            );
        }

        // FIFO order: every leg was matched and filled.
        prop_assert_eq!(tracker.remaining_targets().len(), 0);
        prop_assert_eq!(tracker.filled_targets().len(), 3);
        prop_assert_eq!(tracker.open_qty().value(), Decimal::from(10));
    }
}
