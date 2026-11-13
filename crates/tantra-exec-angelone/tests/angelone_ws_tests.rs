//! SmartStream order-feed tests: feed JSON frames through the channel and
//! assert the emitted [`BrokerExecutionEvent`]s.
//!
//! The stream is driven entirely offline through `AngelOneOrderStream::new`
//! — no socket required.

use std::collections::HashMap;

use tokio::sync::mpsc;
use uuid::Uuid;

use tantra_core::{Exchange, OrderSide, Price};
use tantra_exec_angelone::ws::{AngelOneOrderStream, parse_order_feed_frame};
use tantra_exec_core::{BrokerExecutionEvent, ExecutionStream};

/// Build an executor-style stream fed by an unbounded frame channel.
fn stream() -> (mpsc::UnboundedSender<String>, AngelOneOrderStream) {
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    (tx, AngelOneOrderStream::new(rx))
}

fn open_frame(order_id: &str, status: &str) -> String {
    serde_json::json!({
        "orderid": order_id,
        "tradingsymbol": "RELIANCE",
        "symboltoken": "2885",
        "exch": "NSE",
        "transactiontype": "BUY",
        "ordertype": "LIMIT",
        "producttype": "DELIVERY",
        "quantity": "25",
        "price": "1450.50",
        "triggerprice": "0.00",
        "filledqty": "0",
        "status": status,
        "updatetime": "2024-02-21T09:20:00 IST",
        "ordertag": "ALGO-1"
    })
    .to_string()
}

fn fill_frame(order_id: &str, status: &str, filled: &str) -> String {
    serde_json::json!({
        "orderid": order_id,
        "tradingsymbol": "RELIANCE",
        "symboltoken": "2885",
        "exch": "NSE",
        "transactiontype": "BUY",
        "ordertype": "LIMIT",
        "producttype": "DELIVERY",
        "quantity": "25",
        "price": "1450.50",
        "averageprice": "1450.50",
        "filledqty": filled,
        "status": status,
        "ordergenerationtime": "2024-02-21T09:20:05 IST",
        "ordertag": "ALGO-1"
    })
    .to_string()
}

#[tokio::test]
async fn open_frame_emits_order_ack() {
    let (tx, mut stream) = stream();
    tx.send(open_frame("110000001", "open")).unwrap();

    match stream.next_event().await.unwrap() {
        BrokerExecutionEvent::OrderAck(ack) => {
            assert_eq!(ack.order_id, "110000001");
            assert_eq!(ack.status, tantra_core::OrderStatus::New);
        }
        other => panic!("expected OrderAck, got {other:?}"),
    }
}

#[tokio::test]
async fn complete_frame_emits_fill_with_exact_fields() {
    let (tx, mut stream) = stream();
    tx.send(fill_frame("110000001", "complete", "25")).unwrap();

    match stream.next_event().await.unwrap() {
        BrokerExecutionEvent::Fill(fill) => {
            assert_eq!(fill.fill_id, "110000001:25");
            assert_eq!(fill.order_id, "110000001");
            assert_eq!(fill.instrument.symbol, "RELIANCE");
            assert_eq!(fill.instrument.exchange, Exchange::Nse);
            assert_eq!(fill.side, OrderSide::Buy);
            assert_eq!(fill.price.0, Price::from_rupees("1450.50").unwrap().0);
            assert_eq!(fill.quantity.0, rust_decimal::Decimal::from(25));
            assert_eq!(fill.algo_id, "ALGO-1");
        }
        other => panic!("expected Fill, got {other:?}"),
    }
}

#[tokio::test]
async fn partial_then_complete_emits_two_delta_fills() {
    let (tx, mut stream) = stream();
    tx.send(fill_frame("110000001", "partial fill", "10"))
        .unwrap();
    tx.send(fill_frame("110000001", "complete", "25")).unwrap();

    let first = stream.next_event().await.unwrap();
    let second = stream.next_event().await.unwrap();
    match (first, second) {
        (BrokerExecutionEvent::Fill(first), BrokerExecutionEvent::Fill(second)) => {
            assert_eq!(first.fill_id, "110000001:10");
            assert_eq!(first.quantity.0, rust_decimal::Decimal::from(10));
            assert_eq!(second.fill_id, "110000001:25");
            assert_eq!(second.quantity.0, rust_decimal::Decimal::from(15));
        }
        (a, b) => panic!("expected two fills, got {a:?} and {b:?}"),
    }
}

