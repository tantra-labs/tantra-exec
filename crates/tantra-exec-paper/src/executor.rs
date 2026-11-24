//! Async `BrokerExecutor` wire port + deterministic sync surface for the
//! paper exchange.
//!
//! The sync surface (`submit_order_sync`, `cancel_order_sync`, `feed_bar`,
//! `feed_tick`) is determinism-critical and takes an explicit `now` — tests
//! and backtests drive the exchange through it. The `#[async_trait]`
//! [`BrokerExecutor`] impl is the live wire port: it may read the wall clock
//! (`Utc::now()`) because it fronts a real broker API contract.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use tantra_core::{Bar, Money, OrderAck, OrderRequest, OrderStatus, Price, Quantity, Tick};
use tantra_exec_core::{
    BrokerError, BrokerExecutor, BrokerHealth, BrokerMargins, BrokerPosition, OrderSnapshot,
    PositionSide, ProductType,
};

use crate::lifecycle::OrderState;
use crate::paper::PaperExchange;

/// Thread-safe wrapper around a [`PaperExchange`] implementing the canonical
/// [`BrokerExecutor`] trait.
#[derive(Debug, Clone)]
pub struct PaperExchangeExecutor {
    exchange: Arc<Mutex<PaperExchange>>,
}

impl PaperExchangeExecutor {
    /// Wrap an exchange into a shared executor.
    pub fn new(exchange: PaperExchange) -> Self {
        Self {
            exchange: Arc::new(Mutex::new(exchange)),
        }
    }

    /// Access the shared exchange handle.
    pub fn exchange(&self) -> &Arc<Mutex<PaperExchange>> {
        &self.exchange
    }

    /// Deterministic synchronous order submission (errors flattened to strings).
    pub fn submit_order_sync(
        &self,
        order: &OrderRequest,
        now: DateTime<Utc>,
    ) -> Result<String, String> {
        self.exchange
            .lock()
            .unwrap()
            .submit_order(order.clone(), now)
            .map_err(|e| e.to_string())
    }

    /// Deterministic synchronous cancellation (errors flattened to strings).
    pub fn cancel_order_sync(
        &self,
        broker_order_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        self.exchange
            .lock()
            .unwrap()
            .cancel_order(broker_order_id, now)
            .map_err(|e| e.to_string())
    }

    /// Feed a bar into the exchange.
    pub fn feed_bar(&self, bar: &Bar) {
        self.exchange.lock().unwrap().feed_bar(bar);
    }

    /// Feed a tick into the exchange.
    pub fn feed_tick(&self, tick: &Tick) {
        self.exchange.lock().unwrap().feed_tick(tick);
    }

    /// Map an exchange error string to the canonical [`BrokerError`] taxonomy.
    fn map_error(msg: String) -> BrokerError {
        if msg.starts_with("order not found") {
            BrokerError::OrderNotFound(msg)
        } else {
            BrokerError::Rejected(msg)
        }
    }
}

impl tantra_exec_core::sim::SimExecutionPort for PaperExchangeExecutor {
    fn broker_name(&self) -> &'static str {
        "paper"
    }

    fn submit_order(&self, order: &OrderRequest, now: DateTime<Utc>) -> Result<String, String> {
        self.submit_order_sync(order, now)
    }

    fn cancel_order(&self, broker_order_id: &str, now: DateTime<Utc>) -> Result<(), String> {
        self.cancel_order_sync(broker_order_id, now)
    }
}

/// Map the paper lifecycle state to the canonical [`OrderStatus`].
fn map_state(state: OrderState) -> OrderStatus {
    match state {
        OrderState::New | OrderState::Working | OrderState::StopTriggered => OrderStatus::New,
        OrderState::PartiallyFilled => OrderStatus::PartiallyFilled,
        OrderState::Filled => OrderStatus::Filled,
        OrderState::Cancelled => OrderStatus::Cancelled,
        OrderState::Rejected => OrderStatus::Rejected,
    }
}

#[async_trait]
impl BrokerExecutor for PaperExchangeExecutor {
    fn name(&self) -> &str {
        "paper"
    }

    async fn health_check(&self) -> Result<BrokerHealth, BrokerError> {
        Ok(BrokerHealth::session_ok(
            "paper",
            "simulated session active",
        ))
    }

