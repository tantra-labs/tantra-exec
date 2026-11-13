//! Angel One SmartStream order-update WebSocket feed.
//!
//! SmartStream (`wss://smartapisocket.angelone.in/smartstream`) pushes JSON
//! order-update frames to the client authenticated with the `feed_token` from
//! the SmartAPI session. This module mirrors the `UpstoxOrderStream` shape:
//! [`AngelOneOrderStream`] is an [`ExecutionStream`] fed by text frames from
//! an unbounded channel (testable offline), and `spawn_order_updates` drives
//! the live socket with a reconnect/backoff loop and ping keepalive.
//!
//! Fills are delta-tracked on `filledqty`: a `COMPLETE`/partial-fill frame
//! only emits a [`BrokerExecutionEvent::Fill`] when the cumulative filled
//! quantity has grown, so a partial-then-complete sequence yields two fills.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::mpsc;

use tantra_core::{FillReport, InstrumentId, Money, OrderAck, OrderSide, Price, Quantity};
use tantra_exec_core::wire;
use tantra_exec_core::{BrokerError, BrokerExecutionEvent, ExecutionStream};

/// Default SmartStream order-update feed endpoint.
pub const ANGEL_WS_URL: &str = "wss://smartapisocket.angelone.in/smartstream";

/// One order update from the SmartStream feed.
///
/// Field names are the documented camelCase SmartAPI keys; aliases cover the
/// snake_case variants seen in the REST order book.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AngelOneFeedUpdate {
    #[serde(default, rename = "orderid", alias = "order_id")]
    pub order_id: Option<String>,
    #[serde(default, rename = "tradingsymbol")]
    pub trading_symbol: Option<String>,
    #[serde(default, rename = "symboltoken")]
    pub symbol_token: Option<String>,
    #[serde(default, rename = "exch", alias = "exchange")]
    pub exchange: Option<String>,
    #[serde(default, rename = "transactiontype")]
    pub transaction_type: Option<String>,
    #[serde(default, rename = "ordertype")]
    pub order_type: Option<String>,
    #[serde(default, rename = "producttype")]
    pub product_type: Option<String>,
    #[serde(default)]
    pub quantity: Option<Value>,
    #[serde(default)]
    pub price: Option<Value>,
    #[serde(default, rename = "triggerprice", alias = "trigger_price")]
    pub trigger_price: Option<Value>,
    #[serde(default, rename = "filledqty", alias = "filledquantity")]
    pub filled_quantity: Option<Value>,
    #[serde(default, rename = "averageprice", alias = "average_price")]
    pub average_price: Option<Value>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default, rename = "statusmessage", alias = "status_message")]
    pub status_message: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(
        default,
        rename = "ordergenerationtime",
        alias = "order_generation_time"
    )]
    pub order_generation_time: Option<String>,
    #[serde(default, rename = "updatetime", alias = "update_time")]
    pub update_time: Option<String>,
    /// SEBI algo id carried by the order.
    #[serde(default, rename = "ordertag", alias = "tag")]
    pub order_tag: Option<String>,
}

/// Parse a SmartStream text frame into order updates.
///
/// Frames carry a single object or an array; an empty frame yields no updates
/// and malformed JSON fails the stream.
pub fn parse_order_feed_frame(text: &str) -> Result<Vec<AngelOneFeedUpdate>, BrokerError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|e| BrokerError::InvalidRequest(format!("angelone feed frame not JSON: {e}")))?;

    match value {
        Value::Array(items) => items
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<AngelOneFeedUpdate>, _>>()
            .map_err(|e| {
                BrokerError::InvalidRequest(format!("angelone feed array element not order: {e}"))
            }),
        Value::Object(_) => {
            let update: AngelOneFeedUpdate = serde_json::from_value(value).map_err(|e| {
                BrokerError::InvalidRequest(format!("angelone feed frame shape: {e}"))
            })?;
            Ok(vec![update])
        }
        _ => Err(BrokerError::InvalidRequest(
            "angelone feed frame must be a JSON object or array".into(),
        )),
    }
}

fn feed_instrument(update: &AngelOneFeedUpdate) -> InstrumentId {
    let exchange = crate::mapping::exchange(update.exchange.as_deref().unwrap_or_default());
    let symbol = update
        .trading_symbol
        .as_deref()
        .unwrap_or_default()
        .to_uppercase();
    let mut instrument = InstrumentId::new(exchange, symbol);
    if let Some(token) = update.symbol_token.as_deref() {
        instrument = instrument.with_token(token.to_string());
    }
    instrument
}

fn feed_side(update: &AngelOneFeedUpdate) -> OrderSide {
    crate::mapping::order_side(update.transaction_type.as_deref().unwrap_or_default())
}