#[tokio::test]
async fn duplicate_fill_frame_does_not_emit() {
    let (tx, mut stream) = stream();
    tx.send(fill_frame("110000001", "complete", "25")).unwrap();
    tx.send(fill_frame("110000001", "complete", "25")).unwrap();

    let first = stream.next_event().await.unwrap();
    assert!(matches!(first, BrokerExecutionEvent::Fill(_)));
    // No delta → the second frame must produce no event; prove it by sending
    // a cancelled frame afterwards and asserting it arrives next.
    tx.send(
        serde_json::json!({
            "orderid": "110000001",
            "status": "cancelled"
        })
        .to_string(),
    )
    .unwrap();
    let next = stream.next_event().await.unwrap();
    assert!(matches!(next, BrokerExecutionEvent::OrderCancelled { .. }));
}

#[tokio::test]
async fn cancelled_frame_emits_order_cancelled() {
    let (tx, mut stream) = stream();
    tx.send(
        serde_json::json!({
            "orderid": "110000001",
            "status": "cancelled",
            "statusmessage": "Order cancelled by user"
        })
        .to_string(),
    )
    .unwrap();

    match stream.next_event().await.unwrap() {
        BrokerExecutionEvent::OrderCancelled { order_id, reason } => {
            assert_eq!(order_id, "110000001");
            assert_eq!(reason.as_deref(), Some("Order cancelled by user"));
        }
        other => panic!("expected OrderCancelled, got {other:?}"),
    }
}

#[tokio::test]
async fn rejected_frame_emits_order_rejected() {
    let (tx, mut stream) = stream();
    tx.send(
        serde_json::json!({
            "orderid": "110000001",
            "status": "rejected",
            "statusmessage": "Insufficient margin"
        })
        .to_string(),
    )
    .unwrap();

    match stream.next_event().await.unwrap() {
        BrokerExecutionEvent::OrderRejected { order_id, reason } => {
            assert_eq!(order_id.as_deref(), Some("110000001"));
            assert_eq!(reason, "Insufficient margin");
        }
        other => panic!("expected OrderRejected, got {other:?}"),
    }
}

#[tokio::test]
async fn correlation_lookup_is_applied() {
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    let client_id = Uuid::new_v4();
    let mut lookup: HashMap<String, Uuid> = HashMap::new();
    lookup.insert("110000001".to_string(), client_id);
    let mut stream = AngelOneOrderStream::new(rx)
        .with_correlation_lookup(move |order_id| lookup.get(order_id).copied());
    tx.send(open_frame("110000001", "open")).unwrap();

    match stream.next_event().await.unwrap() {
        BrokerExecutionEvent::OrderAck(ack) => assert_eq!(ack.client_order_id, client_id),
        other => panic!("expected OrderAck, got {other:?}"),
    }
}

#[tokio::test]
async fn empty_and_malformed_frames() {
    let (tx, mut stream) = stream();
    tx.send(String::new()).unwrap();
    tx.send(serde_json::json!([open_frame_as_object("110000002", "open")]).to_string())
        .unwrap();

    // Empty frame yields no event; the next frame's ack must arrive.
    match stream.next_event().await.unwrap() {
        BrokerExecutionEvent::OrderAck(ack) => assert_eq!(ack.order_id, "110000002"),
        other => panic!("expected OrderAck, got {other:?}"),
    }

    // Malformed JSON fails the stream.
    tx.send("not json {{{".to_string()).unwrap();
    let err = stream.next_event().await.unwrap_err();
    assert!(matches!(
        err,
        tantra_exec_core::BrokerError::InvalidRequest(_)
    ));
}

#[tokio::test]
async fn parse_frame_handles_object_array_and_empty() {
    assert!(parse_order_feed_frame("").unwrap().is_empty());
    assert_eq!(parse_order_feed_frame("  ").unwrap().len(), 0);
    assert_eq!(
        parse_order_feed_frame(&open_frame("1", "open"))
            .unwrap()
            .len(),
        1
    );
    let array =
        serde_json::json!([
            serde_json::from_str::<serde_json::Value>(&open_frame("1", "open")).unwrap()
        ])
        .to_string();
    assert_eq!(parse_order_feed_frame(&array).unwrap().len(), 1);
    assert!(parse_order_feed_frame("42").is_err());
}

fn open_frame_as_object(order_id: &str, status: &str) -> serde_json::Value {
    serde_json::json!({
        "orderid": order_id,
        "tradingsymbol": "RELIANCE",
        "symboltoken": "2885",
        "exch": "NSE",
        "transactiontype": "BUY",
        "ordertype": "LIMIT",
        "producttype": "DELIVERY",
        "quantity": "25",
        "filledqty": "0",
        "status": status,
        "ordertag": "ALGO-1"
    })
}
