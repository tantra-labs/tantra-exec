//! Bracket-order (SL / multi-target / trailing-stop) execution contract.
//!
//! Gap 0003 (`docs/gaps/0003-bracket-order-slt-sl-execution.md`): the
//! [`BrokerExecutor`] trait routes plain orders but cannot express an entry
//! order with an attached stop-loss, partial-exit target legs, and a trailing
//! stop that gets modified in both price and remaining quantity as price moves
//! or partials fill.
//!
//! This module provides the pure, `Decimal`-only contract types plus the
//! deterministic [`BracketTracker`] state machine that models one bracket
//! across its lifecycle (entry fill → partial exits → stop trail/resize →
//! stop trigger → close) so a strategy/runtime can drive SL/TSL and partial
//! exits deterministically. Zero I/O and zero clock reads: every timestamp is
//! injected by the caller, and every transition is journaled into
//! [`BracketEvent`]s for the audit WAL.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use tantra_core::{OrderRequest, OrderSide, OrderType, PlannedTarget, Price, Quantity, TrailRule};

/// Deterministic client order id for a bracket's stop leg.
///
/// The stop child order is idempotent across modify/re-arm cycles: its client
/// order id is derived from the bracket id (`bracket_id + "#stop"`), never a
/// fresh UUID, so a broker reject + retry cannot double-submit the stop.
pub fn stop_client_order_id(bracket_id: &str) -> String {
    format!("{bracket_id}#stop")
}

/// A bracket-order specification: an entry order with an attached initial
/// stop, one or more partial-exit target legs, and an optional trailing rule.
///
/// All prices and quantities are exact `Decimal`s. The validating constructor
/// enforces that the stop sits on the correct side of the (known) entry price
/// and that every target quantity is positive with the ladder summing to at
/// most the entry quantity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BracketOrderSpec {
    /// Entry order: side, instrument, and quantity define the bracket's legs.
    pub entry: OrderRequest,
    /// Initial stop price (below the entry for longs, above for shorts).
    pub initial_stop: Price,
    /// Partial-exit target legs, in fill order.
    pub targets: Vec<PlannedTarget>,
    /// Optional trailing rule (TSL updates are driven by the caller/runtime).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail: Option<TrailRule>,
}

impl BracketOrderSpec {
    /// The reference entry price from the entry order type, when determinable
    /// at spec time. A market entry has no price yet and is validated on fill.
    fn entry_reference_price(&self) -> Option<Price> {
        match self.entry.order_type {
            OrderType::Limit(price) => Some(price),
            OrderType::StopLoss(trigger) => Some(trigger),
            OrderType::StopLimit { trigger, .. } => Some(trigger),
            OrderType::Market => None,
        }
    }

    /// Validate the spec: stop side vs the known entry price, positive target
    /// quantities, and a target ladder that does not exceed the entry quantity.
    ///
    /// # Errors
    /// Returns [`BracketError::InvalidSpec`] on any violation.
    pub fn validate(&self) -> Result<(), BracketError> {
        if let Some(entry) = self.entry_reference_price() {
            let wrong_side = match self.entry.side {
                OrderSide::Buy => self.initial_stop >= entry,
                OrderSide::Sell => self.initial_stop <= entry,
            };
            if wrong_side {
                let side = match self.entry.side {
                    OrderSide::Buy => "below",
                    OrderSide::Sell => "above",
                };
                return Err(BracketError::InvalidSpec(format!(
                    "{} bracket initial stop {} must be {side} the entry price {}",
                    match self.entry.side {
                        OrderSide::Buy => "buy",
                        OrderSide::Sell => "sell",
                    },
                    self.initial_stop,
                    entry
                )));
            }
        }
        let mut target_total = Decimal::ZERO;
        for target in &self.targets {
            if !target.qty.is_positive() {
                return Err(BracketError::InvalidSpec(format!(
                    "bracket target quantity must be positive, got {}",
                    target.qty
                )));
            }
            target_total += target.qty.value();
        }
        if target_total > self.entry.quantity.value() {
            return Err(BracketError::InvalidSpec(format!(
                "bracket target quantities sum to {target_total}, exceeding entry quantity {}",
                self.entry.quantity
            )));
        }
        Ok(())
    }