fn feed_timestamp(update: &AngelOneFeedUpdate) -> DateTime<Utc> {
    let raw = update
        .order_generation_time
        .as_deref()
        .or(update.update_time.as_deref());
    let parsed = raw.and_then(|s| {
        wire::parse_broker_ist_datetime(s).or_else(|| {
            // SmartStream sometimes emits a bare time-of-day anchored at the
            // current IST date.
            wire::parse_ist_time_of_day(s, wire::ist_today(Utc::now()))
        })
    });
    parsed.unwrap_or_else(Utc::now)
}

fn feed_fill_price(update: &AngelOneFeedUpdate) -> Price {
    let average = update
        .average_price
        .as_ref()
        .and_then(wire::price_from_json)
        .filter(|p| !p.0.is_zero());
    if let Some(price) = average {
        return price;
    }
    update
        .price
        .as_ref()
        .and_then(wire::price_from_json)
        .filter(|p| !p.0.is_zero())
        .unwrap_or(Price::ZERO)
}

/// Broker-order-id → client-order-id resolution used by the stream.
pub type FeedCorrelationLookup = Arc<dyn Fn(&str) -> Option<uuid::Uuid> + Send + Sync>;

/// Angel One order-feed [`ExecutionStream`].
///
/// Delta-tracks cumulative `filled_quantity` exactly like the Upstox/Zerodha
/// streams, and supports injectable broker-order-id → client-correlation
/// resolution.
pub struct AngelOneOrderStream {
    frame_rx: mpsc::UnboundedReceiver<String>,
    pending: VecDeque<BrokerExecutionEvent>,
    fills: HashMap<String, u64>,
    client_order_id_of: FeedCorrelationLookup,
}

impl AngelOneOrderStream {
    /// Stream fed by text frames from a channel.
    pub fn new(frame_rx: mpsc::UnboundedReceiver<String>) -> Self {
        Self {
            frame_rx,
            pending: VecDeque::new(),
            fills: HashMap::new(),
            client_order_id_of: Arc::new(|_| None),
        }
    }

    /// Inject a broker-order-id → client-order-id resolver (nil marker when
    /// unknown).
    pub fn with_correlation_lookup(
        mut self,
        lookup: impl Fn(&str) -> Option<uuid::Uuid> + Send + Sync + 'static,
    ) -> Self {
        self.client_order_id_of = Arc::new(lookup);
        self
    }

    fn dispatch(&mut self, update: AngelOneFeedUpdate) -> Option<BrokerExecutionEvent> {
        let order_id = update.order_id.clone().unwrap_or_default();
        if order_id.is_empty() {
            return None;
        }
        let client_order_id = (self.client_order_id_of)(&order_id).unwrap_or(uuid::Uuid::nil());
        let status = update.status.as_deref().unwrap_or_default().to_uppercase();
        match status.as_str() {
            "OPEN" | "TRIGGER PENDING" | "PENDING" | "NEW" | "PUT ORDER REQ RECEIVED" => {
                Some(BrokerExecutionEvent::OrderAck(OrderAck {
                    order_id,
                    client_order_id,
                    exchange_order_id: None,
                    status: crate::mapping::order_status(&status),
                    timestamp: feed_timestamp(&update),
                    correlation_id: None,
                    client_id: None,
                    trace_context: None,
                }))
            }
            "COMPLETE" | "PARTIAL FILL" | "PARTIAL_FILL" => {
                let filled = update
                    .filled_quantity
                    .as_ref()
                    .and_then(wire::integer_from_json)
                    .unwrap_or(0)
                    .max(0) as u64;
                let previous = self.fills.insert(order_id.clone(), filled).unwrap_or(0);
                if filled <= previous {
                    return None;
                }
                Some(BrokerExecutionEvent::Fill(FillReport {
                    fill_id: format!("{order_id}:{filled}"),
                    order_id,
                    client_order_id,
                    instrument: feed_instrument(&update),
                    side: feed_side(&update),
                    price: feed_fill_price(&update),
                    quantity: Quantity::from_u64(filled - previous),
                    algo_id: update.order_tag.clone().unwrap_or_default(),
                    timestamp: feed_timestamp(&update),
                    correlation_id: None,
                    client_id: None,
                    account_id: None,
                    fee: Money::zero_inr(),
                    fee_breakdown: None,
                    trace_context: None,
                }))
            }
            "REJECTED" => Some(BrokerExecutionEvent::OrderRejected {
                order_id: Some(order_id),
                reason: update
                    .status_message
                    .clone()
                    .or(update.message.clone())
                    .unwrap_or_else(|| "rejected by broker".into()),
            }),
            "CANCELLED" | "CANCELED" => Some(BrokerExecutionEvent::OrderCancelled {
                order_id,
                reason: update.status_message.clone().or(update.message.clone()),
            }),
            _ => None,
        }
    }
}

