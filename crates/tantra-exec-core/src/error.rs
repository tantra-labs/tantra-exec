//! Broker execution error taxonomy.

use thiserror::Error;
use uuid::Uuid;

/// Errors encountered during order execution across broker adapters.
#[derive(Debug, Error)]
pub enum BrokerError {
    /// Authentication or session token failure.
    #[error("auth failure: {0}")]
    Auth(String),

    /// Order rejected by broker or exchange.
    #[error("order rejected by broker: {0}")]
    Rejected(String),

    /// Order rejected by the synchronous pre-trade risk gate. Boxed because
    /// `tantra_risk::RiskRefusal` is a rich error record that would otherwise
    /// balloon the size of every `Result<_, BrokerError>` return.
    #[error("pre-trade risk refusal: {0}")]
    RiskRefusal(Box<tantra_risk::RiskRefusal>),

    /// Network or transport error communicating with broker API before request
    /// submission or on safe idempotent reads.
    #[error("network or transport error: {0}")]
    Transport(String),

    /// Local operation timed out prior to network submission (e.g. rate-limiter wait).
    #[error("broker request timeout")]
    Timeout,

    /// Order submission outcome is unknown (post-send timeout, connection drop,
    /// or unreadable reply after the request may have reached the broker).
    ///
    /// Non-retryable without reconciliation: blind retry or failover risks
    /// duplicate execution.
    #[error("order submission outcome unknown for {client_order_id}: {message}")]
    OutcomeUnknown {
        /// Client order ID whose execution outcome could not be verified.
        client_order_id: Uuid,
        /// Transport or read failure description.
        message: String,
    },

    /// Order ID not found at broker.
    #[error("order not found: {0}")]
    OrderNotFound(String),

    /// Invalid request or parameters.
    #[error("invalid request or parameters: {0}")]
    InvalidRequest(String),

    /// Local token-bucket rate limiter refused or exhausted the request.
    #[error("rate limiter exhausted: {0}")]
    RateLimited(String),
}

impl BrokerError {
    /// True when the error represents an ambiguous post-send failure where the
    /// venue may have accepted the order.
    pub fn is_outcome_unknown(&self) -> bool {
        matches!(self, Self::OutcomeUnknown { .. })
    }
}

impl From<tantra_risk::RiskRefusal> for BrokerError {
    fn from(value: tantra_risk::RiskRefusal) -> Self {
        Self::RiskRefusal(Box::new(value))
    }
}
