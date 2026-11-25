//! Angel One SmartAPI 2.0 execution adapter.
//!
//! [`AngelOneExecutor`] is the [`BrokerExecutor`] implementation for the
//! Angel One SmartAPI 2.0 REST gateway (`https://apiconnect.angelone.in`).
//! It consumes [`SessionTokens`] produced by `tantra_exec_auth`'s
//! `AngelOneSmartLogin` flow (access token for REST, feed token for the
//! SmartStream order feed) and speaks the documented SmartAPI envelope
//! (`status` / `message` / `errorcode` / `data`).
//!
//! Every outbound price, trigger and quantity is serialized as an exact
//! decimal string (`format_fixed_exact` / `quantity_to_u32_exact`) — no
//! floating point anywhere on the wire. The companion [`ws`] module provides
//! the SmartStream order-update feed.
//!
//! // stand-in for tantra-exec/docs/gaps/0001-angelone-adapter.md
//! // grouped under tantra-exec/docs/plans/0002-milestone3-adapter-completion.md

pub mod mapping;
pub mod ws;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::time::Duration;
use uuid::Uuid;

use tantra_core::{
    InstrumentId, Money, OrderAck, OrderRequest, OrderSide, OrderStatus, OrderType, Price,
    Quantity, TimeInForce,
};
use tantra_exec_auth::tokens::SessionTokens;
use tantra_exec_core::decimal::{format_fixed_exact, quantity_to_u32_exact};
use tantra_exec_core::types::ProductType;
use tantra_exec_core::wire;
use tantra_exec_core::{
    BrokerError, BrokerExecutor, BrokerHealth, BrokerMargins, BrokerPosition, BrokerRateCatalog,
    DecodedOrderTag, OrderSnapshot, OrderTag, PositionSide, RateLimitedBroker,
};

/// Default SmartAPI 2.0 REST gateway.
pub const DEFAULT_BASE_URL: &str = "https://apiconnect.angelone.in";

const PLACE_ORDER_PATH: &str = "/rest/secure/angelbroking/order/v1/placeOrder";
const MODIFY_ORDER_PATH: &str = "/rest/secure/angelbroking/order/v1/modifyOrder";
const CANCEL_ORDER_PATH: &str = "/rest/secure/angelbroking/order/v1/cancelOrder";
const ORDER_BOOK_PATH: &str = "/rest/secure/angelbroking/order/v1/getOrderBook";
const POSITIONS_PATH: &str = "/rest/secure/angelbroking/order/v1/getPosition";
const RMS_PATH: &str = "/rest/secure/angelbroking/user/v1/getRMS";
const PROFILE_PATH: &str = "/rest/secure/angelbroking/user/v1/getProfile";

/// Angel One `ordertype` token for a canonical [`OrderType`].
pub fn order_type_token(order_type: &OrderType) -> &'static str {
    match order_type {
        OrderType::Market => "MARKET",
        OrderType::Limit(_) => "LIMIT",
        OrderType::StopLoss(_) => "STOPLOSS_MARKET",
        OrderType::StopLimit { .. } => "STOPLOSS_LIMIT",
    }
}

/// Exact 2-scale `price` / `triggerprice` legs for a canonical [`OrderType`].
///
/// `format_fixed_exact` rejects values that cannot be represented exactly at
/// two decimal places, so the returned strings are byte-exact on the wire.
fn order_price_legs(order_type: &OrderType) -> Result<(String, String), BrokerError> {
    Ok(match order_type {
        OrderType::Market => ("0.00".to_string(), "0.00".to_string()),
        OrderType::Limit(price) => (format_fixed_exact(price.value(), 2)?, "0.00".to_string()),
        OrderType::StopLoss(trigger) => {
            ("0.00".to_string(), format_fixed_exact(trigger.value(), 2)?)
        }
        OrderType::StopLimit { trigger, limit } => (
            format_fixed_exact(limit.value(), 2)?,
            format_fixed_exact(trigger.value(), 2)?,
        ),
    })
}

