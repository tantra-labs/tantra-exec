//! REST conformance tests against a local axum mock of the SmartAPI gateway.
//!
//! The mock serves the documented envelope (`status` / `message` / `errorcode`
//! / `data`) so the executor's decode, error mapping, idempotent `OrderTag`
//! correlation, unknown-outcome classification, and exact-decimal wire handling
//! are exercised end-to-end without a real broker.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use chrono::Utc;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use tantra_core::{
    Exchange, InstrumentId, OrderRequest, OrderSide, OrderStatus, OrderType, Price, Quantity,
};
use tantra_exec_angelone::{AngelOneExecutor, DEFAULT_BASE_URL};
use tantra_exec_auth::tokens::SessionTokens;
use tantra_exec_core::tag::OrderTag;
use tantra_exec_core::{BrokerError, BrokerExecutor, PositionSide};

/// Canned SmartAPI success envelope.
fn envelope(data: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "status": true,
        "message": "SUCCESS",
        "errorcode": "0",
        "data": data,
    })
}

fn rejected_envelope(message: &str, errorcode: &str) -> serde_json::Value {
    serde_json::json!({
        "status": false,
        "message": message,
        "errorcode": errorcode,
        "data": null,
    })
}

fn session_tokens() -> SessionTokens {
    let now = Utc::now();
    SessionTokens {
        access_token: "jwt.access.token".to_string(),
        refresh_token: Some("refresh".to_string()),
        feed_token: Some("feed.token".to_string()),
        issued_at: now,
        expires_at: now + chrono::Duration::hours(6),
    }
}

fn executor(base_url: &str) -> AngelOneExecutor {
    AngelOneExecutor::new("client123", "api-key-1", &session_tokens()).with_base_url(base_url)
}

fn sample_order() -> OrderRequest {
    let instrument = InstrumentId::try_new(Exchange::Nse, "RELIANCE")
        .unwrap()
        .with_token("2885".to_string());
    OrderRequest::new(
        instrument,
        OrderSide::Buy,
        OrderType::Limit(Price::from_rupees("1450.50").unwrap()),
        Quantity::from_u64(25),
        "ALGO-1".to_string(),
    )
}

/// Bind a router on an ephemeral port and return its base URL.
async fn serve(app: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}")
}

/// Server exposing the full read-only surface (order book, positions, margins,
/// profile) used by the decode tests.
fn read_server() -> Router {
    Router::new()
        .route(
            "/rest/secure/angelbroking/order/v1/getOrderBook",
            axum::routing::get(|| async {
                axum::response::Json(envelope(serde_json::json!([
                    {
                        "orderid": "260223110000001",
                        "exchorderid": "110000001",
                        "tradingsymbol": "RELIANCE",
                        "symboltoken": "2885",
                        "exchange": "NSE",
                        "transactiontype": "BUY",
                        "ordertype": "LIMIT",
                        "producttype": "DELIVERY",
                        "quantity": "25",
                        "price": "1450.50",
                        "triggerprice": "0.00",
                        "filledquantity": "0",
                        "averageprice": "0.00",
                        "status": "complete",
                        "orderentertime": "2024-02-21T09:20:00 IST",
                        "ordertag": "ALGO-1"
                    }
                ])))
            }),
        )
        .route(
            "/rest/secure/angelbroking/order/v1/getPosition",
            axum::routing::get(|| async {
                axum::response::Json(envelope(serde_json::json!([
                    {
                        "tradingsymbol": "RELIANCE",
                        "symboltoken": "2885",
                        "exchange": "NSE",
                        "producttype": "DELIVERY",
                        "quantity": "50",
                        "avgprice": "1440.25",
                        "ltp": "1450.50",
                        "realised": "12.50",
                        "unrealised": "5.00"
                    },
                    {
                        "tradingsymbol": "TATAMOTORS",
                        "symboltoken": "3456",
                        "exchange": "NSE",
                        "producttype": "INTRADAY",
                        "quantity": "-25",
                        "avgprice": "990.00",
                        "ltp": "985.00",
                        "realised": "0.00",
                        "unrealised": "0.00"
                    }
                ])))
            }),
        )
        .route(
            "/rest/secure/angelbroking/user/v1/getRMS",
            axum::routing::get(|| async {
                axum::response::Json(envelope(serde_json::json!([
                    { "availablecash": "250000.00", "marginused": "75000.00" }
                ])))
            }),
        )
        .route(
            "/rest/secure/angelbroking/user/v1/getProfile",
            axum::routing::get(|| async {
                axum::response::Json(envelope(serde_json::json!({ "clientcode": "client123" })))
            }),
        )
}