    /// Validating constructor — see [`BracketOrderSpec::validate`].
    ///
    /// # Errors
    /// Returns [`BracketError::InvalidSpec`] when the stop is on the wrong
    /// side of the entry price or the target ladder is malformed.
    pub fn new(
        entry: OrderRequest,
        initial_stop: Price,
        targets: Vec<PlannedTarget>,
        trail: Option<TrailRule>,
    ) -> Result<Self, BracketError> {
        let spec = Self {
            entry,
            initial_stop,
            targets,
            trail,
        };
        spec.validate()?;
        Ok(spec)
    }
}

/// Broker-side acknowledgement of a bracket submission.
///
/// `entry_id`/`stop_id`/`target_ids` are the broker order ids of the bracket
/// legs (for native BO/CO brackets `entry_id` and `stop_id` collapse onto the
/// single native order).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BracketTicket {
    /// Canonical bracket id.
    pub bracket_id: String,
    /// Broker order id of the entry leg.
    pub entry_id: String,
    /// Broker order id of the resting stop child order.
    pub stop_id: String,
    /// Broker order ids of the target legs, in ladder order.
    pub target_ids: Vec<String>,
}

/// Execution-domain record of a stop modification (trail and/or quantity
/// resize). This is an execution record, not a duplicate of a `tantra-core`
/// type — no canonical stop-update type exists there yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StopUpdate {
    /// The bracket being modified.
    pub bracket_id: String,
    /// Broker order id of the stop child order that was modified.
    pub order_id: String,
    /// New resting stop price.
    pub stop: Price,
    /// Remaining quantity covered by the stop.
    pub qty: Quantity,
    /// Modification timestamp (injected by the caller).
    pub ts: DateTime<Utc>,
}

/// Per-broker bracket-order capability flags.
///
/// `native_bo` and `emulated_multileg` select the placement strategy (a single
/// native BO/CO order vs. emulated entry + stop + target child orders);
/// `atomic_modify` reports whether a stop modify updates price and quantity in
/// one broker call (otherwise the adapter must cancel + replace).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BracketCapabilities {
    /// The broker supports a native BO/CO bracket (entry + SL as one order).
    pub native_bo: bool,
    /// The broker can host emulated child orders (entry + stop + targets).
    pub emulated_multileg: bool,
    /// A stop modify atomically updates price and remaining quantity.
    pub atomic_modify: bool,
}

impl BracketCapabilities {
    /// No bracket support at all (default for adapters that don't implement
    /// the bracket contract).
    pub const fn none() -> Self {
        Self {
            native_bo: false,
            emulated_multileg: false,
            atomic_modify: false,
        }
    }
}

impl Default for BracketCapabilities {
    /// Synthetic/OCO emulation with atomic stop modification: the runtime
    /// coordinates entry + stop + target child orders itself.
    fn default() -> Self {
        Self {
            native_bo: false,
            emulated_multileg: true,
            atomic_modify: true,
        }
    }
}

/// Bracket-order contract errors.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum BracketError {
    /// The bracket spec (or a state transition) violates the bracket contract.
    #[error("invalid bracket spec: {0}")]
    InvalidSpec(String),

    /// The referenced bracket id is unknown.
    #[error("unknown bracket: {0}")]
    UnknownBracket(String),

    /// A stop move widens risk: for a long the stop may only move up
    /// (`new >= current`), for a short only down (`new <= current`).
    #[error("stop move widens risk: current {current} -> requested {new}")]
    StopWidening {
        /// Current resting stop price.
        current: Price,
        /// Requested (widening) stop price.
        new: Price,
    },

    /// The stop quantity is inconsistent with the open position.
    #[error("stop quantity mismatch: open {open_qty}, stop {stop_qty}")]
    QuantityMismatch {
        /// Quantity still open in the position.
        open_qty: Quantity,
        /// Quantity the stop would cover.
        stop_qty: Quantity,
    },

    /// The operation is not valid for the bracket's current lifecycle state
    /// (e.g. before the entry fills, or after the bracket closed).
    #[error("bracket not active: {0}")]
    NotActive(String),
}

