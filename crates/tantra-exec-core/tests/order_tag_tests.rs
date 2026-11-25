//! TDD tests for Gap 0004: shared `OrderTag` codec, `OrderTagRegistry`,
//! `BrokerError::OutcomeUnknown`, and `BrokerExecutor::find_by_client_order_id`.

use std::sync::Arc;

use chrono::Utc;
use proptest::prelude::*;
use uuid::Uuid;

use tantra_core::{
    Exchange, InstrumentId, OrderRequest, OrderSide, OrderStatus, OrderType, Price, Quantity,
};
use tantra_exec_core::tag::{
    ANGELONE_TAG_MAX_LEN, DecodedOrderTag, KITE_TAG_MAX_LEN, OrderTag, OrderTagRegistry,
    UPSTOX_TAG_MAX_LEN,
};
use tantra_exec_core::{BrokerError, BrokerExecutor, MockBrokerExecutor, OrderSnapshot};

#[test]
fn order_tag_wire_lengths_and_character_sets_respect_broker_limits() {
    let cid = Uuid::new_v4();
    let tag = OrderTag::new(cid, "algo-sebi-01");

    let kite = tag.encode_for_broker("zerodha");
    assert!(kite.len() <= KITE_TAG_MAX_LEN, "kite={kite}");
    assert!(kite.chars().all(|c| c.is_ascii_alphanumeric()));

    let angel = tag.encode_for_broker("angelone");
    assert!(angel.len() <= ANGELONE_TAG_MAX_LEN, "angel={angel}");
    assert!(angel.chars().all(|c| c.is_ascii_alphanumeric()));

    let upstox = tag.encode_for_broker("upstox");
    assert!(upstox.len() <= UPSTOX_TAG_MAX_LEN, "upstox={upstox}");
    assert!(upstox.chars().all(|c| c.is_ascii_alphanumeric()));
    assert_eq!(
        DecodedOrderTag::decode(&upstox),
        Some(DecodedOrderTag::Exact(cid))
    );
}

#[test]
fn order_tag_registry_recovers_client_id_and_algo_id_from_compact_tag() {
    let cid = Uuid::new_v4();
    let tag = OrderTag::new(cid, "SEBI-ALGO-42");
    let registry = OrderTagRegistry::new();
    registry.record(&tag);

    let wire = tag.wire_tag_20();
    let resolved = registry.resolve(&wire).expect("registered compact tag");
    assert_eq!(resolved.client_order_id, cid);
    assert_eq!(resolved.algo_id, "SEBI-ALGO-42");
}

#[test]
fn outcome_unknown_variant_is_non_failover_and_preserves_client_order_id() {
    let cid = Uuid::new_v4();
    let err = BrokerError::OutcomeUnknown {
        client_order_id: cid,
        message: "connection reset after POST body".into(),
    };
    assert!(err.is_outcome_unknown());
    assert!(err.to_string().contains(&cid.to_string()));
}

#[tokio::test]
async fn find_by_client_order_id_matches_exact_and_compact_wire_tags() {
    let mock = Arc::new(MockBrokerExecutor::accept_all());
    let req = OrderRequest::new(
        InstrumentId::try_new(Exchange::Nse, "INFY").unwrap(),
        OrderSide::Buy,
        OrderType::Limit(Price::from_rupees("1500.00").unwrap()),
        Quantity::from_u64(5),
        "algo-recon",
    );
    let cid = req.client_order_id;
    mock.place_order(req).await.unwrap();

    let found = mock
        .find_by_client_order_id(cid)
        .await
        .unwrap()
        .expect("order should be reconciled by client_order_id");
    assert_eq!(found.client_order_id, Some(cid));

    let missing = mock.find_by_client_order_id(Uuid::new_v4()).await.unwrap();
    assert!(missing.is_none());

    // Simulate a broker book entry carrying only a 20-char Kite compact tag.
    let compact_cid = Uuid::new_v4();
    let compact_tag = OrderTag::new(compact_cid, "algo-kite").wire_tag_20();
    let snapshot = OrderSnapshot {
        order_id: "KITE-999".into(),
        client_order_id: None,
        exchange_order_id: None,
        tag: Some(compact_tag),
        instrument: InstrumentId::try_new(Exchange::Nse, "SBIN").unwrap(),
        side: OrderSide::Buy,
        order_type: OrderType::Market,
        quantity: Quantity::from_u64(1),
        filled_quantity: Quantity::ZERO,
        price: None,
        trigger_price: None,
        status: OrderStatus::New,
        product: None,
        timestamp: Utc::now(),
    };
    assert!(snapshot.matches_client_order_id(compact_cid));
    assert!(!snapshot.matches_client_order_id(Uuid::new_v4()));
}

proptest! {
    #[test]
    fn proptest_order_tag_full_and_compact_round_trip(bytes in any::<[u8; 16]>(), algo in "[A-Za-z0-9_-]{1,24}") {
        let cid = Uuid::from_bytes(bytes);
        let tag = OrderTag::new(cid, algo.clone());

        // Full 26-char Base32 encoding round-trips all 128 bits losslessly.
        let full = tag.wire_tag_full();
        prop_assert_eq!(full.len(), 26);
        prop_assert!(full.len() <= UPSTOX_TAG_MAX_LEN);
        prop_assert!(full.chars().all(|c| c.is_ascii_alphanumeric()));
        prop_assert_eq!(OrderTag::decode_uuid_full(&full), Some(cid));
        prop_assert_eq!(DecodedOrderTag::decode(&full), Some(DecodedOrderTag::Exact(cid)));
        prop_assert!(tag.matches_wire_tag(&full));

        // Compact 20-char Base32 encoding stays <= 20 alphanumeric chars and
        // round-trips its 96-bit prefix losslessly.
        let compact = tag.wire_tag_20();
        prop_assert!(compact.len() <= KITE_TAG_MAX_LEN);
        prop_assert!(compact.len() <= ANGELONE_TAG_MAX_LEN);
        prop_assert_eq!(compact.len(), 20);
        prop_assert!(compact.chars().all(|c| c.is_ascii_alphanumeric()));

        let decoded_prefix = OrderTag::decode_compact_20(&compact);
        let expected_prefix: [u8; 12] = bytes[..12].try_into().unwrap();
        prop_assert_eq!(decoded_prefix, Some(expected_prefix));
        prop_assert_eq!(DecodedOrderTag::decode(&compact), Some(DecodedOrderTag::CompactPrefix(expected_prefix)));
        prop_assert!(tag.matches_wire_tag(&compact));

        // Corrupting the string never accidentally matches the wrong exact prefix.
        let different = Uuid::from_bytes({
            let mut other = bytes;
            other[0] ^= 0xFF;
            other
        });
        prop_assert!(!DecodedOrderTag::decode(&full).unwrap().matches(different));
        prop_assert!(!DecodedOrderTag::decode(&compact).unwrap().matches(different));
    }
}