#[async_trait]
impl ExecutionStream for AngelOneOrderStream {
    async fn next_event(&mut self) -> Result<BrokerExecutionEvent, BrokerError> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(event);
            }
            let frame = self
                .frame_rx
                .recv()
                .await
                .ok_or_else(|| BrokerError::Transport("angelone feed stream closed".into()))?;
            let started = Instant::now();
            let updates = parse_order_feed_frame(&frame)?;
            for update in updates {
                let ws_span = tracing::info_span!(
                    "broker_websocket_event",
                    broker = "angelone",
                    order_id = %update.order_id.as_deref().unwrap_or(""),
                    status = %update.status.as_deref().unwrap_or(""),
                );
                let _guard = ws_span.enter();
                if let Some(event) = self.dispatch(update) {
                    self.pending.push_back(event);
                }
            }
            tantra_exec_core::telemetry::record_websocket_latency("angelone", started.elapsed());
        }
    }
}

/// Feed connection behaviour knobs.
#[derive(Debug, Clone)]
pub struct FeedConnectConfig {
    /// SmartAPI `feed_token` (Authorization: Bearer …) on the handshake.
    pub feed_token: String,
    /// Max reconnect attempts per epoch.
    pub max_attempts: u32,
    /// Initial reconnect delay (doubles up to `max_backoff`).
    pub backoff: Duration,
    /// Reconnect delay cap.
    pub max_backoff: Duration,
    /// Application-level ping interval.
    pub keepalive: Duration,
}

impl FeedConnectConfig {
    /// Config with production defaults for the given feed token.
    pub fn new(feed_token: impl Into<String>) -> Self {
        Self {
            feed_token: feed_token.into(),
            max_attempts: 8,
            backoff: Duration::from_secs(2),
            max_backoff: Duration::from_secs(30),
            keepalive: Duration::from_secs(5),
        }
    }
}

/// Connect to the Angel One SmartStream order feed and spawn the read loop.
pub async fn spawn_order_updates(feed_token: &str) -> Result<AngelOneOrderStream, BrokerError> {
    spawn_order_updates_with(FeedConnectConfig::new(feed_token)).await
}

/// Same as [`spawn_order_updates`] with explicit connection knobs.
pub async fn spawn_order_updates_with(
    config: FeedConnectConfig,
) -> Result<AngelOneOrderStream, BrokerError> {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION as WS_AUTH;

    let (tx, rx) = mpsc::unbounded_channel::<String>();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Result<(), BrokerError>>();

    tokio::spawn(async move {
        let mut delay = config.backoff;
        let mut ready = Some(ready_tx);
        for attempt in 1..=config.max_attempts {
            let connect = || async {
                let mut request = ANGEL_WS_URL.into_client_request().map_err(|e| {
                    BrokerError::Transport(format!("angelone feed request build: {e}"))
                })?;
                request.headers_mut().insert(
                    WS_AUTH,
                    format!("Bearer {}", config.feed_token)
                        .parse()
                        .map_err(|e| {
                            BrokerError::Auth(format!("angelone feed auth header: {e}"))
                        })?,
                );
                let (ws, _) = tokio_tungstenite::connect_async(request)
                    .await
                    .map_err(|err| {
                        BrokerError::Transport(format!("angelone feed connect failed: {err}"))
                    })?;
                Ok(ws)
            };
            let socket = match connect().await {
                Ok(socket) => socket,
                Err(err) => {
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(Err(err));
                    }
                    tracing::warn!(
                        attempt,
                        "angelone feed connect failed; retrying in {:?}",
                        delay
                    );
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(config.max_backoff);
                    continue;
                }
            };
            if let Some(ready) = ready.take() {
                let _ = ready.send(Ok(()));
            }
            let (mut write, mut read) = socket.split();
            let mut keepalive = tokio::time::interval(config.keepalive);
            let mut connected = true;
            while connected {
                tokio::select! {
                    _ = keepalive.tick() => {
                        if write.send(Message::Ping(b"tantra".as_slice().into())).await.is_err() {
                            connected = false;
                        }
                    }
                    frame = read.next() => {
                        match frame {
                            Some(Ok(Message::Text(text))) => {
                                if tx.send(text.to_string()).is_err() {
                                    connected = false;
                                }
                            }
                            Some(Ok(_)) => {
                                // Binary market-tick frames carry no order content.
                            }
                            Some(Err(err)) => {
                                tracing::warn!(%err, "angelone feed read error");
                                connected = false;
                            }
                            None => connected = false,
                        }
                    }
                }
            }
            delay = (delay * 2).min(config.max_backoff);
            tokio::time::sleep(delay).await;
        }
        drop(tx);
    });

    match ready_rx.await {
        Ok(Ok(())) => Ok(AngelOneOrderStream::new(rx)),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(BrokerError::Transport(
            "angelone feed connector died".into(),
        )),
    }
}
