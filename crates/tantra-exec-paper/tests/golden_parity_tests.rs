//! T14.14 P4 golden vector (root plan 0007): the canonical cross-embedding
//! parity test.
//!
//! `golden/paper_golden.json` is the single oracle for the paper-exchange
//! golden tape — byte-identical across the backtest E9 suite
//! (`tantra-runtime-backtest/tests/paper_parity_tests.rs`), this engine
//! (`tantra-exec-paper`), the Python wheel (`tantra-ffi`), and WASM
//! (`tantra-ui-api-wasm`). Replaying the tape through a fresh `PaperExchange`
//! must reproduce exactly the 4 golden fills, the order lifecycle states, the
//! zero-fee cash round trip, and the 7-event stream — deterministically on
//! replay.

use std::collections::HashMap;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;
use tantra_core::{Exchange, InstrumentId, OrderRequest, OrderSide, OrderType, Price, Quantity};
use tantra_exec_paper::{OrderState, PaperExchange, ZeroFillCost};
use uuid::Uuid;

const GOLDEN_JSON: &str = include_str!("golden/paper_golden.json");

#[derive(Debug, Deserialize)]
struct GoldenTape {
    instrument: String,
    initial_cash: String,
    market_price: String,
    ts0: String,
    orders: Vec<GoldenOrder>,
    expected: GoldenSummary,
}