#[tokio::test]
async fn place_order_returns_ack_and_exact_payload() {
    let place_response = envelope(serde_json::json!({
        "orderid": "260223110000001",
        "uniqueorderid": "u1"
    }));
    let log: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    let log_for_route = log.clone();
    let app = Router::new().route(
        "/rest/secure/angelbroking/order/v1/placeOrder",
        axum::routing::post(move |body: axum::body::Bytes| {
            let log = log_for_route.clone();
            let response = place_response.clone();
            async move {
                if let Ok(value) = serde_json::from_slice(&body) {
                    log.lock().await.push(value);
                }
                axum::response::Json(response)
            }
        }),
    );
    let base_url = serve(app).await;
    let executor = executor(&base_url);
    let req = sample_order();
    let expected_tag = OrderTag::from_request(&req).wire_tag_20();
    let ack = executor.place_order(req).await.unwrap();

    assert_eq!(ack.order_id, "260223110000001");
    assert_eq!(ack.status, OrderStatus::New);
    assert_eq!(executor.name(), "angelone");

    let bodies = log.lock().await.clone();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["variety"], "NORMAL");
    assert_eq!(bodies[0]["tradingsymbol"], "RELIANCE");
    assert_eq!(bodies[0]["symboltoken"], "2885");
    assert_eq!(bodies[0]["exchange"], "NSE");
    assert_eq!(bodies[0]["transactiontype"], "BUY");
    assert_eq!(bodies[0]["ordertype"], "LIMIT");
    assert_eq!(bodies[0]["producttype"], "DELIVERY");
    assert_eq!(bodies[0]["quantity"], 25);
    assert_eq!(bodies[0]["price"], "1450.50");
    assert_eq!(bodies[0]["triggerprice"], "0.00");
    assert_eq!(bodies[0]["duration"], "DAY");
    assert_eq!(bodies[0]["tag"], expected_tag);
}

#[tokio::test]
async fn place_order_drop_connection_returns_outcome_unknown() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            drop(socket);
        }
    });

    let req = sample_order();
    let cid = req.client_order_id;
    let err = executor(&format!("http://{addr}"))
        .place_order(req)
        .await
        .unwrap_err();
    match err {
        BrokerError::OutcomeUnknown {
            client_order_id, ..
        } => assert_eq!(client_order_id, cid),
        other => panic!("expected OutcomeUnknown on post-send drop, got {other:?}"),
    }
}