/// Build the SmartAPI `placeOrder` JSON payload for a canonical order.
///
/// Prices and triggers are exact 2-scale decimal strings, quantity is a u32,
/// and the `symboltoken` falls back to the symbol when no token is attached.
/// Every placement carries the idempotent 20-char Base32 correlation `tag`
/// derived from [`OrderRequest::client_order_id`].
pub fn build_order_payload(req: &OrderRequest, product: ProductType) -> Result<Value, BrokerError> {
    let quantity = quantity_to_u32_exact(req.quantity.value())?;
    let ordertype = order_type_token(&req.order_type);
    let (price, triggerprice) = order_price_legs(&req.order_type)?;
    let transactiontype = match &req.side {
        OrderSide::Buy => "BUY",
        OrderSide::Sell => "SELL",
    };
    let duration = match &req.tif {
        TimeInForce::Ioc => "IOC",
        _ => "DAY",
    };
    let symboltoken = req
        .instrument
        .token()
        .map(str::to_string)
        .unwrap_or_else(|| req.instrument.symbol().to_string());

    let mut payload = serde_json::json!({
        "variety": "NORMAL",
        "tradingsymbol": req.instrument.symbol(),
        "symboltoken": symboltoken,
        "exchange": req.instrument.exchange().as_str(),
        "transactiontype": transactiontype,
        "ordertype": ordertype,
        "producttype": product.angelone(),
        "quantity": quantity,
        "price": price,
        "triggerprice": triggerprice,
        "disclosedquantity": "0",
        "duration": duration,
    });
    payload["tag"] = Value::String(OrderTag::from_request(req).wire_tag_20());
    Ok(payload)
}

/// Map a SmartAPI envelope-level rejection (`status != true`) to a
/// [`BrokerError`]: auth/session problems become [`BrokerError::Auth`], rate
/// messages become [`BrokerError::RateLimited`], everything else is a
/// rejection.
fn map_envelope_error(message: &str, errorcode: &str) -> BrokerError {
    let needle = format!("{message} {errorcode}").to_lowercase();
    if needle.contains("401")
        || needle.contains("token")
        || needle.contains("unauthor")
        || needle.contains("session")
        || needle.contains("login")
    {
        BrokerError::Auth(message.to_string())
    } else if needle.contains("429") || needle.contains("rate") || needle.contains("too many") {
        BrokerError::RateLimited(message.to_string())
    } else {
        BrokerError::Rejected(message.to_string())
    }
}

/// True when a cancel failure means the order was already cancelled (idempotent
/// success per the adapter contract).
fn is_already_cancelled(err: &BrokerError) -> bool {
    let BrokerError::Rejected(message) = err else {
        return false;
    };
    let needle = message.to_lowercase();
    (needle.contains("already") && needle.contains("cancel"))
        || needle.contains("cannot be cancelled")
}

/// Map a cancel failure: unknown orders become [`BrokerError::OrderNotFound`],
/// everything else passes through.
fn map_cancel_error(order_id: &str, err: BrokerError) -> BrokerError {
    let not_found = match &err {
        BrokerError::Rejected(message) => {
            let needle = message.to_lowercase();
            needle.contains("not found")
                || needle.contains("invalid order")
                || needle.contains("no such order")
        }
        _ => false,
    };
    if not_found {
        BrokerError::OrderNotFound(order_id.to_string())
    } else {
        err
    }
}

