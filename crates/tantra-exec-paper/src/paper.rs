//! Paper exchange matching engine aggregate.
//!
//! [`PaperExchange`] is a deterministic, zero-I/O venue: every state
//! transition takes an explicit `DateTime<Utc>` from the caller (no wall
//! clock is ever read), all prices/quantities/cash are exact
//! `rust_decimal::Decimal` values wrapped in the canonical [`tantra_core`]
//! value objects, and replaying an identical tape produces byte-identical
//! fills and events.
//!
//! Matching is price-time priority across two liquidity pools:
//! 1. **Resting participant orders** — working/partially-filled contra
//!    orders tracked in [`PaperExchange::by_instrument`] arrival order.
//! 2. **Feed book depth** — synthetic/real depth maintained by
//!    [`PaperExchange::feed_tick`] / [`PaperExchange::feed_bar`] in the
//!    ported [`crate::book::OrderBook`].

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use tantra_core::{
    AccountId, Bar, FillReport, InstrumentId, Money, OrderRequest, OrderSide, OrderType, Price,
    Quantity, Tick, TimeInForce,
};

use crate::book;
use crate::latency::NetworkLatency;
use crate::lifecycle::{ExchangeError, ExchangeEvent, OrderState, WorkingOrder};
use crate::mis::MisSquareOffManager;
use crate::slippage::{AdverseFillModel, SlippageModel};

/// Computes total statutory fees (INR) for a fill given the order side and
/// notional. Returned as a `Decimal` so the exchange can keep every cash
/// transition exact.
pub trait FillCostFn: Send + Sync {
    /// Total statutory fees in INR for a fill of `notional`.
    fn compute(&self, side: OrderSide, notional: Decimal) -> Decimal;
}

/// A fee model that charges nothing — the deterministic default.
#[derive(Debug, Clone, Copy, Default)]
pub struct ZeroFillCost;

impl FillCostFn for ZeroFillCost {
    fn compute(&self, _side: OrderSide, _notional: Decimal) -> Decimal {
        Decimal::ZERO
    }
}

/// Per-account cash and signed positions.
#[derive(Debug, Clone, Default)]
pub struct AccountState {
    /// Free cash balance in INR (exact decimal).
    pub cash: Decimal,
    /// Net signed position per instrument (positive = long, negative = short).
    pub positions: HashMap<InstrumentId, Decimal>,
}

/// The paper trading exchange aggregate: account, per-instrument books,
/// resting orders, fills, and the immutable event stream.
pub struct PaperExchange {
    pub(crate) account: AccountState,
    pub(crate) account_id: AccountId,
    pub(crate) books: HashMap<InstrumentId, book::OrderBook>,
    pub(crate) orders: HashMap<String, WorkingOrder>,
    /// Working order ids per instrument, in arrival order (time priority).
    pub(crate) by_instrument: HashMap<InstrumentId, Vec<String>>,
    pub(crate) fills: Vec<FillReport>,
    pub(crate) events: Vec<ExchangeEvent>,
    pub(crate) fill_seq: u64,
    pub(crate) order_seq: u64,
    pub(crate) latency: Option<NetworkLatency>,
    pub(crate) slippage: Option<SlippageModel>,
    pub(crate) adverse_fill: Option<AdverseFillModel>,
    pub(crate) mis: Option<MisSquareOffManager>,
    pub(crate) cost_fn: Box<dyn FillCostFn>,
    pub(crate) last_prices: HashMap<InstrumentId, Price>,
}

impl fmt::Debug for PaperExchange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PaperExchange")
            .field("account", &self.account)
            .field("account_id", &self.account_id)
            .field("orders", &self.orders)
            .field("fills", &self.fills)
            .field("events", &self.events)
            .field("fill_seq", &self.fill_seq)
            .field("order_seq", &self.order_seq)
            .field("cost_fn", &"Box<dyn FillCostFn>")
            .finish_non_exhaustive()
    }
}