/// Journaled lifecycle events of a bracket (audit WAL / event-sourcing input).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BracketEvent {
    /// The entry leg filled and the position opened.
    EntryFilled {
        /// Fill price.
        price: Price,
        /// Filled quantity (equals the bracket entry quantity).
        qty: Quantity,
        /// Fill timestamp (injected).
        ts: DateTime<Utc>,
    },
    /// A partial-exit leg (or free partial) filled.
    PartialExit {
        /// Exit price.
        price: Price,
        /// Exited quantity.
        qty: Quantity,
        /// Human-readable reason (e.g. `"target 1 at 1R"`).
        reason: String,
    },
    /// The resting stop was modified (trailed and/or quantity-resized).
    StopMoved {
        /// Previous resting stop price.
        old: Price,
        /// New resting stop price.
        new: Price,
        /// Quantity covered by the stop after the move.
        qty: Quantity,
        /// Modification timestamp (injected).
        ts: DateTime<Utc>,
    },
    /// The resting stop triggered and the stop order filled.
    StopTriggered {
        /// Price at which the stop triggered.
        price: Price,
        /// Quantity filled by the stop.
        qty: Quantity,
        /// Trigger timestamp (injected).
        ts: DateTime<Utc>,
    },
    /// The bracket closed and the position is flat.
    Closed {
        /// Close reason (`"stop_trigger"`, `"target"`, ...).
        reason: String,
        /// Realized P&L over the closed quantity (exact `Decimal`).
        realized_pnl: Decimal,
    },
}

/// Record of the bracket's entry fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FillRecord {
    /// Entry fill price.
    pub price: Price,
    /// Entry fill quantity.
    pub qty: Quantity,
    /// Fill timestamp (injected).
    pub ts: DateTime<Utc>,
}

/// Deterministic bracket lifecycle state machine.
///
/// Models one bracket from open (entry unfilled) through entry fill, partial
/// exits with FIFO target matching, stop trails/resizes, and a stop-trigger
/// close. Pure and clock-free: timestamps are injected by the caller, and
/// every transition journals a [`BracketEvent`].
#[derive(Debug, Clone)]
pub struct BracketTracker {
    spec: BracketOrderSpec,
    /// When the bracket was opened (injected, for audit/journaling).
    opened_at: DateTime<Utc>,
    /// Entry fill record; `None` until [`BracketTracker::on_entry_fill`].
    entry: Option<FillRecord>,
    /// Quantity still open (not yet exited).
    open_qty: Quantity,
    /// Quantity currently covered by the resting stop.
    stop_qty: Quantity,
    /// Current resting stop price.
    resting_stop: Price,
    /// Target legs not yet filled, in order (FIFO matching at the front).
    remaining_targets: Vec<PlannedTarget>,
    /// Target legs already filled, in fill order.
    filled_targets: Vec<PlannedTarget>,
    /// Whether the bracket has closed (stop triggered).
    closed: bool,
    /// Journaled lifecycle events, in order.
    events: Vec<BracketEvent>,
}

impl BracketTracker {
    /// Open a bracket for a (validated) spec. The entry has not filled yet.
    ///
    /// # Errors
    /// Returns [`BracketError::InvalidSpec`] when the spec fails
    /// [`BracketOrderSpec::validate`].
    pub fn new(spec: BracketOrderSpec, now: DateTime<Utc>) -> Result<Self, BracketError> {
        spec.validate()?;
        Ok(Self {
            opened_at: now,
            entry: None,
            open_qty: Quantity::ZERO,
            stop_qty: spec.entry.quantity,
            resting_stop: spec.initial_stop,
            remaining_targets: spec.targets.clone(),
            filled_targets: Vec::new(),
            closed: false,
            events: Vec::new(),
            spec,
        })
    }