/// Build a canonical [`InstrumentId`] from a REST record's exchange /
/// tradingsymbol / symboltoken fields.
fn instrument_from_json(row: &Value) -> Result<InstrumentId, BrokerError> {
    let exchange = mapping::exchange(
        row.get("exchange")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let symbol = row
        .get("tradingsymbol")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_uppercase();
    let mut instrument = InstrumentId::try_new(exchange, symbol)
        .map_err(|e| BrokerError::Transport(format!("invalid angelone instrument: {e}")))?;
    if let Some(token) = row.get("symboltoken").and_then(Value::as_str) {
        instrument = instrument.with_token(token.to_string());
    }
    Ok(instrument)
}

/// Decode one order-book record into an [`OrderSnapshot`].
fn parse_order_snapshot(row: &Value) -> Result<OrderSnapshot, BrokerError> {
    let raw_tag = row
        .get("ordertag")
        .or_else(|| row.get("tag"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let client_order_id = raw_tag
        .as_deref()
        .and_then(DecodedOrderTag::decode)
        .and_then(|decoded| decoded.exact_uuid());
    Ok(OrderSnapshot {
        order_id: row
            .get("orderid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        client_order_id,
        exchange_order_id: row
            .get("exchorderid")
            .and_then(Value::as_str)
            .map(str::to_string),
        tag: raw_tag,
        instrument: instrument_from_json(row)?,
        side: mapping::order_side(
            row.get("transactiontype")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ),
        order_type: mapping::order_type(
            row.get("ordertype")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            row.get("price").cloned(),
            row.get("triggerprice").cloned(),
        ),
        quantity: row
            .get("quantity")
            .and_then(wire::quantity_from_json)
            .unwrap_or(Quantity::ZERO),
        filled_quantity: row
            .get("filledquantity")
            .and_then(wire::quantity_from_json)
            .unwrap_or(Quantity::ZERO),
        price: row.get("price").and_then(wire::price_from_json),
        trigger_price: row.get("triggerprice").and_then(wire::price_from_json),
        status: mapping::order_status(
            row.get("status")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ),
        product: row
            .get("producttype")
            .and_then(Value::as_str)
            .and_then(ProductType::from_angelone),
        timestamp: row
            .get("orderentertime")
            .or_else(|| row.get("updatetime"))
            .and_then(Value::as_str)
            .and_then(wire::parse_broker_ist_datetime)
            .unwrap_or_else(Utc::now),
    })
}

/// Angel One SmartAPI 2.0 [`BrokerExecutor`].
///
/// All REST calls carry the SmartAPI header set built from the authenticated
/// session tokens and static credentials; the wire envelope is decoded to
/// [`BrokerError`] on application-level rejections.
pub struct AngelOneExecutor {
    client: reqwest::Client,
    base_url: String,
    access_token: String,
    feed_token: Option<String>,
    client_code: String,
    api_key: String,
    expires_at: DateTime<Utc>,
    default_product: ProductType,
}

impl AngelOneExecutor {
    /// Request timeout applied to every REST call.
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

    /// Build the executor from a SmartAPI session plus the static credentials.
    ///
    /// The session is obtained from `tantra_exec_auth::login::AngelOneSmartLogin`
    /// (the login flow itself lives there); this constructor consumes the
    /// resulting [`SessionTokens`] — `access_token` for REST and `feed_token`
    /// for the SmartStream order feed — and records the session expiry so
    /// callers can decide when to re-run the login flow.
    pub fn new(
        client_code: impl Into<String>,
        api_key: impl Into<String>,
        tokens: &SessionTokens,
    ) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Self::REQUEST_TIMEOUT)
                .build()
                .expect("reqwest client builds without a TLS backend failure"),
            base_url: DEFAULT_BASE_URL.to_string(),
            access_token: tokens.access_token.clone(),
            feed_token: tokens.feed_token.clone(),
            client_code: client_code.into(),
            api_key: api_key.into(),
            expires_at: tokens.expires_at,
            default_product: ProductType::CncDelivery,
        }
    }

    /// Point the executor at a different gateway root (tests / proxies).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Override the HTTP request timeout applied to REST calls.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client builds without a TLS backend failure");
        self
    }

    /// Override the product type used for orders that carry no product
    /// attribution (defaults to [`ProductType::CncDelivery`]).
    pub fn with_default_product(mut self, product: ProductType) -> Self {
        self.default_product = product;
        self
    }

    /// Feed token for the SmartStream order feed.
    pub fn feed_token(&self) -> Option<&str> {
        self.feed_token.as_deref()
    }

    /// SmartAPI sessions expire at 03:30 IST on the next run; this exposes the
    /// recorded expiry so the caller can re-login in time.
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    /// Whether the SmartAPI session has passed its documented expiry
    /// (recorded from the issuing login flow).
    pub fn is_session_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }

    /// Wrap the executor in a token-bucket rate limiter using the verified
    /// Angel One 10 requests/second preset.
    pub fn rate_limited(self) -> Result<RateLimitedBroker<Self>, BrokerError> {
        RateLimitedBroker::with_config(self, BrokerRateCatalog::for_broker("angelone"))
    }

    /// Build a request with the documented SmartAPI header set.
    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> reqwest::RequestBuilder {
        let mut builder = self
            .client
            .request(method, format!("{}{}", self.base_url, path))
            .bearer_auth(&self.access_token)
            .header("X-ClientCode", &self.client_code)
            .header("X-ApiKey", &self.api_key)
            .header("X-ClientLocalIP", "127.0.0.1")
            .header("X-ClientPublicIP", "127.0.0.1")
            .header("X-MACAddress", "00:00:00:00:00:00")
            .header("X-SourceID", "WEB")
            .header("X-UserType", "USER");
        if let Some(body) = body {
            builder = builder
                .header("Content-Type", "application/json")
                .json(&body);
        }
        builder
    }

    /// Execute a SmartAPI call and return the decoded `data` field.
    ///
    /// HTTP 401/429 map to auth / rate-limit errors; a non-`true` envelope
    /// `status` maps through [`map_envelope_error`].
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, BrokerError> {
        self.call_with_idempotency(method, path, body, None).await
    }

    async fn call_with_idempotency(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        ambiguous_on_io_err: Option<Uuid>,
    ) -> Result<Value, BrokerError> {
        let response = match self.request(method, path, body).send().await {
            Ok(resp) => resp,
            Err(e) => {
                if e.is_connect() {
                    return Err(BrokerError::Transport(format!(
                        "angelone {path} transport: {e}"
                    )));
                }
                if e.is_builder() {
                    return Err(BrokerError::InvalidRequest(format!(
                        "angelone {path} request: {e}"
                    )));
                }
                if let Some(client_order_id) = ambiguous_on_io_err {
                    return Err(BrokerError::OutcomeUnknown {
                        client_order_id,
                        message: format!("angelone {path} post-send error: {e}"),
                    });
                }
                return Err(BrokerError::Transport(format!(
                    "angelone {path} transport: {e}"
                )));
            }
        };
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| match ambiguous_on_io_err {
                Some(client_order_id) => BrokerError::OutcomeUnknown {
                    client_order_id,
                    message: format!("angelone {path} read: {e}"),
                },
                None => BrokerError::Transport(format!("angelone {path} read: {e}")),
            })?;
        let text = String::from_utf8_lossy(&bytes);

        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(BrokerError::Auth(
                "angelone session token rejected (HTTP 401)".into(),
            ));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(BrokerError::RateLimited(
                "angelone rate limited (HTTP 429)".into(),
            ));
        }
        if !status.is_success() {
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_else(|| format!("angelone {path} HTTP {}", status.as_u16()));
            return Err(BrokerError::Rejected(message));
        }

        let envelope: Value = serde_json::from_str(&text)
            .map_err(|e| BrokerError::Transport(format!("angelone {path} envelope decode: {e}")))?;
        let ok = envelope
            .get("status")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !ok {
            let message = envelope
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("angelone request rejected")
                .to_string();
            let errorcode = envelope
                .get("errorcode")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            return Err(map_envelope_error(&message, &errorcode));
        }
        Ok(envelope.get("data").cloned().unwrap_or(Value::Null))
    }
}

