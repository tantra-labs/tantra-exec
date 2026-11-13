//! Wire-token → canonical-enum mapping table tests.
//!
//! Every mapped Angel One token decodes to the documented canonical enum and
//! unknown tokens fall back to the documented default (PendingNew for status,
//! Buy for side, Nse for exchange, Market for order type).

use tantra_core::{Exchange, OrderSide, OrderStatus, OrderType, Price};
use tantra_exec_angelone::mapping;
use tantra_exec_core::types::ProductType;

#[test]
fn order_status_tokens() {
    assert_eq!(mapping::order_status("COMPLETE"), OrderStatus::Filled);
    assert_eq!(mapping::order_status("complete"), OrderStatus::Filled);
    assert_eq!(mapping::order_status("FILLED"), OrderStatus::Filled);
    assert_eq!(mapping::order_status("OPEN"), OrderStatus::New);
    assert_eq!(mapping::order_status("NEW"), OrderStatus::New);
    assert_eq!(mapping::order_status("PENDING"), OrderStatus::PendingNew);
    assert_eq!(
        mapping::order_status("TRIGGER PENDING"),
        OrderStatus::PendingNew
    );
    assert_eq!(mapping::order_status("CANCELLED"), OrderStatus::Cancelled);
    assert_eq!(mapping::order_status("CANCELED"), OrderStatus::Cancelled);
    assert_eq!(mapping::order_status("REJECTED"), OrderStatus::Rejected);
    // Unknown / transitional statuses fall back to PendingNew.
    assert_eq!(mapping::order_status("BOGUS"), OrderStatus::PendingNew);
    assert_eq!(mapping::order_status(""), OrderStatus::PendingNew);
}

#[test]
fn order_side_tokens() {
    assert_eq!(mapping::order_side("BUY"), OrderSide::Buy);
    assert_eq!(mapping::order_side("SELL"), OrderSide::Sell);
    // Unknown transaction types default to Buy (previous match-chain behavior).
    assert_eq!(mapping::order_side("BOGUS"), OrderSide::Buy);
    assert_eq!(mapping::order_side(""), OrderSide::Buy);
}

#[test]
fn exchange_tokens() {
    assert_eq!(mapping::exchange("NSE"), Exchange::Nse);
    assert_eq!(mapping::exchange("BSE"), Exchange::Bse);
    assert_eq!(mapping::exchange("NFO"), Exchange::Nfo);
    assert_eq!(mapping::exchange("MCX"), Exchange::Mcx);
    assert_eq!(mapping::exchange("CDS"), Exchange::Cds);
    assert_eq!(mapping::exchange("BOGUS"), Exchange::Nse);
}

#[test]
fn order_type_tokens_with_exact_prices() {
    let limit = mapping::order_type("LIMIT", Some(serde_json::json!("1450.50")), None);
    assert_eq!(
        limit,
        OrderType::Limit(Price::from_rupees("1450.50").unwrap())
    );

    let stop_market =
        mapping::order_type("STOPLOSS_MARKET", None, Some(serde_json::json!("1440.25")));
    assert_eq!(
        stop_market,
        OrderType::StopLoss(Price::from_rupees("1440.25").unwrap())
    );

    let stop_limit = mapping::order_type(
        "STOPLOSS_LIMIT",
        Some(serde_json::json!("1451.00")),
        Some(serde_json::json!("1440.25")),
    );
    assert_eq!(
        stop_limit,
        OrderType::StopLimit {
            trigger: Price::from_rupees("1440.25").unwrap(),
            limit: Price::from_rupees("1451.00").unwrap(),
        }
    );

    assert_eq!(mapping::order_type("MARKET", None, None), OrderType::Market);
    assert_eq!(mapping::order_type("BOGUS", None, None), OrderType::Market);
}

#[test]
fn product_type_round_trip() {
    for (token, product) in [
        ("DELIVERY", ProductType::CncDelivery),
        ("INTRADAY", ProductType::MisIntraday),
        ("CARRYFORWARD", ProductType::NrmlDerivative),
        ("MTF", ProductType::MtfMargin),
    ] {
        assert_eq!(ProductType::from_angelone(token), Some(product));
        assert_eq!(product.angelone(), token);
    }
    assert_eq!(ProductType::from_angelone("BOGUS"), None);
}
