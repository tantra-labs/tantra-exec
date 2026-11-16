//! Paper-exchange order lifecycle: state machine, resting order record,
//! immutable event stream, and error taxonomy.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use tantra_core::{FillReport, InstrumentId, OrderSide, OrderType, Price, Quantity, TimeInForce};

/// Lifecycle state of a single order inside the paper exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderState {
    /// Stop order armed, awaiting its trigger price to be breached.
    New,
    /// Resting on the book (limit) or waiting, marketable only on trigger.
    Working,
    /// Partially filled, still active.
    PartiallyFilled,
    /// Completely filled.
    Filled,
    /// Cancelled by the user or by time-in-force expiry.
    Cancelled,
    /// Rejected at submission (validation failure).
    Rejected,
    /// Stop order whose trigger was breached; now executes as Market/Limit.
    StopTriggered,
}

/// A resting (or just-submitted) order tracked by the exchange.
#[derive(Debug, Clone)]
pub struct WorkingOrder {
    /// Broker/venue order ID (`PAPER-0001`).
    pub broker_order_id: String,
    /// Client correlation UUID.
    pub client_order_id: Uuid,
    /// Traded instrument.
    pub instrument: InstrumentId,
    /// Order direction.
    pub side: OrderSide,
    /// Pricing instructions (mutated on stop trigger / amend).
    pub order_type: OrderType,
    /// Time in force.
    pub tif: TimeInForce,
    /// Original order quantity.
    pub original_qty: Quantity,
    /// Quantity still outstanding.
    pub remaining_qty: Quantity,
    /// Quantity filled so far.
    pub filled_qty: Quantity,
    /// Volume-weighted average fill price.
    pub avg_fill_price: Price,
    /// Lifecycle state.
    pub state: OrderState,
    /// When the order becomes ready to execute (latency model), if any.
    pub ready_ts: Option<DateTime<Utc>>,
    /// Absolute expiry (GTD), if any.
    pub expires_ts: Option<DateTime<Utc>>,
    /// Rejection reason, if the order was rejected.
    pub reject_reason: Option<String>,
    /// Caller-supplied submission timestamp (deterministic; no wall clock).
    pub submitted_ts: DateTime<Utc>,
}

impl WorkingOrder {
    /// Returns `true` when the order is active and can be matched/cancelled.
    pub fn is_active(&self) -> bool {
        matches!(
            self.state,
            OrderState::New | OrderState::Working | OrderState::PartiallyFilled
        )
    }

    /// Returns `true` when the order is resting on the book and may be
    /// matched. Armed stops (`New`) are deliberately excluded — they only
    /// match after their trigger price is breached.
    pub fn is_resting(&self) -> bool {
        matches!(
            self.state,
            OrderState::Working | OrderState::PartiallyFilled
        )
    }
}

/// Immutable, timestamped event emitted on every exchange state transition.
///
/// No wall clock is ever read inside the exchange: every event carries the
/// caller-supplied `DateTime<Utc>`, so replaying an identical tape yields
/// byte-identical event streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExchangeEvent {
    /// Order accepted into the exchange.
    OrderAccepted {
        broker_order_id: String,
        instrument: InstrumentId,
        side: OrderSide,
        quantity: Quantity,
        timestamp: DateTime<Utc>,
    },
    /// Order fully filled.
    Fill { fill: FillReport },
    /// Order partially filled (still active).
    PartialFill { fill: FillReport },
    /// Order cancelled.
    Cancelled {
        broker_order_id: String,
        timestamp: DateTime<Utc>,
    },
    /// Order rejected at submission.
    Rejected {
        broker_order_id: String,
        reason: String,
        timestamp: DateTime<Utc>,
    },
    /// Stop order's trigger price was breached.
    StopTriggered {
        broker_order_id: String,
        timestamp: DateTime<Utc>,
    },
}

/// Errors raised by the paper exchange.
#[derive(Debug, thiserror::Error)]
pub enum ExchangeError {
    /// A market order had no contra liquidity to trade against.
    #[error("no market price available for market order on {0}")]
    NoMarketPrice(InstrumentId),
    /// Referenced order is not tracked by the exchange.
    #[error("order not found: {0}")]
    OrderNotFound(String),
    /// The request itself is invalid.
    #[error("invalid order: {0}")]
    Invalid(String),
    /// The operation is not permitted on a terminal-state order.
    #[error("order in terminal state: {0}")]
    Terminal(String),
}