impl PaperExchange {
    /// Create a new paper exchange with an initial INR cash balance.
    pub fn new(initial_cash: Decimal) -> Self {
        Self {
            account: AccountState {
                cash: initial_cash,
                positions: HashMap::new(),
            },
            account_id: AccountId::new("paper"),
            books: HashMap::new(),
            orders: HashMap::new(),
            by_instrument: HashMap::new(),
            fills: Vec::new(),
            events: Vec::new(),
            fill_seq: 0,
            order_seq: 0,
            latency: None,
            slippage: None,
            adverse_fill: None,
            mis: None,
            cost_fn: Box::new(ZeroFillCost),
            last_prices: HashMap::new(),
        }
    }

    /// Configure the statutory fee model.
    pub fn with_cost_fn(mut self, cost_fn: Box<dyn FillCostFn>) -> Self {
        self.cost_fn = cost_fn;
        self
    }

    /// Configure a seeded network-latency model (sets order `ready_ts`).
    pub fn with_latency(mut self, latency: NetworkLatency) -> Self {
        self.latency = Some(latency);
        self
    }

    /// Configure a slippage model (reserved; not yet applied in P0 matching).
    pub fn with_slippage(mut self, slippage: SlippageModel) -> Self {
        self.slippage = Some(slippage);
        self
    }

    /// Configure adverse-fill slippage applied to feed-book fills.
    pub fn with_adverse_fill(mut self, adverse_fill: AdverseFillModel) -> Self {
        self.adverse_fill = Some(adverse_fill);
        self
    }

    /// Configure the MIS square-off manager.
    pub fn with_mis(mut self, mis: MisSquareOffManager) -> Self {
        self.mis = Some(mis);
        self
    }

    // ------------------------------------------------------------------
    // Order entry
    // ------------------------------------------------------------------

    /// Submit an order at `now`.
    ///
    /// Returns the broker order id (`PAPER-0001`, ...). A market order with
    /// no contra liquidity (neither feed depth nor a resting participant) is
    /// rejected with [`ExchangeError::NoMarketPrice`]; a zero/negative
    /// quantity order is accepted as a rejected order (a `Rejected` event is
    /// emitted). Stop orders arm in `New` state and only match after their
    /// trigger price is breached.
    pub fn submit_order(
        &mut self,
        order: OrderRequest,
        now: DateTime<Utc>,
    ) -> Result<String, ExchangeError> {
        let instrument = order.instrument.clone();

        // Validation: quantity must be strictly positive.
        if order.quantity.value() <= Decimal::ZERO {
            self.order_seq += 1;
            let order_seq = self.order_seq;
            let id = format!("PAPER-{order_seq:04}");
            let reason = "quantity must be positive".to_string();
            self.orders.insert(
                id.clone(),
                WorkingOrder {
                    broker_order_id: id.clone(),
                    client_order_id: order.client_order_id,
                    instrument,
                    side: order.side,
                    order_type: order.order_type,
                    tif: order.tif,
                    original_qty: order.quantity,
                    remaining_qty: order.quantity,
                    filled_qty: Quantity::ZERO,
                    avg_fill_price: Price::ZERO,
                    state: OrderState::Rejected,
                    ready_ts: None,
                    expires_ts: None,
                    reject_reason: Some(reason.clone()),
                    submitted_ts: now,
                },
            );
            self.events.push(ExchangeEvent::Rejected {
                broker_order_id: id.clone(),
                reason,
                timestamp: now,
            });
            return Ok(id);
        }

        let is_stop = order.order_type.trigger_price().is_some();
        let is_pure_market =
            order.order_type.limit_price().is_none() && order.order_type.trigger_price().is_none();

        // A pure market order needs contra liquidity to trade against.
        if is_pure_market {
            let has_contra_participant = self.has_contra_participant(&instrument, order.side);
            let has_feed_depth = self
                .books
                .get(&instrument)
                .map(|b| b.best_contra_depth(order.side) > 0)
                .unwrap_or(false);
            if !has_contra_participant && !has_feed_depth {
                return Err(ExchangeError::NoMarketPrice(instrument));
            }
        }

        self.order_seq += 1;
        let order_seq = self.order_seq;
        let id = format!("PAPER-{order_seq:04}");

        let state = if is_stop {
            OrderState::New
        } else {
            OrderState::Working
        };

        let mut working = WorkingOrder {
            broker_order_id: id.clone(),
            client_order_id: order.client_order_id,
            instrument: instrument.clone(),
            side: order.side,
            order_type: order.order_type,
            tif: order.tif,
            original_qty: order.quantity,
            remaining_qty: order.quantity,
            filled_qty: Quantity::ZERO,
            avg_fill_price: Price::ZERO,
            state,
            ready_ts: None,
            expires_ts: None,
            reject_reason: None,
            submitted_ts: now,
        };
        if let Some(latency) = self.latency.as_mut() {
            let us = latency.sample();
            working.ready_ts = Some(now + Duration::microseconds(us as i64));
        }

        self.orders.insert(id.clone(), working);
        // Time priority: arrival order in the per-instrument working list.
        self.by_instrument
            .entry(instrument.clone())
            .or_default()
            .push(id.clone());

        self.events.push(ExchangeEvent::OrderAccepted {
            broker_order_id: id.clone(),
            instrument: instrument.clone(),
            side: order.side,
            quantity: order.quantity,
            timestamp: now,
        });

        // Stops arm in `New`; they only trigger once the last price crosses.
        if is_stop {
            let crossed = {
                let last = self.last_prices.get(&instrument).copied();
                match (order.order_type.trigger_price(), last) {
                    (Some(trigger), Some(last)) => match order.side {
                        OrderSide::Buy => last.value() >= trigger.value(),
                        OrderSide::Sell => last.value() <= trigger.value(),
                    },
                    _ => false,
                }
            };
            if crossed {
                self.trigger_stop(&id, now);
                self.match_order(&id, now);
            }
            return Ok(id);
        }

        // Market / limit: match immediately (a non-marketable limit rests).
        self.match_order(&id, now);
        Ok(id)
    }

