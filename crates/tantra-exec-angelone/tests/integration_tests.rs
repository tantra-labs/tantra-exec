//! Smoke tests: executor construction from session tokens, rate-limiter
//! wrapping, the broker-name contract, and trait default behaviour.

use async_trait::async_trait;
use chrono::Utc;

use tantra_core::{OrderAck, OrderRequest, OrderStatus};
use tantra_exec_angelone::AngelOneExecutor;
use tantra_exec_auth::tokens::SessionTokens;
use tantra_exec_core::{BrokerError, BrokerExecutor};

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

/// Minimal in-memory executor used to prove trait-default behaviour.
struct MockBrokerExecutor;

#[async_trait]
impl BrokerExecutor for MockBrokerExecutor {
    fn name(&self) -> &str {
        "mock"
    }

    async fn place_order(&self, req: OrderRequest) -> Result<OrderAck, BrokerError> {
        Ok(OrderAck::new(
            "mock-order".to_string(),
            req.client_order_id,
            OrderStatus::New,
            Utc::now(),
        ))
    }

    async fn cancel_order(&self, _order_id: &str) -> Result<(), BrokerError> {
        Ok(())
    }

    async fn get_order_status(&self, _order_id: &str) -> Result<OrderStatus, BrokerError> {
        Ok(OrderStatus::PendingNew)
    }
}

#[tokio::test]
async fn executor_constructs_with_session_tokens() {
    let executor = AngelOneExecutor::new("client123", "api-key-1", &session_tokens());
    assert_eq!(executor.name(), "angelone");
    assert_eq!(executor.feed_token(), Some("feed.token"));
    assert!(!executor.is_session_expired(Utc::now()));
}

#[tokio::test]
async fn rate_limited_wraps_the_executor() {
    let executor = AngelOneExecutor::new("client123", "api-key-1", &session_tokens());
    let limited = executor
        .rate_limited()
        .expect("angelone rate preset exists");
    assert_eq!(limited.name(), "angelone");
}

#[tokio::test]
async fn default_trait_methods_return_invalid_request() {
    let err = MockBrokerExecutor
        .health_check()
        .await
        .expect_err("default health_check must be unimplemented");
    assert!(matches!(err, BrokerError::InvalidRequest(_)));
}
