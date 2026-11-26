//! Async `PaperExchangeExecutor` (`BrokerExecutor` wire port) tests.

use chrono::Utc;
use rust_decimal_macros::dec;
use tantra_core::{
    Exchange, InstrumentId, OrderRequest, OrderSide, OrderStatus, OrderType, Price, Quantity, Tick,
};
use tantra_exec_core::{BrokerError, BrokerExecutor, PositionSide};
use tantra_exec_paper::{PaperExchange, PaperExchangeExecutor};
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

/// Seed the exchange with real bid/ask feed depth via a Quote tick.
fn seeded_exchange() -> PaperExchange {
    let mut ex = PaperExchange::new(dec!(1_000_000));
    let t = Utc::now();
    ex.feed_tick(&Tick::Quote {
        instrument: inst(),
        bid: Price::try_new(dec!(100)).unwrap(),
        ask: Price::try_new(dec!(105)).unwrap(),
        bid_qty: Quantity::from_u64(50),
        ask_qty: Quantity::from_u64(50),
        last_price: Some(Price::try_new(dec!(100)).unwrap()),
        exchange_ts: t,
        receive_ts: t,
        band: None,
    });
    ex
}

#[tokio::test]
async fn place_market_order_then_inspect_positions_margins_health() {
    let exec = PaperExchangeExecutor::new(seeded_exchange());

    let req = order(OrderSide::Sell, OrderType::Market, 50, 7);
    let ack = exec.place_order(req).await.unwrap();
    assert_eq!(ack.order_id, "PAPER-0001");
    assert_eq!(ack.status, OrderStatus::New);
    assert_eq!(ack.exchange_order_id.as_deref(), Some("EX-PAPER-0001"));

    // Sold 50 @ 100 against the seeded bid depth → filled.
    assert_eq!(
        exec.get_order_status(&ack.order_id).await.unwrap(),
        OrderStatus::Filled
    );

    let positions = exec.fetch_positions().await.unwrap();
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].instrument, inst());
    assert_eq!(positions[0].side, PositionSide::Short);
    assert_eq!(positions[0].quantity, Quantity::from_u64(50));

    // Cash: 1_000_000 + 50 * 100 = 1_005_000 (zero fees).
    let margins = exec.fetch_margins().await.unwrap();
    assert_eq!(margins.available_cash.amount(), dec!(1_005_000));
    assert_eq!(margins.used_margin.amount(), dec!(0));

    let health = exec.health_check().await.unwrap();
    assert_eq!(health.broker, "paper");
    assert!(health.session_valid);

    // The fill shows up in the paper exchange itself.
    let exchange = exec.exchange();
    let guard = exchange.lock().unwrap();
    assert_eq!(guard.fills().len(), 1);
}

#[tokio::test]
async fn cancel_path_and_terminal_errors() {
    let exec = PaperExchangeExecutor::new(PaperExchange::new(dec!(1_000_000)));

    // A resting buy limit: New while working.
    let req = order(
        OrderSide::Buy,
        OrderType::Limit(Price::try_new(dec!(90)).unwrap()),
        10,
        8,
    );
    let ack = exec.place_order(req).await.unwrap();
    assert_eq!(
        exec.get_order_status(&ack.order_id).await.unwrap(),
        OrderStatus::New
    );

    // Cancel → Cancelled; a second cancel is a terminal-state error.
    exec.cancel_order(&ack.order_id).await.unwrap();
    assert_eq!(
        exec.get_order_status(&ack.order_id).await.unwrap(),
        OrderStatus::Cancelled
    );
    assert!(matches!(
        exec.cancel_order(&ack.order_id).await,
        Err(BrokerError::Rejected(_))
    ));

    // Unknown ids surface as OrderNotFound.
    assert!(matches!(
        exec.get_order_status("PAPER-9999").await,
        Err(BrokerError::OrderNotFound(_))
    ));
    assert!(matches!(
        exec.cancel_order("PAPER-9999").await,
        Err(BrokerError::OrderNotFound(_))
    ));
}

#[tokio::test]
async fn modify_order_updates_and_acknowledges() {
    let exec = PaperExchangeExecutor::new(PaperExchange::new(dec!(1_000_000)));
    let req = order(
        OrderSide::Buy,
        OrderType::Limit(Price::try_new(dec!(90)).unwrap()),
        10,
        9,
    );
    let ack = exec.place_order(req.clone()).await.unwrap();

    let amended = exec
        .modify_order(&ack.order_id, Some(Price::try_new(dec!(95)).unwrap()), None)
        .await
        .unwrap();
    assert_eq!(amended.order_id, ack.order_id);
    assert_eq!(amended.status, OrderStatus::New);
    assert_eq!(amended.client_order_id, req.client_order_id);

    // The exchange order book reflects the amended limit price.
    let book = exec.fetch_order_book().await.unwrap();
    assert_eq!(book.len(), 1);
    assert_eq!(book[0].price, Some(Price::try_new(dec!(95)).unwrap()));
    assert_eq!(book[0].status, OrderStatus::New);
}