#[derive(Debug, Deserialize)]
struct GoldenOrder {
    id: usize,
    side: String,
    order_type: String,
    quantity: String,
    limit_price: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoldenFill {
    side: String,
    price: String,
    quantity: String,
}

#[derive(Debug, Deserialize)]
struct GoldenSummary {
    fills: Vec<GoldenFill>,
    order_states: HashMap<String, String>,
    sell_filled_qty: String,
    sell_avg_fill_price: String,
    net_cash: String,
    event_count: usize,
}

fn decimal(s: &str) -> Decimal {
    Decimal::from_str(s).expect("valid decimal literal in golden tape")
}

fn side(s: &str) -> OrderSide {
    match s {
        "BUY" => OrderSide::Buy,
        "SELL" => OrderSide::Sell,
        other => panic!("unknown side {other:?} in golden tape"),
    }
}

/// Replay the canonical golden tape through a fresh `PaperExchange`, mapping
/// the tape's order labels (`buy101` / `buy100` / `sell`) to the broker order
/// ids returned by the engine.
fn run_tape(tape: &GoldenTape) -> (PaperExchange, HashMap<String, String>) {
    let inst = InstrumentId::new(Exchange::Nse, "RELIANCE");
    let ts0 = DateTime::parse_from_rfc3339(&tape.ts0)
        .expect("valid rfc3339 ts0 in golden tape")
        .with_timezone(&Utc);
    let mut ex =
        PaperExchange::new(decimal(&tape.initial_cash)).with_cost_fn(Box::new(ZeroFillCost));
    ex.set_market_price(inst.clone(), Price::new(decimal(&tape.market_price)));

    let mut ids = HashMap::new();
    for order in &tape.orders {
        let order_type = match order.order_type.as_str() {
            "LIMIT" => OrderType::Limit(Price::new(decimal(
                order.limit_price.as_ref().expect("limit price"),
            ))),
            "MARKET" => OrderType::Market,
            other => panic!("unknown order type {other:?} in golden tape"),
        };
        let req = OrderRequest::with_id(
            Uuid::from_u128(order.id as u128),
            inst.clone(),
            side(&order.side),
            order_type,
            Quantity::new(decimal(&order.quantity)),
            "paper_golden",
        );
        let id = ex.submit_order(req, ts0).unwrap();
        let label = match order.side.as_str() {
            "BUY" => format!("buy{}", order.limit_price.as_ref().expect("limit price")),
            "SELL" => "sell".to_string(),
            other => panic!("unknown side {other:?} in golden tape"),
        };
        ids.insert(label, id);
    }
    (ex, ids)
}

/// The engine's fills equal the golden vector, in order: sell 100@101,
/// buy@101 100@101, sell 50@100, buy@100 50@100 (one `FillReport` per side
/// per participant fill), each attributed to the originating order.
#[test]
fn golden_tape_fills_match_golden_values() {
    let tape: GoldenTape = serde_json::from_str(GOLDEN_JSON).expect("golden tape parses");
    assert_eq!(tape.instrument, "NSE:RELIANCE");
    let (ex, _) = run_tape(&tape);

    let fills = ex.fills();
    assert_eq!(fills.len(), tape.expected.fills.len());

    let sell_id = tape
        .orders
        .iter()
        .find(|o| o.side == "SELL")
        .map(|o| o.id as u128)
        .expect("golden tape has one market sell");
    let buy_id_by_price: HashMap<&str, u128> = tape
        .orders
        .iter()
        .filter(|o| o.side == "BUY")
        .map(|o| {
            (
                o.limit_price.as_ref().expect("limit price").as_str(),
                o.id as u128,
            )
        })
        .collect();

    for (idx, expected) in tape.expected.fills.iter().enumerate() {
        let fill = &fills[idx];
        assert_eq!(fill.price.0, decimal(&expected.price), "fill {idx} price");
        assert_eq!(
            fill.quantity.0,
            decimal(&expected.quantity),
            "fill {idx} quantity"
        );
        assert_eq!(fill.side, side(&expected.side), "fill {idx} side");
        let expected_id = if expected.side == "SELL" {
            sell_id
        } else {
            *buy_id_by_price
                .get(expected.price.as_str())
                .expect("buy limit for fill price")
        };
        assert_eq!(
            fill.client_order_id,
            Uuid::from_u128(expected_id),
            "fill {idx} order id"
        );
    }
}

/// Order lifecycle states and the zero-fee cash round trip.
#[test]
fn golden_tape_order_states_and_account() {
    let tape: GoldenTape = serde_json::from_str(GOLDEN_JSON).expect("golden tape parses");
    let (ex, ids) = run_tape(&tape);

    let golden_state = |label: &str| -> OrderState {
        match tape
            .expected
            .order_states
            .get(label)
            .expect("golden order state label")
            .as_str()
        {
            "FILLED" => OrderState::Filled,
            "PARTIALLY_FILLED" => OrderState::PartiallyFilled,
            other => panic!("unknown golden order state {other:?}"),
        }
    };
    assert_eq!(
        ex.order(ids.get("buy101").expect("buy101 id"))
            .unwrap()
            .state,
        golden_state("buy101")
    );
    assert_eq!(
        ex.order(ids.get("buy100").expect("buy100 id"))
            .unwrap()
            .state,
        golden_state("buy100")
    );
    let sell = ex.order(ids.get("sell").expect("sell id")).unwrap();
    assert_eq!(sell.state, golden_state("sell"));
    assert_eq!(sell.filled_qty.0, decimal(&tape.expected.sell_filled_qty));
    assert_eq!(
        sell.avg_fill_price.0,
        decimal(&tape.expected.sell_avg_fill_price)
    );

    // Zero-fee round trip: the sell nets the resting buys, so the net
    // position is flat and cash returns to the initial balance.
    let inst = InstrumentId::new(Exchange::Nse, "RELIANCE");
    assert_eq!(ex.positions().get(&inst), Some(&dec!(0)));
    assert_eq!(ex.cash(), decimal(&tape.expected.net_cash));
    assert_eq!(ex.cash(), decimal(&tape.initial_cash));

    // Event shape: 3 OrderAccepted + 4 fill events = 7.
    assert_eq!(ex.events().len(), tape.expected.event_count);
    assert_eq!(ex.events().len(), 7);
}

/// Determinism: replaying the same tape through two fresh exchanges produces
/// byte-identical events and fills.
#[test]
fn golden_tape_is_deterministic() {
    let tape: GoldenTape = serde_json::from_str(GOLDEN_JSON).expect("golden tape parses");
    let (a, _) = run_tape(&tape);
    let (b, _) = run_tape(&tape);
    assert_eq!(a.events(), b.events());
    assert_eq!(a.fills(), b.fills());
    assert_eq!(a.cash(), b.cash());
}

/// The golden summary is internally consistent with the canonical tape.
#[test]
fn golden_summary_is_internally_consistent() {
    let tape: GoldenTape = serde_json::from_str(GOLDEN_JSON).expect("golden tape parses");
    assert_eq!(tape.expected.fills.len(), 4);
    assert_eq!(tape.expected.event_count, 7);
    assert_eq!(decimal(&tape.expected.sell_filled_qty), dec!(150));
    assert_eq!(decimal(&tape.expected.sell_avg_fill_price), dec!(100.6667));
    assert_eq!(decimal(&tape.expected.net_cash), dec!(1_000_000));
    assert_eq!(
        tape.expected.order_states.get("buy101").map(String::as_str),
        Some("FILLED")
    );
    assert_eq!(
        tape.expected.order_states.get("buy100").map(String::as_str),
        Some("PARTIALLY_FILLED")
    );
    assert_eq!(
        tape.expected.order_states.get("sell").map(String::as_str),
        Some("FILLED")
    );
}
