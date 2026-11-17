//! Bracket-order broker contract tests (gap 0003): `MockBrokerExecutor`
//! `submit_bracket` / `modify_stop` behavior and the native-vs-emulated
//! bracket capability matrix.

use tantra_core::{
    Exchange, InstrumentId, OrderRequest, OrderSide, OrderType, PlannedTarget, Price, Quantity,
};
use tantra_exec_core::bracket::{BracketCapabilities, BracketOrderSpec, stop_client_order_id};
use tantra_exec_core::{BrokerError, BrokerExecutor, MockBrokerExecutor};

fn inst() -> InstrumentId {
    InstrumentId::new(Exchange::Nse, "NIFTY")
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

fn target(price: &str, qty: u64, reason: &str) -> PlannedTarget {
    PlannedTarget {
        price: Price::from_rupees(price).unwrap(),
        qty: Quantity::from_u64(qty),
        reason: reason.into(),
    }
}

fn spec() -> BracketOrderSpec {
    BracketOrderSpec::new(
        entry_buy("100.00", 30),
        Price::from_rupees("95.00").unwrap(),
        vec![target("110.00", 10, "t1"), target("120.00", 5, "t2")],
        None,
    )
    .unwrap()
}

fn price(s: &str) -> Price {
    Price::from_rupees(s).unwrap()
}

#[tokio::test]
async fn submit_bracket_returns_ticket_with_distinct_ids() {
    let broker = MockBrokerExecutor::accept_all();
    let ticket = broker.submit_bracket(spec()).await.unwrap();

    assert!(!ticket.bracket_id.is_empty());
    assert!(!ticket.entry_id.is_empty());
    assert!(!ticket.stop_id.is_empty());
    assert_eq!(ticket.target_ids.len(), 2);

    // Every bracket leg id is distinct.
    let mut ids = vec![ticket.entry_id.clone(), ticket.stop_id.clone()];
    ids.extend(ticket.target_ids.iter().cloned());
    for (i, a) in ids.iter().enumerate() {
        for b in ids.iter().skip(i + 1) {
            assert_ne!(a, b, "bracket leg ids must be distinct, got {ids:?}");
        }
    }

    // The stop leg carries the deterministic client order id.
    assert_eq!(ticket.stop_id, stop_client_order_id(&ticket.bracket_id));

    // Emulated placement records entry + stop + target child orders.
    assert_eq!(broker.placed_orders().await.len(), 1 + 1 + 2);
}

#[tokio::test]
async fn modify_stop_round_trips() {
    let broker = MockBrokerExecutor::accept_all();
    let ticket = broker.submit_bracket(spec()).await.unwrap();

    let update = broker
        .modify_stop(
            &ticket.bracket_id,
            price("97.50"),
            Some(Quantity::from_u64(15)),
        )
        .await
        .unwrap();
    assert_eq!(update.bracket_id, ticket.bracket_id);
    assert_eq!(update.order_id, ticket.stop_id);
    assert_eq!(update.stop, price("97.50"));
    assert_eq!(update.qty, Quantity::from_u64(15));

    // A follow-up modify reflects the updated resting state.
    let update = broker
        .modify_stop(&ticket.bracket_id, price("98.25"), None)
        .await
        .unwrap();
    assert_eq!(update.order_id, ticket.stop_id);
    assert_eq!(update.stop, price("98.25"));
    assert_eq!(update.qty, Quantity::from_u64(15));

    // Unknown bracket ids are refused.
    let err = broker
        .modify_stop("MOCK-BR-nope", price("96.00"), None)
        .await
        .unwrap_err();
    assert!(matches!(err, BrokerError::OrderNotFound(_)));
}

#[tokio::test]
async fn capability_matrix_selects_placement_strategy() {
    // Emulated multileg (default): child orders for entry + stop + targets.
    let emulated = MockBrokerExecutor::accept_all();
    let ticket = emulated.submit_bracket(spec()).await.unwrap();
    assert_eq!(emulated.placed_orders().await.len(), 1 + 1 + 2);
    assert_ne!(ticket.entry_id, ticket.stop_id);
    assert_eq!(ticket.target_ids.len(), 2);
    assert!(emulated.bracket_capabilities().emulated_multileg);
    assert!(emulated.bracket_capabilities().atomic_modify);

    // Native BO: one broker order for the whole bracket; the legs collapse.
    let native = MockBrokerExecutor::accept_all().with_bracket_capabilities(BracketCapabilities {
        native_bo: true,
        emulated_multileg: false,
        atomic_modify: true,
    });
    let ticket = native.submit_bracket(spec()).await.unwrap();
    assert_eq!(native.placed_orders().await.len(), 1);
    assert_eq!(ticket.entry_id, ticket.bracket_id);
    assert_eq!(ticket.entry_id, ticket.stop_id);
    assert!(ticket.target_ids.is_empty());
    assert!(native.bracket_capabilities().native_bo);

    // No strategy at all: refused fail-closed.
    let none =
        MockBrokerExecutor::accept_all().with_bracket_capabilities(BracketCapabilities::none());
    let err = none.submit_bracket(spec()).await.unwrap_err();
    assert!(matches!(err, BrokerError::InvalidRequest(_)));

    // Non-atomic modify still round-trips (the mock simulates cancel+replace).
    let non_atomic =
        MockBrokerExecutor::accept_all().with_bracket_capabilities(BracketCapabilities {
            native_bo: false,
            emulated_multileg: true,
            atomic_modify: false,
        });
    let ticket = non_atomic.submit_bracket(spec()).await.unwrap();
    let update = non_atomic
        .modify_stop(&ticket.bracket_id, price("96.50"), None)
        .await
        .unwrap();
    assert_eq!(update.stop, price("96.50"));
    assert!(!non_atomic.bracket_capabilities().atomic_modify);
}

#[test]
fn bracket_capabilities_defaults_and_none() {
    let default = BracketCapabilities::default();
    assert!(!default.native_bo);
    assert!(default.emulated_multileg);
    assert!(default.atomic_modify);

    let none = BracketCapabilities::none();
    assert!(!none.native_bo && !none.emulated_multileg && !none.atomic_modify);
}

/// An adapter that doesn't implement the bracket contract inherits the trait
/// defaults: `bracket_capabilities()` reports none and `submit_bracket` fails.
#[tokio::test]
async fn unadopted_adapter_inherits_no_bracket_capabilities() {
    struct NoBrackets;
    #[async_trait::async_trait]
    impl BrokerExecutor for NoBrackets {
        fn name(&self) -> &str {
            "no-brackets"
        }
        async fn place_order(
            &self,
            _req: tantra_core::OrderRequest,
        ) -> Result<tantra_core::OrderAck, BrokerError> {
            unreachable!("not used in this test")
        }
        async fn cancel_order(&self, _order_id: &str) -> Result<(), BrokerError> {
            unreachable!("not used in this test")
        }
        async fn get_order_status(
            &self,
            _order_id: &str,
        ) -> Result<tantra_core::OrderStatus, BrokerError> {
            unreachable!("not used in this test")
        }
    }

    let broker = NoBrackets;
    assert_eq!(broker.bracket_capabilities(), BracketCapabilities::none());
    let err = broker.submit_bracket(spec()).await.unwrap_err();
    assert!(matches!(err, BrokerError::InvalidRequest(_)));
}
