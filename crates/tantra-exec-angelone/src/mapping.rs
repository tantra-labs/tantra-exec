//! Angel One SmartAPI wire-token → canonical-enum mapping tables.
//!
//! Table-driven replacement for ad-hoc match chains: every SmartAPI token an
//! adapter can meet (REST order book, REST positions and the SmartStream order
//! feed) is declared once here, as data, and both the REST and WebSocket paths
//! look the tables up through the small functions below.
//!
//! Lookups are ASCII case-insensitive — Angel One emits both `COMPLETE` and
//! `complete` in the wild.

use serde_json::Value;
use tantra_core::{Exchange, OrderSide, OrderStatus, OrderType, Price};
use tantra_exec_core::wire;
use tantra_exec_core::wiring::{BrokerEnumMap, OrderTypeKind};

/// Angel One order-status tokens → canonical [`OrderStatus`].
///
/// The REST order book (`pending`, `open`, `trigger pending`, `complete`,
/// `cancelled`, `rejected`) and the SmartStream feed share the same
/// vocabulary; unmapped/transitional statuses fall back to
/// [`OrderStatus::PendingNew`].
pub const ANGEL_ORDER_STATUS: BrokerEnumMap<OrderStatus> = BrokerEnumMap::new(&[
    ("COMPLETE", OrderStatus::Filled),
    ("FILLED", OrderStatus::Filled),
    ("OPEN", OrderStatus::New),
    ("NEW", OrderStatus::New),
    ("PENDING", OrderStatus::PendingNew),
    ("TRIGGER PENDING", OrderStatus::PendingNew),
    ("PUT ORDER REQ RECEIVED", OrderStatus::PendingNew),
    ("CANCELLED", OrderStatus::Cancelled),
    ("CANCELED", OrderStatus::Cancelled),
    ("REJECTED", OrderStatus::Rejected),
]);

/// Translate an Angel One order-status token into the canonical status.
pub fn order_status(status: &str) -> OrderStatus {
    ANGEL_ORDER_STATUS
        .get(status)
        .unwrap_or(OrderStatus::PendingNew)
}

/// Angel One `transactiontype` tokens → canonical [`OrderSide`].
pub const ANGEL_ORDER_SIDE: BrokerEnumMap<OrderSide> =
    BrokerEnumMap::new(&[("BUY", OrderSide::Buy), ("SELL", OrderSide::Sell)]);

/// Translate an Angel One `transactiontype` token into the canonical side.
///
/// Unmapped tokens default to [`OrderSide::Buy`], matching the previous
/// match-chain behavior.
pub fn order_side(transaction_type: &str) -> OrderSide {
    ANGEL_ORDER_SIDE
        .get(transaction_type)
        .unwrap_or(OrderSide::Buy)
}

/// Angel One exchange-segment tokens → canonical [`Exchange`].
///
/// SmartAPI reports exchanges with bare segment codes (`NSE`, `BSE`, `NFO`,
/// `MCX`, `CDS`); every documented segment is enumerated and unknown segments
/// fall back to [`Exchange::Nse`].
pub const ANGEL_EXCHANGE: BrokerEnumMap<Exchange> = BrokerEnumMap::new(&[
    ("NSE", Exchange::Nse),
    ("BSE", Exchange::Bse),
    ("NFO", Exchange::Nfo),
    ("MCX", Exchange::Mcx),
    ("CDS", Exchange::Cds),
]);

/// Translate an Angel One exchange-segment token into the canonical exchange
/// (default NSE).
pub fn exchange(exchange: &str) -> Exchange {
    ANGEL_EXCHANGE.get(exchange).unwrap_or(Exchange::Nse)
}

/// Angel One order-type tokens → order-type *shape*.
///
/// Angel One's limit/trigger prices ride on the wire record next to the token,
/// so the table maps to an [`OrderTypeKind`] discriminant and [`order_type`]
/// applies the decoded prices.
pub const ANGEL_ORDER_TYPE: BrokerEnumMap<OrderTypeKind> = BrokerEnumMap::new(&[
    ("MARKET", OrderTypeKind::Market),
    ("LIMIT", OrderTypeKind::Limit),
    ("STOPLOSS_MARKET", OrderTypeKind::StopLoss),
    ("STOPLOSS", OrderTypeKind::StopLoss),
    ("STOPLOSS_LIMIT", OrderTypeKind::StopLimit),
]);

/// Translate the Angel One order-type token + price/trigger trio into
/// [`OrderType`].
///
/// Unmapped order types default to [`OrderType::Market`], matching the
/// previous match-chain behavior.
pub fn order_type(order_type: &str, price: Option<Value>, trigger: Option<Value>) -> OrderType {
    let price = price
        .and_then(|v| wire::price_from_json(&v))
        .unwrap_or(Price::ZERO);
    let trigger = trigger
        .and_then(|v| wire::price_from_json(&v))
        .unwrap_or(Price::ZERO);
    ANGEL_ORDER_TYPE
        .get(order_type)
        .unwrap_or(OrderTypeKind::Market)
        .into_order_type(price, trigger)
}