    async fn place_order(&self, req: OrderRequest) -> Result<OrderAck, BrokerError> {
        let now = Utc::now();
        let order_id = self.submit_order_sync(&req, now).map_err(Self::map_error)?;
        Ok(OrderAck {
            order_id: order_id.clone(),
            client_order_id: req.client_order_id,
            exchange_order_id: Some(format!("EX-{order_id}")),
            status: OrderStatus::New,
            timestamp: Utc::now(),
            correlation_id: Some(req.correlation_id()),
            client_id: req.client_id.clone(),
            trace_context: None,
        })
    }

    async fn cancel_order(&self, order_id: &str) -> Result<(), BrokerError> {
        self.cancel_order_sync(order_id, Utc::now())
            .map_err(Self::map_error)
    }

    async fn modify_order(
        &self,
        order_id: &str,
        new_price: Option<Price>,
        new_quantity: Option<Quantity>,
    ) -> Result<OrderAck, BrokerError> {
        let now = Utc::now();
        let mut exchange = self.exchange.lock().unwrap();
        exchange
            .amend_order(order_id, new_price, new_quantity, now)
            .map_err(|e| Self::map_error(e.to_string()))?;
        let client_order_id = exchange
            .order(order_id)
            .map(|o| o.client_order_id)
            .unwrap_or_default();
        let status = exchange
            .order(order_id)
            .map(|o| map_state(o.state))
            .unwrap_or(OrderStatus::New);
        Ok(OrderAck {
            order_id: order_id.to_string(),
            client_order_id,
            exchange_order_id: Some(format!("EX-{order_id}")),
            status,
            timestamp: now,
            correlation_id: None,
            client_id: None,
            trace_context: None,
        })
    }

    async fn fetch_order_book(&self) -> Result<Vec<OrderSnapshot>, BrokerError> {
        let exchange = self.exchange.lock().unwrap();
        let mut book = Vec::new();
        for (order_id, working) in exchange.working_orders() {
            book.push(OrderSnapshot {
                order_id: order_id.clone(),
                client_order_id: Some(working.client_order_id),
                exchange_order_id: Some(format!("EX-{order_id}")),
                instrument: working.instrument.clone(),
                side: working.side,
                order_type: working.order_type,
                quantity: working.original_qty,
                filled_quantity: working.filled_qty,
                price: working.order_type.limit_price(),
                trigger_price: working.order_type.trigger_price(),
                status: map_state(working.state),
                product: Some(ProductType::CncDelivery),
                timestamp: working.submitted_ts,
            });
        }
        Ok(book)
    }

    async fn fetch_positions(&self) -> Result<Vec<BrokerPosition>, BrokerError> {
        let exchange = self.exchange.lock().unwrap();
        let mut positions = Vec::new();
        for (instrument, qty) in exchange.positions() {
            let side = if *qty > Decimal::ZERO {
                PositionSide::Long
            } else if *qty < Decimal::ZERO {
                PositionSide::Short
            } else {
                PositionSide::Flat
            };
            let last_price = exchange.last_price(instrument);
            positions.push(BrokerPosition {
                instrument: instrument.clone(),
                product: ProductType::CncDelivery,
                side,
                quantity: Quantity::try_new(qty.abs()).unwrap_or(Quantity::ZERO),
                average_price: last_price.unwrap_or(Price::ZERO),
                realized_pnl: None,
                last_price,
            });
        }
        Ok(positions)
    }

    async fn fetch_margins(&self) -> Result<BrokerMargins, BrokerError> {
        let exchange = self.exchange.lock().unwrap();
        Ok(BrokerMargins {
            available_cash: Money::inr(exchange.cash()),
            used_margin: Money::inr(Decimal::ZERO),
            collateral: Money::inr(Decimal::ZERO),
        })
    }

    async fn get_order_status(&self, order_id: &str) -> Result<OrderStatus, BrokerError> {
        let exchange = self.exchange.lock().unwrap();
        match exchange.order(order_id) {
            Some(working) => Ok(map_state(working.state)),
            None => Err(BrokerError::OrderNotFound(order_id.to_string())),
        }
    }
}