    /// Record the entry fill, opening the position.
    ///
    /// The fill quantity must equal the bracket entry quantity exactly, and
    /// (fail-closed) the fill price must keep the stop on the correct side of
    /// the entry — this also validates market entries that had no reference
    /// price at spec time.
    ///
    /// # Errors
    /// - [`BracketError::NotActive`] if the bracket is closed or already filled.
    /// - [`BracketError::InvalidSpec`] on a quantity or stop-side violation.
    pub fn on_entry_fill(
        &mut self,
        price: Price,
        qty: Quantity,
        ts: DateTime<Utc>,
    ) -> Result<(), BracketError> {
        if self.closed {
            return Err(BracketError::NotActive("bracket already closed".into()));
        }
        if self.entry.is_some() {
            return Err(BracketError::NotActive(
                "bracket entry already filled".into(),
            ));
        }
        if qty != self.spec.entry.quantity {
            return Err(BracketError::InvalidSpec(format!(
                "entry fill qty {qty} does not match bracket entry qty {}",
                self.spec.entry.quantity
            )));
        }
        let wrong_side = match self.spec.entry.side {
            OrderSide::Buy => price <= self.spec.initial_stop,
            OrderSide::Sell => price >= self.spec.initial_stop,
        };
        if wrong_side {
            return Err(BracketError::InvalidSpec(format!(
                "entry fill {price} is on the wrong side of initial stop {}",
                self.spec.initial_stop
            )));
        }
        self.open_qty = qty;
        self.stop_qty = qty;
        self.entry = Some(FillRecord { price, qty, ts });
        self.events
            .push(BracketEvent::EntryFilled { price, qty, ts });
        Ok(())
    }

    /// Trail and/or quantity-resize the resting stop.
    ///
    /// The stop must never widen risk: for a long the new stop must be
    /// `>=` the resting stop, for a short `<=` it. A `new_qty` (when given)
    /// must be within `0..=open_qty`.
    ///
    /// # Errors
    /// - [`BracketError::NotActive`] if the bracket is closed or unfilled.
    /// - [`BracketError::StopWidening`] when the move would widen risk.
    /// - [`BracketError::QuantityMismatch`] when `new_qty` exceeds the open
    ///   position or is negative.
    pub fn modify_stop(
        &mut self,
        new_stop: Price,
        new_qty: Option<Quantity>,
        ts: DateTime<Utc>,
    ) -> Result<(), BracketError> {
        if self.closed {
            return Err(BracketError::NotActive("bracket already closed".into()));
        }
        if self.entry.is_none() {
            return Err(BracketError::NotActive("bracket entry not filled".into()));
        }
        let widening = match self.spec.entry.side {
            OrderSide::Buy => new_stop < self.resting_stop,
            OrderSide::Sell => new_stop > self.resting_stop,
        };
        if widening {
            return Err(BracketError::StopWidening {
                current: self.resting_stop,
                new: new_stop,
            });
        }
        if let Some(qty) = new_qty {
            if qty > self.open_qty || qty < Quantity::ZERO {
                return Err(BracketError::QuantityMismatch {
                    open_qty: self.open_qty,
                    stop_qty: qty,
                });
            }
            self.stop_qty = qty;
        }
        let old = self.resting_stop;
        self.resting_stop = new_stop;
        self.events.push(BracketEvent::StopMoved {
            old,
            new: new_stop,
            qty: self.stop_qty,
            ts,
        });
        Ok(())
    }