#[async_trait]
impl BrokerExecutor for AngelOneExecutor {
    fn name(&self) -> &str {
        "angelone"
    }

    async fn health_check(&self) -> Result<BrokerHealth, BrokerError> {
        match self.call(reqwest::Method::GET, PROFILE_PATH, None).await {
            Ok(data) => {
                let clientcode = data
                    .get("clientcode")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if clientcode.is_empty() {
                    Ok(BrokerHealth {
                        broker: "angelone".into(),
                        session_valid: false,
                        authenticated_user: None,
                        latency: None,
                        detail: "angelone profile returned no clientcode".into(),
                    })
                } else {
                    Ok(BrokerHealth {
                        broker: "angelone".into(),
                        session_valid: true,
                        authenticated_user: Some(clientcode.to_string()),
                        latency: None,
                        detail: "angelone profile ok".into(),
                    })
                }
            }
            Err(err) => Ok(BrokerHealth {
                broker: "angelone".into(),
                session_valid: false,
                authenticated_user: None,
                latency: None,
                detail: err.to_string(),
            }),
        }
    }

    async fn place_order(&self, req: OrderRequest) -> Result<OrderAck, BrokerError> {
        let payload = build_order_payload(&req, self.default_product)?;
        let data = self
            .call_with_idempotency(
                reqwest::Method::POST,
                PLACE_ORDER_PATH,
                Some(payload),
                Some(req.client_order_id),
            )
            .await?;
        let order_id = data
            .get("orderid")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                BrokerError::InvalidRequest("angelone placeOrder returned no orderid".into())
            })?;
        let ack = OrderAck {
            order_id,
            client_order_id: req.client_order_id,
            exchange_order_id: None,
            status: OrderStatus::New,
            timestamp: Utc::now(),
            correlation_id: req.correlation_id,
            client_id: req.client_id.clone(),
            trace_context: req.trace_context,
        };
        tracing::debug!(order_id = %ack.order_id, "angelone order placed");
        Ok(ack)
    }

    async fn cancel_order(&self, order_id: &str) -> Result<(), BrokerError> {
        let payload = serde_json::json!({ "variety": "NORMAL", "orderid": order_id });
        match self
            .call(reqwest::Method::POST, CANCEL_ORDER_PATH, Some(payload))
            .await
        {
            Ok(_) => Ok(()),
            Err(err) if is_already_cancelled(&err) => Ok(()),
            Err(err) => Err(map_cancel_error(order_id, err)),
        }
    }

    async fn modify_order(
        &self,
        order_id: &str,
        new_price: Option<Price>,
        new_quantity: Option<Quantity>,
    ) -> Result<OrderAck, BrokerError> {
        if new_price.is_none() && new_quantity.is_none() {
            return Err(BrokerError::InvalidRequest(
                "angelone modify requires a new price and/or quantity".into(),
            ));
        }
        // Read-then-patch: reuse the current order legs, overriding the legs
        // being modified.
        let current = self
            .fetch_order_book()
            .await?
            .into_iter()
            .find(|snapshot| snapshot.order_id == order_id)
            .ok_or_else(|| BrokerError::OrderNotFound(order_id.to_string()))?;
        let price = new_price.unwrap_or(current.price.unwrap_or(Price::ZERO));
        let trigger = current.trigger_price.unwrap_or(Price::ZERO);
        let quantity = new_quantity.unwrap_or(current.quantity);
        let product = current.product.unwrap_or(self.default_product);
        let symboltoken = current
            .instrument
            .token()
            .map(str::to_string)
            .unwrap_or_else(|| current.instrument.symbol().to_string());

        let payload = serde_json::json!({
            "variety": "NORMAL",
            "orderid": order_id,
            "ordertype": order_type_token(&current.order_type),
            "producttype": product.angelone(),
            "quantity": quantity_to_u32_exact(quantity.value())?,
            "price": format_fixed_exact(price.value(), 2)?,
            "triggerprice": format_fixed_exact(trigger.value(), 2)?,
            "tradingsymbol": current.instrument.symbol(),
            "symboltoken": symboltoken,
            "exchange": current.instrument.exchange().as_str(),
            "duration": "DAY",
        });
        let data = self
            .call(reqwest::Method::POST, MODIFY_ORDER_PATH, Some(payload))
            .await?;
        let new_order_id = data
            .get("orderid")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                BrokerError::InvalidRequest("angelone modifyOrder returned no orderid".into())
            })?;
        Ok(OrderAck::new(
            new_order_id,
            uuid::Uuid::nil(),
            OrderStatus::New,
            Utc::now(),
        ))
    }

    async fn fetch_order_book(&self) -> Result<Vec<OrderSnapshot>, BrokerError> {
        let data = self
            .call(reqwest::Method::GET, ORDER_BOOK_PATH, None)
            .await?;
        let rows = data.as_array().ok_or_else(|| {
            BrokerError::Transport("angelone order book data is not an array".into())
        })?;
        rows.iter().map(parse_order_snapshot).collect()
    }

    async fn fetch_positions(&self) -> Result<Vec<BrokerPosition>, BrokerError> {
        let data = self
            .call(reqwest::Method::GET, POSITIONS_PATH, None)
            .await?;
        let rows = data
            .as_array()
            .ok_or_else(|| {
                BrokerError::Transport("angelone positions data is not an array".into())
            })?
            .as_slice();
        rows.iter()
            .map(|row| {
                let instrument = instrument_from_json(row)?;
                let quantity = row
                    .get("quantity")
                    .and_then(wire::decimal_from_json)
                    .unwrap_or(rust_decimal::Decimal::ZERO);
                let side = if quantity.is_sign_negative() {
                    PositionSide::Short
                } else if quantity.is_zero() {
                    PositionSide::Flat
                } else {
                    PositionSide::Long
                };
                Ok(BrokerPosition {
                    instrument,
                    product: row
                        .get("producttype")
                        .and_then(Value::as_str)
                        .and_then(ProductType::from_angelone)
                        .unwrap_or(ProductType::CncDelivery),
                    side,
                    quantity: Quantity::try_new(quantity.abs()).unwrap_or(Quantity::ZERO),
                    average_price: row
                        .get("avgprice")
                        .and_then(wire::price_from_json)
                        .unwrap_or(Price::ZERO),
                    realized_pnl: Some(Money::inr(
                        row.get("realised")
                            .and_then(wire::decimal_from_json)
                            .unwrap_or(rust_decimal::Decimal::ZERO),
                    )),
                    last_price: row.get("ltp").and_then(wire::price_from_json),
                })
            })
            .collect()
    }

    async fn fetch_margins(&self) -> Result<BrokerMargins, BrokerError> {
        let data = self.call(reqwest::Method::GET, RMS_PATH, None).await?;
        let rows: &[Value] = data.as_array().map(Vec::as_slice).unwrap_or(&[]);
        let Some(row) = rows.first() else {
            return Ok(BrokerMargins {
                available_cash: Money::zero_inr(),
                used_margin: Money::zero_inr(),
                collateral: Money::zero_inr(),
            });
        };
        Ok(BrokerMargins {
            available_cash: Money::inr(
                row.get("availablecash")
                    .and_then(wire::decimal_from_json)
                    .unwrap_or(rust_decimal::Decimal::ZERO),
            ),
            used_margin: Money::inr(
                row.get("marginused")
                    .and_then(wire::decimal_from_json)
                    .unwrap_or(rust_decimal::Decimal::ZERO),
            ),
            collateral: Money::zero_inr(),
        })
    }

    async fn get_order_status(&self, order_id: &str) -> Result<OrderStatus, BrokerError> {
        let book = self.fetch_order_book().await?;
        book.into_iter()
            .find(|snapshot| snapshot.order_id == order_id)
            .map(|snapshot| snapshot.status)
            .ok_or_else(|| BrokerError::OrderNotFound(order_id.to_string()))
    }
}
