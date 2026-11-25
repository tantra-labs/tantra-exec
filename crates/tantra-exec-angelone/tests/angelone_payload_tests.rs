//! `placeOrder` payload tests: exact 2-scale decimal strings on the wire,
//! idempotent wire `OrderTag` encoding, and no floating point anywhere.
//!
//! Prices / triggers are asserted as exact strings (e.g. `"1450.50"`), the
//! quantity is a u32, and the `ordertype` / `transactiontype` / `producttype`
//! / `duration` / `symboltoken` / `tag` legs follow the SmartAPI contract.

use uuid::Uuid;

use tantra_core::{
    Exchange, InstrumentId, OrderRequest, OrderSide, OrderType, Price, Quantity, TimeInForce,
};
use tantra_exec_angelone::build_order_payload;
use tantra_exec_core::tag::{ANGELONE_TAG_MAX_LEN, DecodedOrderTag, OrderTag};
use tantra_exec_core::types::ProductType;

fn request(order_type: OrderType) -> OrderRequest {
    let instrument = InstrumentId::try_new(Exchange::Nse, "RELIANCE")
        .unwrap()
        .with_token("2885".to_string());
    OrderRequest::new(
        instrument,
        OrderSide::Buy,
        order_type,
        Quantity::from_u64(25),
        "ALGO-1".to_string(),
    )
}

#[test]
fn market_order_payload() {
    let cid = Uuid::parse_str("01234567-89ab-4cde-8123-456789abcdef").unwrap();
    let instrument = InstrumentId::try_new(Exchange::Nse, "RELIANCE")
        .unwrap()
        .with_token("2885".to_string());
    let req = OrderRequest::with_id(
        cid,
        instrument,
        OrderSide::Buy,
        OrderType::Market,
        Quantity::from_u64(25),
        "ALGO-1",
    );
    let payload = build_order_payload(&req, ProductType::CncDelivery).unwrap();
    assert_eq!(payload["variety"], "NORMAL");
    assert_eq!(payload["tradingsymbol"], "RELIANCE");
    assert_eq!(payload["symboltoken"], "2885");
    assert_eq!(payload["exchange"], "NSE");
    assert_eq!(payload["transactiontype"], "BUY");
    assert_eq!(payload["ordertype"], "MARKET");
    assert_eq!(payload["producttype"], "DELIVERY");
    assert_eq!(payload["quantity"], 25);
    assert_eq!(payload["price"], "0.00");
    assert_eq!(payload["triggerprice"], "0.00");
    assert_eq!(payload["disclosedquantity"], "0");
    assert_eq!(payload["duration"], "DAY");

    // Golden vector: 20-char compact Base32 correlation tag from client_order_id.
    let expected_tag = OrderTag::from_request(&req).wire_tag_20();
    assert_eq!(expected_tag, "AERUKZ4JVNGN5AJDIVTQ");
    let wire_tag = payload["tag"].as_str().unwrap();
    assert_eq!(wire_tag, expected_tag);
    assert!(wire_tag.len() <= ANGELONE_TAG_MAX_LEN);
    assert!(DecodedOrderTag::decode(wire_tag).unwrap().matches(cid));
}

#[test]
fn limit_order_payload_exact_price() {
    let order = request(OrderType::Limit(Price::from_rupees("1450.50").unwrap()));
    let payload = build_order_payload(&order, ProductType::CncDelivery).unwrap();
    assert_eq!(payload["ordertype"], "LIMIT");
    // Exact 2-scale decimal string — never a float.
    assert_eq!(payload["price"], "1450.50");
    assert_eq!(payload["triggerprice"], "0.00");
}

#[test]
fn stop_loss_payload_exact_trigger() {
    let order = request(OrderType::StopLoss(Price::from_rupees("1440.25").unwrap()));
    let payload = build_order_payload(&order, ProductType::MisIntraday).unwrap();
    assert_eq!(payload["ordertype"], "STOPLOSS_MARKET");
    assert_eq!(payload["price"], "0.00");
    assert_eq!(payload["triggerprice"], "1440.25");
    assert_eq!(payload["producttype"], "INTRADAY");
}

#[test]
fn stop_limit_payload_exact_trigger_and_limit() {
    let order = request(OrderType::StopLimit {
        trigger: Price::from_rupees("1440.25").unwrap(),
        limit: Price::from_rupees("1451.00").unwrap(),
    });
    let payload = build_order_payload(&order, ProductType::CncDelivery).unwrap();
    assert_eq!(payload["ordertype"], "STOPLOSS_LIMIT");
    assert_eq!(payload["price"], "1451.00");
    assert_eq!(payload["triggerprice"], "1440.25");
}

#[test]
fn sell_side_and_ioc_duration() {
    let mut order = request(OrderType::Limit(Price::from_rupees("100.00").unwrap()));
    order.side = OrderSide::Sell;
    order.tif = TimeInForce::Ioc;
    let payload = build_order_payload(&order, ProductType::MtfMargin).unwrap();
    assert_eq!(payload["transactiontype"], "SELL");
    assert_eq!(payload["duration"], "IOC");
    assert_eq!(payload["producttype"], "MTF");
}

#[test]
fn symboltoken_falls_back_to_symbol_and_always_tags_client_order_id() {
    let instrument = InstrumentId::try_new(Exchange::Nfo, "NIFTY24DEC24000CE").unwrap();
    let order = OrderRequest::new(
        instrument,
        OrderSide::Buy,
        OrderType::Market,
        Quantity::from_u64(50),
        String::new(),
    );
    let cid = order.client_order_id;
    let payload = build_order_payload(&order, ProductType::CncDelivery).unwrap();
    assert_eq!(payload["symboltoken"], "NIFTY24DEC24000CE");
    // Empty algo_id still carries client_order_id tag for idempotent reconciliation.
    let wire_tag = payload.get("tag").and_then(|v| v.as_str()).unwrap();
    assert!(DecodedOrderTag::decode(wire_tag).unwrap().matches(cid));
}
