//! # व्यापार・निष्पादन tantra-exec-core
//!
//! Canonical broker execution interfaces for the व्यापार (tantra-exec) subsystem:
//! exact-decimal domain types, the `BrokerExecutor` trait v2, push-based
//! [`ExecutionStream`], the canonical Indian [`ProductType`] taxonomy, and the
//! prioritized [`TokenBucketRateLimiter`] queue used to shield every broker
//! adapter from HTTP 429 bans.
//!
//! # Module map
//! - [`bracket`] — bracket-order (SL / multi-target / trailing-stop) contract & tracker
//! - [`types`] — [`ProductType`], broker health, order snapshots, positions, margins
//! - [`executor`] — [`BrokerExecutor`] trait v2, [`RiskGatedExecutor`], mock adapter
//! - [`stream`] — [`ExecutionStream`] push trait + channel transport
//! - [`ratelimit`] — prioritized token bucket + [`RateLimitedBroker`] wrapper
//! - [`wiring`] — table-driven broker wire-token → canonical-enum mapping
//! - [`decimal`] — exact decimal wire formatting (no floats, ever)
//! - [`error`] — [`BrokerError`] taxonomy

pub mod bracket;
pub mod decimal;
pub mod error;
pub mod executor;
pub mod ratelimit;
pub mod sim;
pub mod stream;
pub mod telemetry;
pub mod types;
pub mod wire;
pub mod wiring;

pub use bracket::{
    BracketCapabilities, BracketError, BracketEvent, BracketOrderSpec, BracketTicket,
    BracketTracker, FillRecord, StopUpdate, stop_client_order_id,
};
pub use decimal::{format_fixed_exact, quantity_to_u32_exact};
pub use error::BrokerError;
pub use executor::{
    BrokerExecutor, IcebergOrderExecutor, MockBrokerExecutor, RiskGatedExecutor,
    validate_native_legs,
};
pub use ratelimit::{
    BrokerRateCatalog, RateLimitedBroker, RateLimiterConfig, RequestPriority,
    TokenBucketRateLimiter,
};
pub use sim::SimExecutionPort;
pub use stream::{
    BrokerExecutionEvent, ChannelExecutionStream, ExecutionEventSink, ExecutionStream,
};
pub use types::{
    BrokerHealth, BrokerMargins, BrokerPosition, OrderSnapshot, PositionSide, ProductType,
};
pub use wiring::{BrokerEnumMap, OrderTypeKind};

// Canonical domain re-exports (unified strictly on tantra-core).
pub use tantra_core::{
    Exchange, FillReport, InstrumentId, Money, OrderAck, OrderRequest, OrderSide, OrderStatus,
    OrderType, Price, PriceBand, Quantity, TimeInForce,
};
pub use tantra_risk::{RiskEngine, RiskRefusal};