    /// Submit a Day order with an explicit good-till date. The order is
    /// cancelled by [`PaperExchange::advance_to`] once `gtd_until` passes.
    pub fn submit_order_gtd(
        &mut self,
        order: OrderRequest,
        gtd_until: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<String, ExchangeError> {
        let id = self.submit_order(order, now)?;
        if let Some(working) = self.orders.get_mut(&id) {
            working.expires_ts = Some(gtd_until);
        }
        Ok(id)
    }

    /// Cancel an active order (New/Working/PartiallyFilled).
    pub fn cancel_order(
        &mut self,
        broker_order_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), ExchangeError> {
        let state = self
            .orders
            .get(broker_order_id)
            .map(|o| o.state)
            .ok_or_else(|| ExchangeError::OrderNotFound(broker_order_id.to_string()))?;
        if !matches!(
            state,
            OrderState::New | OrderState::Working | OrderState::PartiallyFilled
        ) {
            return Err(ExchangeError::Terminal(broker_order_id.to_string()));
        }
        if let Some(working) = self.orders.get_mut(broker_order_id) {
            working.state = OrderState::Cancelled;
        }
        self.events.push(ExchangeEvent::Cancelled {
            broker_order_id: broker_order_id.to_string(),
            timestamp: now,
        });
        self.remove_from_by_instrument(broker_order_id);
        Ok(())
    }

    /// Amend an active order's price and/or quantity, then re-run matching
    /// (the amended order may fill immediately).
    pub fn amend_order(
        &mut self,
        broker_order_id: &str,
        new_price: Option<Price>,
        new_qty: Option<Quantity>,
        now: DateTime<Utc>,
    ) -> Result<(), ExchangeError> {
        let (active, filled_qty) = {
            let working = self
                .orders
                .get(broker_order_id)
                .ok_or_else(|| ExchangeError::OrderNotFound(broker_order_id.to_string()))?;
            (
                matches!(
                    working.state,
                    OrderState::New | OrderState::Working | OrderState::PartiallyFilled
                ),
                working.filled_qty.value(),
            )
        };
        if !active {
            return Err(ExchangeError::Terminal(broker_order_id.to_string()));
        }
        if let Some(working) = self.orders.get_mut(broker_order_id) {
            if let Some(price) = new_price {
                working.order_type = match working.order_type {
                    OrderType::Limit(_) => OrderType::Limit(price),
                    OrderType::StopLimit { trigger, .. } => OrderType::StopLimit {
                        trigger,
                        limit: price,
                    },
                    other => other,
                };
            }
            if let Some(qty) = new_qty {
                working.original_qty = qty;
                working.remaining_qty =
                    Quantity::try_new((qty.value() - filled_qty).max(Decimal::ZERO))
                        .unwrap_or(Quantity::ZERO);
            }
        }
        self.match_order(broker_order_id, now);
        Ok(())
    }

    // ------------------------------------------------------------------
    // Market data feeds
    // ------------------------------------------------------------------

    /// Process a bar: update last price, add synthetic depth at the close,
    /// seed the participation cap with the bar's traded volume, then match.
    pub fn feed_bar(&mut self, bar: &Bar) {
        let instrument = bar.instrument.clone();
        let volume = bar.volume.value().to_i64().unwrap_or(0);
        {
            let book = self
                .books
                .entry(instrument.clone())
                .or_insert_with(|| book::OrderBook::new(0, Decimal::ONE));
            book.add_order(bar.close, volume);
            book.set_avg_daily_volume(volume);
        }
        self.last_prices.insert(instrument.clone(), bar.close);
        self.process_instrument(&instrument, bar.timestamp);
    }

    /// Process a tick: update the feed book exactly like
    /// `tantra-runtime-backtest::engine.rs::process_market_event` step 3
    /// (Ltp → synthetic depth; Quote → bid/ask when both positive, else
    /// synthetic depth at last; Depth → L2 ladder when non-empty), update
    /// last price, then match.
    pub fn feed_tick(&mut self, tick: &Tick) {
        let instrument = tick.instrument().clone();
        let now = tick.receive_ts();
        {
            let book = self
                .books
                .entry(instrument.clone())
                .or_insert_with(|| book::OrderBook::new(0, Decimal::ONE));
            match tick {
                Tick::Ltp { price, volume, .. } => {
                    book.add_order(*price, volume.value().to_i64().unwrap_or(0));
                }
                Tick::Quote {
                    bid,
                    ask,
                    bid_qty,
                    ask_qty,
                    last_price,
                    ..
                } => {
                    if bid.value() > Decimal::ZERO && ask.value() > Decimal::ZERO {
                        book.add_bid(*bid, bid_qty.value().to_i64().unwrap_or(0));
                        book.add_ask(*ask, ask_qty.value().to_i64().unwrap_or(0));
                    } else if let Some(last) = last_price {
                        let qty = bid_qty.value().max(ask_qty.value()).to_i64().unwrap_or(0);
                        book.add_order(*last, qty);
                    }
                }
                Tick::Depth {
                    bids,
                    asks,
                    last_price,
                    ..
                } => {
                    if !bids.is_empty() || !asks.is_empty() {
                        book.set_depth(bids, asks);
                    } else if let Some(last) = last_price {
                        book.add_order(*last, 1);
                    }
                }
            }
        }
        if let Some(last) = tick.last_price() {
            self.last_prices.insert(instrument.clone(), last);
        }
        self.process_instrument(&instrument, now);
    }

    /// Re-match every active order for `instrument` in arrival order:
    /// trigger freshly-breached stops, then match marketable orders.
    pub fn process_instrument(&mut self, instrument: &InstrumentId, now: DateTime<Utc>) {
        let ids: Vec<String> = self
            .by_instrument
            .get(instrument)
            .cloned()
            .unwrap_or_default();
        for id in ids {
            let should_trigger = match self.orders.get(&id) {
                Some(working) if working.state == OrderState::New => {
                    match working.order_type.trigger_price() {
                        Some(trigger) => self
                            .last_prices
                            .get(instrument)
                            .map(|last| match working.side {
                                OrderSide::Buy => last.value() >= trigger.value(),
                                OrderSide::Sell => last.value() <= trigger.value(),
                            })
                            .unwrap_or(false),
                        None => false,
                    }
                }
                _ => false,
            };
            if should_trigger {
                self.trigger_stop(&id, now);
                self.match_order(&id, now);
                continue;
            }
            let resting = self
                .orders
                .get(&id)
                .map(|o| o.is_resting())
                .unwrap_or(false);
            if resting {
                self.match_order(&id, now);
            }
        }
    }

    /// Explicit deterministic last-price seed (stops and tests).
    pub fn set_market_price(&mut self, instrument: InstrumentId, price: Price) {
        self.last_prices.insert(instrument, price);
    }

    /// Advance the simulated clock: cancel expired GTD orders, then re-match
    /// every order whose latency `ready_ts` has arrived.
    pub fn advance_to(&mut self, now: DateTime<Utc>) {
        // GTD expiry: cancel any order past its `expires_ts` that is not filled.
        let expired: Vec<String> = self
            .orders
            .iter()
            .filter(|(_, o)| o.expires_ts.is_some_and(|t| t <= now))
            .filter(|(_, o)| !matches!(o.state, OrderState::Filled))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(working) = self.orders.get_mut(&id) {
                if matches!(
                    working.state,
                    OrderState::Filled | OrderState::Cancelled | OrderState::Rejected
                ) {
                    continue;
                }
                working.state = OrderState::Cancelled;
            }
            self.events.push(ExchangeEvent::Cancelled {
                broker_order_id: id.clone(),
                timestamp: now,
            });
            self.remove_from_by_instrument(&id);
        }

        // Latency-ready orders: re-match them against the current book.
        let ready: Vec<InstrumentId> = self
            .orders
            .iter()
            .filter(|(_, o)| o.ready_ts.is_some_and(|t| t <= now))
            .filter(|(_, o)| o.is_active())
            .map(|(_, o)| o.instrument.clone())
            .collect();
        for instrument in ready {
            self.process_instrument(&instrument, now);
        }
    }