#[tokio::test]
async fn place_order_timeout_returns_outcome_unknown() {
    let app = Router::new().route(
        "/rest/secure/angelbroking/order/v1/placeOrder",
        axum::routing::post(|| async {
            tokio::time::sleep(Duration::from_millis(250)).await;
            axum::response::Json(envelope(serde_json::json!({ "orderid": "LATE" })))
        }),
    );
    let base_url = serve(app).await;
    let req = sample_order();
    let cid = req.client_order_id;
    let err = executor(&base_url)
        .with_timeout(Duration::from_millis(40))
        .place_order(req)
        .await
        .unwrap_err();
    match err {
        BrokerError::OutcomeUnknown {
            client_order_id, ..
        } => assert_eq!(client_order_id, cid),
        other => panic!("expected OutcomeUnknown on post-send timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn place_order_connect_refused_returns_transport() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let err = executor(&format!("http://{addr}"))
        .place_order(sample_order())
        .await
        .unwrap_err();
    assert!(
        matches!(err, BrokerError::Transport(_)),
        "expected Transport on connect refusal, got {err:?}"
    );
}

#[tokio::test]
async fn find_by_client_order_id_reconciles_via_ordertag() {
    let req = sample_order();
    let cid = req.client_order_id;
    let wire_tag = OrderTag::from_request(&req).wire_tag_20();

    let app = Router::new().route(
        "/rest/secure/angelbroking/order/v1/getOrderBook",
        axum::routing::get(move || {
            let tag = wire_tag.clone();
            async move {
                axum::response::Json(envelope(serde_json::json!([{
                    "orderid": "260223110009999",
                    "exchorderid": "110009999",
                    "tradingsymbol": "RELIANCE",
                    "symboltoken": "2885",
                    "exchange": "NSE",
                    "transactiontype": "BUY",
                    "ordertype": "LIMIT",
                    "producttype": "DELIVERY",
                    "quantity": "25",
                    "price": "1450.50",
                    "triggerprice": "0.00",
                    "filledquantity": "0",
                    "averageprice": "0.00",
                    "status": "open",
                    "orderentertime": "2024-02-21T09:20:00 IST",
                    "ordertag": tag
                }])))
            }
        }),
    );
    let base_url = serve(app).await;
    let found = executor(&base_url)
        .find_by_client_order_id(cid)
        .await
        .expect("lookup succeeds")
        .expect("order matched via compact tag");
    assert_eq!(found.order_id, "260223110009999");
}

#[tokio::test]
async fn place_order_envelope_rejection_maps_to_rejected() {
    let app = Router::new().route(
        "/rest/secure/angelbroking/order/v1/placeOrder",
        axum::routing::post(|| async {
            axum::response::Json(rejected_envelope(
                "Order quantity exceeds the maximum allowed",
                "AB1001",
            ))
        }),
    );
    let base_url = serve(app).await;
    let err = executor(&base_url)
        .place_order(sample_order())
        .await
        .unwrap_err();
    assert!(matches!(err, BrokerError::Rejected(_)), "got {err:?}");
}

#[tokio::test]
async fn place_order_http_401_maps_to_auth() {
    let app = Router::new().route(
        "/rest/secure/angelbroking/order/v1/placeOrder",
        axum::routing::post(|| async {
            (
                axum::http::StatusCode::UNAUTHORIZED,
                axum::response::Json(serde_json::json!({
                    "status": false,
                    "message": "Invalid token",
                    "errorcode": "401"
                })),
            )
        }),
    );
    let base_url = serve(app).await;
    let err = executor(&base_url)
        .place_order(sample_order())
        .await
        .unwrap_err();
    assert!(matches!(err, BrokerError::Auth(_)), "got {err:?}");
}

#[tokio::test]
async fn place_order_invalid_token_message_maps_to_auth() {
    let app = Router::new().route(
        "/rest/secure/angelbroking/order/v1/placeOrder",
        axum::routing::post(|| async {
            axum::response::Json(rejected_envelope("Invalid token, please relogin", "AB1002"))
        }),
    );
    let base_url = serve(app).await;
    let err = executor(&base_url)
        .place_order(sample_order())
        .await
        .unwrap_err();
    assert!(matches!(err, BrokerError::Auth(_)), "got {err:?}");
}

#[tokio::test]
async fn place_order_missing_orderid_is_invalid_request() {
    let app = Router::new().route(
        "/rest/secure/angelbroking/order/v1/placeOrder",
        axum::routing::post(|| async { axum::response::Json(envelope(serde_json::json!({}))) }),
    );
    let base_url = serve(app).await;
    let err = executor(&base_url)
        .place_order(sample_order())
        .await
        .unwrap_err();
    assert!(matches!(err, BrokerError::InvalidRequest(_)), "got {err:?}");
}

#[tokio::test]
async fn cancel_order_success() {
    let app = Router::new().route(
        "/rest/secure/angelbroking/order/v1/cancelOrder",
        axum::routing::post(|| async { axum::response::Json(envelope(serde_json::json!({}))) }),
    );
    let base_url = serve(app).await;
    executor(&base_url)
        .cancel_order("260223110000001")
        .await
        .expect("cancel should succeed");
}

#[tokio::test]
async fn cancel_order_already_cancelled_is_idempotent() {
    let app = Router::new().route(
        "/rest/secure/angelbroking/order/v1/cancelOrder",
        axum::routing::post(|| async {
            axum::response::Json(rejected_envelope("Order already cancelled", "AB1004"))
        }),
    );
    let base_url = serve(app).await;
    executor(&base_url)
        .cancel_order("260223110000001")
        .await
        .expect("already-cancelled should be treated as success");
}

#[tokio::test]
async fn cancel_order_unknown_order_is_order_not_found() {
    let app = Router::new().route(
        "/rest/secure/angelbroking/order/v1/cancelOrder",
        axum::routing::post(|| async {
            axum::response::Json(rejected_envelope("Order not found", "AB1003"))
        }),
    );
    let base_url = serve(app).await;
    let err = executor(&base_url).cancel_order("nope").await.unwrap_err();
    assert!(matches!(err, BrokerError::OrderNotFound(_)), "got {err:?}");
}

#[tokio::test]
async fn modify_order_happy_path_reads_then_patches() {
    let app = read_server().route(
        "/rest/secure/angelbroking/order/v1/modifyOrder",
        axum::routing::post(|| async {
            axum::response::Json(envelope(serde_json::json!({
                "orderid": "260223110000002",
                "uniqueorderid": "u2"
            })))
        }),
    );
    let base_url = serve(app).await;
    let ack = executor(&base_url)
        .modify_order(
            "260223110000001",
            Some(Price::from_rupees("1460.00").unwrap()),
            None,
        )
        .await
        .unwrap();
    assert_eq!(ack.order_id, "260223110000002");
    assert_eq!(ack.status, OrderStatus::New);
}

#[tokio::test]
async fn modify_order_with_no_changes_is_invalid_request() {
    let app = read_server();
    let base_url = serve(app).await;
    let err = executor(&base_url)
        .modify_order("260223110000001", None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, BrokerError::InvalidRequest(_)), "got {err:?}");
}

#[tokio::test]
async fn order_book_parse() {
    let base_url = serve(read_server()).await;
    let book = executor(&base_url).fetch_order_book().await.unwrap();
    assert_eq!(book.len(), 1);
    let snapshot = &book[0];
    assert_eq!(snapshot.order_id, "260223110000001");
    assert_eq!(snapshot.exchange_order_id.as_deref(), Some("110000001"));
    assert_eq!(snapshot.status, OrderStatus::Filled);
    assert_eq!(snapshot.side, OrderSide::Buy);
    assert_eq!(snapshot.instrument.symbol(), "RELIANCE");
    assert_eq!(
        snapshot.order_type,
        OrderType::Limit(Price::from_rupees("1450.50").unwrap())
    );
    assert_eq!(snapshot.quantity.value(), rust_decimal::Decimal::from(25));
}

#[tokio::test]
async fn positions_parse_long_and_short() {
    let base_url = serve(read_server()).await;
    let positions = executor(&base_url).fetch_positions().await.unwrap();
    assert_eq!(positions.len(), 2);
    assert_eq!(positions[0].side, PositionSide::Long);
    assert_eq!(
        positions[0].quantity.value(),
        rust_decimal::Decimal::from(50)
    );
    assert_eq!(positions[1].side, PositionSide::Short);
    assert_eq!(
        positions[1].quantity.value(),
        rust_decimal::Decimal::from(25)
    );
    assert_eq!(
        positions[0].average_price.value(),
        rust_decimal::Decimal::from_str_exact("1440.25").unwrap()
    );
}

#[tokio::test]
async fn margins_parse() {
    let base_url = serve(read_server()).await;
    let margins = executor(&base_url).fetch_margins().await.unwrap();
    assert_eq!(
        margins.available_cash.amount(),
        rust_decimal::Decimal::from_str_exact("250000.00").unwrap()
    );
    assert_eq!(
        margins.used_margin.amount(),
        rust_decimal::Decimal::from_str_exact("75000.00").unwrap()
    );
}

#[tokio::test]
async fn health_check_reports_session() {
    let base_url = serve(read_server()).await;
    let health = executor(&base_url).health_check().await.unwrap();
    assert!(health.session_valid);
    assert_eq!(health.authenticated_user.as_deref(), Some("client123"));
    assert_eq!(health.broker, "angelone");
}

#[tokio::test]
async fn get_order_status_finds_order_and_reports_missing() {
    let base_url = serve(read_server()).await;
    let executor = executor(&base_url);
    let status = executor.get_order_status("260223110000001").await.unwrap();
    assert_eq!(status, OrderStatus::Filled);
    let err = executor.get_order_status("missing").await.unwrap_err();
    assert!(matches!(err, BrokerError::OrderNotFound(_)), "got {err:?}");
}

#[test]
fn default_base_url_is_documented_gateway() {
    assert_eq!(DEFAULT_BASE_URL, "https://apiconnect.angelone.in");
}
