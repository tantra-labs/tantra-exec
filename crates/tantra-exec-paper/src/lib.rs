//! # व्यापार・紙 tantra-exec-paper
//!
//! Paper trading exchange matching engine (P0 of
//! [`docs/plans/0003`](../../docs/plans/0003-paper-exchange-matching-core.md)):
//! a deterministic, zero-I/O venue with an L2 order book, full order
//! lifecycle, per-account cash/positions, and an immutable event stream.
//!
//! No wall clock is ever read inside the matching engine — every state
//! transition takes an explicit `DateTime<Utc>` — and all prices,
//! quantities, and cash are exact `rust_decimal::Decimal` values wrapped in
//! the canonical [`tantra_core`] value objects (zero floats, `AGENTS.md` §2.3).
//!
//! # Module map
//! - [`book`] — ported L2 order book (price-time priority, market/limit
//!   execution, participation caps)
//! - [`latency`] — ported seeded network/broker latency models
//! - [`mis`] — ported MIS 15:20 square-off manager
//! - [`slippage`] — ported volatility/adverse-fill slippage models
//! - [`lifecycle`] — order state machine, event stream, error taxonomy
//! - [`paper`] — the [`PaperExchange`] aggregate (account + books + matching)
//! - [`executor`] — [`PaperExchangeExecutor`] (`BrokerExecutor` impl + sync port)
//!
//! # Determinism
//! Feeding the same tape (orders, prices, timestamps, seeds) into two fresh
//! exchanges produces byte-identical `fills()` and `events()` streams
//! (E7 golden-replay property).

pub mod book;
pub mod executor;
pub mod latency;
pub mod lifecycle;
pub mod mis;
pub mod paper;
pub mod slippage;

pub use book::{OrderBook, OrderBookLevel, OrderBookSide};
pub use executor::PaperExchangeExecutor;
pub use latency::{MisSquareOffLatency, NetworkLatency, SeededRng};
pub use lifecycle::{ExchangeError, ExchangeEvent, OrderState, WorkingOrder};
pub use mis::{MisSquareOffManager, SquareOffResult};
pub use paper::{AccountState, FillCostFn, PaperExchange, ZeroFillCost};
pub use slippage::{AdverseFillModel, SlippageModel};

// Canonical shared-kernel re-exports (unified strictly on tantra-core).
pub use tantra_core::{
    Bar, DepthLevel, FillReport, InstrumentId, Money, OrderAck, OrderRequest, OrderSide,
    OrderStatus, OrderType, Price, Quantity, Tick, TimeInForce,
};