    /// Record a partial exit, shrinking the open position.
    ///
    /// When the exit quantity exactly equals the front remaining target leg
    /// (FIFO), that leg moves to the filled set; any other partial is a "free"
    /// exit that still shrinks the open quantity. The position must never go
    /// negative (short-flip guard).
    ///
    /// # Errors
    /// - [`BracketError::NotActive`] if the bracket is closed or unfilled.
    /// - [`BracketError::InvalidSpec`] for a non-positive or over-position exit.
    pub fn partial_exit(
        &mut self,
        qty: Quantity,
        price: Price,
        reason: &str,
        ts: DateTime<Utc>,
    ) -> Result<(), BracketError> {
        let _ = ts;
        if self.closed {
            return Err(BracketError::NotActive("bracket already closed".into()));
        }
        if self.entry.is_none() {
            return Err(BracketError::NotActive("bracket entry not filled".into()));
        }
        if qty <= Quantity::ZERO {
            return Err(BracketError::InvalidSpec(format!(
                "partial exit quantity must be positive, got {qty}"
            )));
        }
        if qty > self.open_qty {
            return Err(BracketError::InvalidSpec(format!(
                "partial exit qty {qty} exceeds open qty {}",
                self.open_qty
            )));
        }
        if self
            .remaining_targets
            .first()
            .is_some_and(|leg| leg.qty == qty)
        {
            let filled = self.remaining_targets.remove(0);
            self.filled_targets.push(filled);
        }
        self.open_qty = Quantity::try_new(self.open_qty.value() - qty.value())
            .map_err(|e| BracketError::InvalidSpec(e.to_string()))?;
        self.events.push(BracketEvent::PartialExit {
            price,
            qty,
            reason: reason.to_string(),
        });
        Ok(())
    }

    /// The resting stop triggered: close the bracket.
    ///
    /// Realized P&L is exact: `(stop − entry) × qty` for longs and
    /// `(entry − stop) × qty` for shorts, where `qty` is the remaining open
    /// quantity.
    ///
    /// # Errors
    /// Returns [`BracketError::NotActive`] when the bracket is closed or
    /// the entry has not filled.
    pub fn on_stop_trigger(&mut self, price: Price, ts: DateTime<Utc>) -> Result<(), BracketError> {
        if self.closed {
            return Err(BracketError::NotActive("bracket already closed".into()));
        }
        let entry = self
            .entry
            .ok_or_else(|| BracketError::NotActive("bracket entry not filled".into()))?;
        let stop = self.resting_stop.value();
        let entry_price = entry.price.value();
        let qty = self.open_qty.value();
        let realized_pnl = match self.spec.entry.side {
            OrderSide::Buy => (stop - entry_price) * qty,
            OrderSide::Sell => (entry_price - stop) * qty,
        };
        self.events.push(BracketEvent::StopTriggered {
            price,
            qty: self.open_qty,
            ts,
        });
        self.events.push(BracketEvent::Closed {
            reason: "stop_trigger".into(),
            realized_pnl,
        });
        self.closed = true;
        self.open_qty = Quantity::ZERO;
        Ok(())
    }

    /// Whether the bracket has closed.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Quantity still open (not yet exited).
    pub fn open_qty(&self) -> Quantity {
        self.open_qty
    }

    /// Current resting stop price.
    pub fn resting_stop(&self) -> Price {
        self.resting_stop
    }

    /// Journaled lifecycle events, in order.
    pub fn events(&self) -> &[BracketEvent] {
        &self.events
    }

    /// When the bracket was opened (injected).
    pub fn opened_at(&self) -> DateTime<Utc> {
        self.opened_at
    }

    /// Target legs not yet filled, in ladder order.
    pub fn remaining_targets(&self) -> &[PlannedTarget] {
        &self.remaining_targets
    }

    /// Target legs already filled, in fill order.
    pub fn filled_targets(&self) -> &[PlannedTarget] {
        &self.filled_targets
    }

    /// Total quantity exited via filled target legs.
    pub fn filled_target_qty(&self) -> Quantity {
        let total = self
            .filled_targets
            .iter()
            .fold(Decimal::ZERO, |acc, leg| acc + leg.qty.value());
        Quantity::try_new(total).unwrap_or(Quantity::ZERO)
    }
}