    // ------------------------------------------------------------------
    // Read surface
    // ------------------------------------------------------------------

    /// Every fill produced so far, in order.
    pub fn fills(&self) -> &[FillReport] {
        &self.fills
    }

    /// Every event produced so far, in order.
    pub fn events(&self) -> &[ExchangeEvent] {
        &self.events
    }

    /// Look up a tracked order by broker order id.
    pub fn order(&self, id: &str) -> Option<&WorkingOrder> {
        self.orders.get(id)
    }

    /// Signed net positions per instrument.
    pub fn positions(&self) -> &HashMap<InstrumentId, Decimal> {
        &self.account.positions
    }

    /// Current free cash balance (INR).
    pub fn cash(&self) -> Decimal {
        self.account.cash
    }

    /// Equity = cash + Σ(position qty × last price).
    pub fn equity(&self) -> Decimal {
        let positions = self
            .account
            .positions
            .iter()
            .fold(Decimal::ZERO, |acc, (inst, qty)| {
                let last = self
                    .last_prices
                    .get(inst)
                    .map(|p| p.value())
                    .unwrap_or(Decimal::ZERO);
                acc + qty * last
            });
        self.account.cash + positions
    }

    /// Current best bid/ask spread for an instrument, if a book exists.
    pub fn depth(&self, instrument: &InstrumentId) -> Option<(Option<Price>, Option<Price>)> {
        self.books.get(instrument).map(|b| b.get_spread())
    }

