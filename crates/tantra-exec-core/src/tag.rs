//! Shared idempotent wire correlation tags (`client_order_id` + `algo_id`).
//!
//! Gap 0004: every outbound broker submission must carry an idempotency tag
//! derived from [`OrderRequest::client_order_id`] so an ambiguous timeout or
//! post-send connection drop can be reconciled against `fetch_order_book` before
//! any retry or failover.
//!
//! Wire constraints vary across Indian broker APIs:
//! - Upstox v2 accepts up to 40 characters (`wire_tag_full`: 26 uppercase
//!   Base32 chars encoding all 128 bits losslessly).
//! - Zerodha Kite v3 and Angel One SmartAPI cap `tag` / `ordertag` at 20
//!   alphanumeric characters (`wire_tag_20`: 20 uppercase Base32 chars encoding
//!   the leading 96 bits of `client_order_id`, paired with [`OrderTagRegistry`]
//!   for exact full-ID and `algo_id` resolution).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use tantra_core::OrderRequest;

/// Kite Connect v3 documented maximum `tag` length (alphanumeric).
pub const KITE_TAG_MAX_LEN: usize = 20;

/// Upstox v2 documented maximum `tag` length.
pub const UPSTOX_TAG_MAX_LEN: usize = 40;

/// Angel One SmartAPI maximum `tag` / `ordertag` length.
pub const ANGELONE_TAG_MAX_LEN: usize = 20;

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Paired client correlation ID and SEBI algorithm ID for a broker order.
///
/// The wire `tag` carries the compact Base32 correlation token derived from
/// [`Self::client_order_id`], while [`Self::algo_id`] is retained for exchange
/// algo fields and local correlation lookups via [`OrderTagRegistry`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrderTag {
    /// Client-assigned correlation UUID.
    pub client_order_id: Uuid,
    /// SEBI-registered algorithm identifier.
    pub algo_id: String,
}

impl OrderTag {
    /// Construct an [`OrderTag`] from explicit IDs.
    pub fn new(client_order_id: Uuid, algo_id: impl Into<String>) -> Self {
        Self {
            client_order_id,
            algo_id: algo_id.into(),
        }
    }

    /// Construct an [`OrderTag`] from a canonical [`OrderRequest`].
    pub fn from_request(req: &OrderRequest) -> Self {
        Self {
            client_order_id: req.client_order_id,
            algo_id: req.algo_id.clone(),
        }
    }

    /// 26-character uppercase Base32 encoding of all 128 bits of
    /// [`Self::client_order_id`]. Fits Upstox (max 40 chars).
    pub fn wire_tag_full(&self) -> String {
        Self::encode_uuid_full(self.client_order_id)
    }

    /// 20-character uppercase Base32 encoding of the leading 96 bits (12 bytes)
    /// of [`Self::client_order_id`]. Fits Kite Connect v3 and Angel One
    /// SmartAPI (max 20 alphanumeric chars).
    pub fn wire_tag_20(&self) -> String {
        Self::encode_uuid_compact_20(self.client_order_id)
    }

    /// Encode the canonical wire tag obeying the target broker's character cap.
    pub fn encode_for_broker(&self, broker: &str) -> String {
        match broker {
            "upstox" => self.wire_tag_full(),
            _ => self.wire_tag_20(),
        }
    }

    /// True if `raw` decodes to a tag matching this order's `client_order_id`.
    pub fn matches_wire_tag(&self, raw: &str) -> bool {
        DecodedOrderTag::decode(raw).is_some_and(|decoded| decoded.matches(self.client_order_id))
    }

    /// Encode all 16 UUID bytes into 26 canonical uppercase Base32 characters.
    pub fn encode_uuid_full(id: Uuid) -> String {
        encode_base32(id.as_bytes())
    }

    /// Decode a 26-character Base32 string back into an exact [`Uuid`].
    pub fn decode_uuid_full(raw: &str) -> Option<Uuid> {
        decode_base32::<16>(raw).map(Uuid::from_bytes)
    }

    /// Encode the leading 12 bytes (96 bits) of a UUID into 20 uppercase
    /// Base32 characters.
    pub fn encode_uuid_compact_20(id: Uuid) -> String {
        encode_base32(&id.as_bytes()[..12])
    }

