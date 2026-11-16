//! Deterministic simulation/paper execution port; the sync counterpart to
//! `BrokerExecutor`, stamped with explicit timestamps so simulated venues
//! replay byte-identically — AGENTS §4.3.3 dual execution target.

use chrono::{DateTime, Utc};
use std::sync::Arc;
use tantra_core::OrderRequest;

/// Abstraction for a broker execution venue.
///
/// Every operation receives the dispatch timestamp explicitly so that
/// simulated venues are temporally deterministic: replaying the same
/// timestamped sequence produces byte-identical fills.
pub trait SimExecutionPort: Send + Sync {
    /// Return the canonical name of the broker (e.g., `"paper"`, `"zerodha"`).
    fn broker_name(&self) -> &'static str;

    /// Submit a concrete [`OrderRequest`] to the broker venue at instant `now`.
    fn submit_order(&self, order: &OrderRequest, now: DateTime<Utc>) -> Result<String, String>;

    /// Request cancellation of an open order by broker order ID at instant `now`.
    fn cancel_order(&self, broker_order_id: &str, now: DateTime<Utc>) -> Result<(), String>;
}

impl<T: SimExecutionPort + ?Sized> SimExecutionPort for Arc<T> {
    fn broker_name(&self) -> &'static str {
        (**self).broker_name()
    }

    fn submit_order(&self, order: &OrderRequest, now: DateTime<Utc>) -> Result<String, String> {
        (**self).submit_order(order, now)
    }

    fn cancel_order(&self, broker_order_id: &str, now: DateTime<Utc>) -> Result<(), String> {
        (**self).cancel_order(broker_order_id, now)
    }
}