    /// Last known price for an instrument (crate-internal read for the executor).
    pub(crate) fn last_price(&self, instrument: &InstrumentId) -> Option<Price> {
        self.last_prices.get(instrument).copied()
    }

    /// Iterate every tracked order (crate-internal read for the executor).
    pub(crate) fn working_orders(&self) -> impl Iterator<Item = (&String, &WorkingOrder)> {
        self.orders.iter()
    }

    // ------------------------------------------------------------------
    // Matching engine
    // ------------------------------------------------------------------

    /// Deterministic price-time priority matching for `order_id`.
    ///
    /// 1. Match against contra resting participants (better price first;
    ///    within a price, `by_instrument` arrival order). The aggressor
    ///    receives price improvement: fills at the participant's resting price.
    /// 2. Match the remainder against feed-book depth (market or limit,
    ///    participation-capped), applying adverse-fill slippage when configured.
    /// 3. IOC remainder → cancel; otherwise rest (`PartiallyFilled` once
    ///    partially filled, else `Working`).
    fn match_order(&mut self, order_id: &str, now: DateTime<Utc>) {
        let (instrument, side, tif, limit) = match self.orders.get(order_id) {
            Some(o) => (
                o.instrument.clone(),
                o.side,
                o.tif,
                o.order_type.limit_price(),
            ),
            None => return,
        };
        // Armed stops (`New`) and terminal orders never match; `StopTriggered`
        // orders (just breached) match as their converted Market/Limit type.
        let state = match self.orders.get(order_id) {
            Some(o) => o.state,
            None => return,
        };
        if !matches!(
            state,
            OrderState::Working | OrderState::PartiallyFilled | OrderState::StopTriggered
        ) {
            return;
        }

        // --- Rule 1: contra resting participants, price-time priority ---
        let order_ids: Vec<String> = self
            .by_instrument
            .get(&instrument)
            .cloned()
            .unwrap_or_default();
        let mut contra: Vec<(Price, usize, String)> = Vec::new();
        for (idx, pid) in order_ids.iter().enumerate() {
            if pid == order_id {
                continue;
            }
            if let Some(participant) = self.orders.get(pid) {
                if participant.side == side {
                    continue;
                }
                if !matches!(
                    participant.state,
                    OrderState::Working | OrderState::PartiallyFilled
                ) {
                    continue;
                }
                if let Some(price) = participant.order_type.limit_price() {
                    contra.push((price, idx, pid.clone()));
                }
            }
        }
        match side {
            OrderSide::Buy => contra.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1))),
            OrderSide::Sell => contra.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1))),
        }

        for (contra_price, _, contra_id) in contra {
            if let Some(limit_price) = limit {
                if (side == OrderSide::Buy && contra_price.value() > limit_price.value())
                    || (side == OrderSide::Sell && contra_price.value() < limit_price.value())
                {
                    break;
                }
            }
            let take = {
                let working = match self.orders.get(order_id) {
                    Some(o) => o,
                    None => break,
                };
                if !working.is_resting() && working.state != OrderState::StopTriggered {
                    break;
                }
                let participant = match self.orders.get(&contra_id) {
                    Some(o) => o,
                    None => continue,
                };
                if !matches!(
                    participant.state,
                    OrderState::Working | OrderState::PartiallyFilled
                ) {
                    continue;
                }
                let take = working
                    .remaining_qty
                    .value()
                    .min(participant.remaining_qty.value());
                if take <= Decimal::ZERO {
                    continue;
                }
                take
            };
            self.apply_participant_fill(
                order_id,
                &contra_id,
                Quantity::try_new(take).unwrap_or(Quantity::ZERO),
                contra_price,
                now,
            );
            let done = self
                .orders
                .get(order_id)
                .map(|o| o.state == OrderState::Filled)
                .unwrap_or(true);
            if done {
                break;
            }
        }

        // --- Rule 2: feed-book depth for the remainder ---
        let remaining = self
            .orders
            .get(order_id)
            .map(|o| o.remaining_qty.value())
            .unwrap_or(Decimal::ZERO);
        if remaining > Decimal::ZERO {
            let remaining_i64 = remaining.to_i64().unwrap_or(0);
            if remaining_i64 > 0 {
                // Snapshot contra levels before execution (adverse-fill input).
                let contra_levels = self.books.get(&instrument).map(|b| match side {
                    OrderSide::Buy => b.asks().levels().clone(),
                    OrderSide::Sell => b.bids().levels().clone(),
                });
                let is_limit = self
                    .orders
                    .get(order_id)
                    .and_then(|o| o.order_type.limit_price());
                let (filled_i64, mut fill_price, _rem_i64) = {
                    let book = self
                        .books
                        .entry(instrument.clone())
                        .or_insert_with(|| book::OrderBook::new(0, Decimal::ONE));
                    match is_limit {
                        Some(limit_price) => {
                            let (filled, price, remaining) =
                                book.execute_limit_order(side, remaining_i64, limit_price);
                            (filled, price, remaining)
                        }
                        None => {
                            let (filled, price, remaining, _worst) =
                                book.execute_market_order(side, remaining_i64);
                            (filled, price, remaining)
                        }
                    }
                };
                if filled_i64 > 0 {
                    if let (Some(adverse), Some(levels)) =
                        (self.adverse_fill.as_ref(), contra_levels.as_ref())
                    {
                        let (_, worst) = adverse.compute_fill_slippage(
                            side,
                            filled_i64,
                            levels,
                            fill_price.value(),
                        );
                        if let Some(worst) = worst {
                            fill_price = worst;
                        }
                    }
                    let filled_qty = Decimal::from(filled_i64);
                    let notional = fill_price.value() * filled_qty;
                    let fee = self.cost_fn.compute(side, notional);
                    self.apply_account(&instrument, side, notional, filled_qty, fee);
                    self.update_order_fill(
                        order_id,
                        Quantity::try_new(filled_qty).unwrap_or(Quantity::ZERO),
                        fill_price,
                        fee,
                        now,
                    );
                }
            }
        }

        // --- Rule 3: IOC remainder cancel / rest ---
        let (remaining, filled) = self
            .orders
            .get(order_id)
            .map(|o| (o.remaining_qty.value(), o.filled_qty.value()))
            .unwrap_or((Decimal::ZERO, Decimal::ZERO));
        if remaining > Decimal::ZERO {
            if tif == TimeInForce::Ioc {
                self.cancel_remaining(order_id, now);
            } else if let Some(working) = self.orders.get_mut(order_id) {
                working.state = if filled > Decimal::ZERO {
                    OrderState::PartiallyFilled
                } else {
                    OrderState::Working
                };
            }
        } else if let Some(working) = self.orders.get_mut(order_id) {
            if working.state == OrderState::Filled {
                self.remove_from_by_instrument(order_id);
            }
        }
    }

    /// Apply a single price-improved fill of `take` at `price` between the
    /// aggressor and a resting participant: cash, positions, order state,
    /// fills, and events for both sides.
    fn apply_participant_fill(
        &mut self,
        aggressor_id: &str,
        participant_id: &str,
        take: Quantity,
        price: Price,
        now: DateTime<Utc>,
    ) {
        let notional = price.value() * take.value();
        let (aggressor_side, participant_side, instrument) = {
            let aggressor = match self.orders.get(aggressor_id) {
                Some(o) => o,
                None => return,
            };
            let participant = match self.orders.get(participant_id) {
                Some(o) => o,
                None => return,
            };
            (
                aggressor.side,
                participant.side,
                aggressor.instrument.clone(),
            )
        };
        let aggressor_fee = self.cost_fn.compute(aggressor_side, notional);
        let participant_fee = self.cost_fn.compute(participant_side, notional);
        self.apply_account(
            &instrument,
            aggressor_side,
            notional,
            take.value(),
            aggressor_fee,
        );
        self.apply_account(
            &instrument,
            participant_side,
            notional,
            take.value(),
            participant_fee,
        );
        self.update_order_fill(aggressor_id, take, price, aggressor_fee, now);
        self.update_order_fill(participant_id, take, price, participant_fee, now);
    }

    /// Update one order's fill state (VWAP avg price, filled/remaining qty),
    /// record the fill report, emit the Fill/PartialFill event, and drop the
    /// order from the working list when filled.
    fn update_order_fill(
        &mut self,
        order_id: &str,
        take: Quantity,
        price: Price,
        fee: Decimal,
        now: DateTime<Utc>,
    ) {
        let (is_filled, snapshot) = {
            let working = match self.orders.get_mut(order_id) {
                Some(o) => o,
                None => return,
            };
            let filled_before = working.filled_qty.value();
            let new_filled = filled_before + take.value();
            let new_remaining = working.remaining_qty.value() - take.value();
            let avg = if new_filled > Decimal::ZERO {
                ((working.avg_fill_price.value() * filled_before) + (price.value() * take.value()))
                    / new_filled
            } else {
                working.avg_fill_price.value()
            };
            working.filled_qty = Quantity::try_new(new_filled).unwrap_or(working.filled_qty);
            working.remaining_qty =
                Quantity::try_new(new_remaining.max(Decimal::ZERO)).unwrap_or(Quantity::ZERO);
            working.avg_fill_price =
                Price::try_new(avg.round_dp(4)).unwrap_or(working.avg_fill_price);
            let is_filled = new_remaining <= Decimal::ZERO;
            working.state = if is_filled {
                OrderState::Filled
            } else if new_filled > Decimal::ZERO {
                OrderState::PartiallyFilled
            } else {
                OrderState::Working
            };
            let snapshot = (
                working.instrument.clone(),
                working.side,
                working.client_order_id,
            );
            (is_filled, snapshot)
        };
        let fill_seq = self.fill_seq;
        self.fill_seq += 1;
        let fill = FillReport {
            fill_id: format!("F{fill_seq:06}"),
            order_id: order_id.to_string(),
            client_order_id: snapshot.2,
            instrument: snapshot.0,
            side: snapshot.1,
            price,
            quantity: take,
            algo_id: "paper".to_string(),
            timestamp: now,
            correlation_id: Some(snapshot.2),
            client_id: None,
            account_id: Some(self.account_id.0.clone()),
            fee: Money::inr(fee),
            fee_breakdown: None,
            trace_context: None,
        };
        self.fill_seq += 1;
        self.fills.push(fill.clone());
        self.events.push(if is_filled {
            ExchangeEvent::Fill { fill }
        } else {
            ExchangeEvent::PartialFill { fill }
        });
        if is_filled {
            self.remove_from_by_instrument(order_id);
        }
    }

    /// Cancel whatever remains of an order (IOC remainder, GTD expiry).
    fn cancel_remaining(&mut self, order_id: &str, now: DateTime<Utc>) {
        if let Some(working) = self.orders.get_mut(order_id) {
            working.state = OrderState::Cancelled;
        }
        self.events.push(ExchangeEvent::Cancelled {
            broker_order_id: order_id.to_string(),
            timestamp: now,
        });
        self.remove_from_by_instrument(order_id);
    }

    /// Arm a stop order into execution mode once its trigger is breached:
    /// emit `StopTriggered` and convert StopLoss → Market, StopLimit → Limit.
    fn trigger_stop(&mut self, order_id: &str, now: DateTime<Utc>) {
        if let Some(working) = self.orders.get_mut(order_id) {
            working.state = OrderState::StopTriggered;
            working.order_type = match working.order_type {
                OrderType::StopLoss(_) => OrderType::Market,
                OrderType::StopLimit { limit, .. } => OrderType::Limit(limit),
                other => other,
            };
        }
        self.events.push(ExchangeEvent::StopTriggered {
            broker_order_id: order_id.to_string(),
            timestamp: now,
        });
    }

    /// Apply a cash + position update for one side of a fill.
    fn apply_account(
        &mut self,
        instrument: &InstrumentId,
        side: OrderSide,
        notional: Decimal,
        qty: Decimal,
        fee: Decimal,
    ) {
        match side {
            OrderSide::Buy => {
                self.account.cash -= notional + fee;
                let entry = self
                    .account
                    .positions
                    .entry(instrument.clone())
                    .or_insert(Decimal::ZERO);
                *entry += qty;
            }
            OrderSide::Sell => {
                self.account.cash += notional - fee;
                let entry = self
                    .account
                    .positions
                    .entry(instrument.clone())
                    .or_insert(Decimal::ZERO);
                *entry -= qty;
            }
        }
    }

    /// Returns `true` when `instrument` has a working/partially-filled contra
    /// resting participant for `side` (used for market-order liquidity checks).
    fn has_contra_participant(&self, instrument: &InstrumentId, side: OrderSide) -> bool {
        self.by_instrument
            .get(instrument)
            .map(|ids| {
                ids.iter().any(|id| {
                    self.orders.get(id).is_some_and(|o| {
                        o.side != side
                            && matches!(o.state, OrderState::Working | OrderState::PartiallyFilled)
                            && o.order_type.limit_price().is_some()
                    })
                })
            })
            .unwrap_or(false)
    }

    /// Drop an order id from its instrument's working list (if present).
    fn remove_from_by_instrument(&mut self, order_id: &str) {
        let instrument = self.orders.get(order_id).map(|o| o.instrument.clone());
        if let Some(instrument) = instrument
            && let Some(list) = self.by_instrument.get_mut(&instrument)
        {
            list.retain(|id| id != order_id);
        }
    }
}