    /// Decode a 20-character compact Base32 string back into its 12-byte
    /// (96-bit) prefix.
    pub fn decode_compact_20(raw: &str) -> Option<[u8; 12]> {
        decode_base32::<12>(raw)
    }
}

/// Decoded wire tag parsed from a broker order book entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecodedOrderTag {
    /// Lossless 128-bit client UUID (from a 26-char Base32 tag or UUID string).
    Exact(Uuid),
    /// 96-bit client UUID prefix (from a 20-char compact Base32 tag).
    CompactPrefix([u8; 12]),
}

impl DecodedOrderTag {
    /// Decode a raw broker tag string (full 26-char Base32, compact 20-char
    /// Base32, or standard UUID hex representation). Returns `None` for
    /// legacy non-correlation tags (e.g. raw algo IDs like `"algo-alpha"`).
    pub fn decode(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if let Some(uuid) = OrderTag::decode_uuid_full(trimmed) {
            return Some(Self::Exact(uuid));
        }
        if let Some(prefix) = OrderTag::decode_compact_20(trimmed) {
            return Some(Self::CompactPrefix(prefix));
        }
        Uuid::parse_str(trimmed).ok().map(Self::Exact)
    }

    /// Return the full 128-bit UUID when recovered losslessly.
    pub fn exact_uuid(&self) -> Option<Uuid> {
        match self {
            Self::Exact(uuid) => Some(*uuid),
            Self::CompactPrefix(_) => None,
        }
    }

    /// True when this decoded wire tag matches `client_order_id`.
    pub fn matches(&self, client_order_id: Uuid) -> bool {
        match self {
            Self::Exact(uuid) => *uuid == client_order_id,
            Self::CompactPrefix(prefix) => &client_order_id.as_bytes()[..12] == prefix,
        }
    }
}

/// Thread-safe in-memory lookup table mapping wire tags back to full
/// `(client_order_id, algo_id)` pairs.
#[derive(Debug, Clone, Default)]
pub struct OrderTagRegistry {
    by_wire: Arc<RwLock<HashMap<String, OrderTag>>>,
}

impl OrderTagRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an [`OrderTag`] under both its full (26-char) and compact
    /// (20-char) wire encodings (normalized to uppercase).
    pub fn record(&self, tag: &OrderTag) {
        let mut map = self.by_wire.write().unwrap();
        map.insert(tag.wire_tag_full(), tag.clone());
        map.insert(tag.wire_tag_20(), tag.clone());
    }

    /// Resolve a raw wire tag (case-insensitive) back to its full [`OrderTag`].
    pub fn resolve(&self, raw_wire_tag: &str) -> Option<OrderTag> {
        let key = raw_wire_tag.trim().to_ascii_uppercase();
        self.by_wire.read().unwrap().get(&key).cloned()
    }
}

fn encode_base32(bytes: &[u8]) -> String {
    let out_len = (bytes.len() * 8).div_ceil(5);
    let mut out = String::with_capacity(out_len);
    let mut bit_idx = 0usize;
    while bit_idx < bytes.len() * 8 {
        let mut chunk = 0u8;
        for offset in 0..5 {
            let pos = bit_idx + offset;
            let bit = if pos < bytes.len() * 8 {
                let byte = bytes[pos / 8];
                (byte >> (7 - (pos % 8))) & 1
            } else {
                0
            };
            chunk = (chunk << 1) | bit;
        }
        out.push(ALPHABET[chunk as usize] as char);
        bit_idx += 5;
    }
    out
}

fn decode_base32<const N: usize>(raw: &str) -> Option<[u8; N]> {
    let expected_chars = (N * 8).div_ceil(5);
    if raw.len() != expected_chars {
        return None;
    }
    let mut out = [0u8; N];
    let mut total_bits = 0usize;
    for b in raw.bytes() {
        let upper = b.to_ascii_uppercase();
        let val = match upper {
            b'A'..=b'Z' => upper - b'A',
            b'2'..=b'7' => 26 + (upper - b'2'),
            _ => return None,
        };
        for offset in (0..5).rev() {
            let bit = (val >> offset) & 1;
            if total_bits < N * 8 {
                if bit != 0 {
                    out[total_bits / 8] |= 1 << (7 - (total_bits % 8));
                }
            } else if bit != 0 {
                return None;
            }
            total_bits += 1;
        }
    }
    Some(out)
}
