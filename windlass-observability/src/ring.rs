//! Bounded rings for stored step records and HTTP exchanges.
//!
//! Both rings enforce a count budget *and* a byte budget — whichever
//! fills first triggers eviction.  Eviction always drops the oldest
//! entry.  EC-3 (ring eviction cleans indices) is honored by returning
//! the evicted record so the controller can drop its `action_id`s and
//! `publish_id`s from the lookup index.

use std::collections::VecDeque;

use crate::stored::{StoredHttpExchange, StoredStepRecord};

// ── Budgets (compile-time defaults, see §37pre B7) ────────────────────────────

pub const STEP_RECORDS_PER_CORE: usize = 500;
/// 4 MiB per per-core step ring.
pub const STEP_RECORD_BYTES_PER_CORE: usize = 4 * 1024 * 1024;

pub const HTTP_EXCHANGES_TOTAL: usize = 500;
/// 8 MiB cross-core HTTP exchange ring.
pub const HTTP_EXCHANGE_BYTES_TOTAL: usize = 8 * 1024 * 1024;

pub const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_BODY_BYTES: usize = 256 * 1024;

// ── Configurable budgets ──────────────────────────────────────────────────────

/// Runtime-configurable budgets for the observability rings and body
/// captures.  The defaults match the locked §37pre B7 constants above;
/// the operator overrides via environment variables in
/// `windlass::shell::config::Config` (see
/// `docs/observability-redesign.md` "Configuration").
///
/// Byte-budget keys honor IEC binary suffixes (`KiB` = 1024 B,
/// `MiB` = 1024 KiB) — see [`parse_byte_budget`].
#[derive(Debug, Clone, Copy)]
pub struct ObservabilityConfig {
    pub step_records_per_core: usize,
    pub step_record_bytes_per_core: usize,
    pub http_exchanges_total: usize,
    pub http_exchange_bytes_total: usize,
    pub max_request_body_bytes: usize,
    pub max_response_body_bytes: usize,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            step_records_per_core: STEP_RECORDS_PER_CORE,
            step_record_bytes_per_core: STEP_RECORD_BYTES_PER_CORE,
            http_exchanges_total: HTTP_EXCHANGES_TOTAL,
            http_exchange_bytes_total: HTTP_EXCHANGE_BYTES_TOTAL,
            max_request_body_bytes: MAX_REQUEST_BODY_BYTES,
            max_response_body_bytes: MAX_RESPONSE_BODY_BYTES,
        }
    }
}

/// Parse a byte-budget value with IEC binary suffix.  Accepted forms:
///
/// - `"123"` → 123 bytes
/// - `"64KiB"` → 65 536 bytes
/// - `"4MiB"` → 4 194 304 bytes
///
/// Case-insensitive on the suffix.  Whitespace around the number is
/// tolerated.  Returns `Err` on unknown suffix or non-numeric prefix —
/// callers map that to a config-load failure.
///
/// # Errors
/// Returns the offending input as the error so the operator can see
/// what failed.
pub fn parse_byte_budget(raw: &str) -> Result<usize, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(format!("empty byte budget: {raw:?}"));
    }
    let lower = s.to_ascii_lowercase();
    let (number, multiplier) = if let Some(prefix) = lower.strip_suffix("mib") {
        (prefix, 1024usize * 1024)
    } else if let Some(prefix) = lower.strip_suffix("kib") {
        (prefix, 1024usize)
    } else if let Some(prefix) = lower.strip_suffix('b') {
        (prefix, 1usize)
    } else {
        (lower.as_str(), 1usize)
    };
    let n: usize = number
        .trim()
        .parse()
        .map_err(|_| format!("byte budget not a number: {raw:?}"))?;
    n.checked_mul(multiplier)
        .ok_or_else(|| format!("byte budget overflows usize: {raw:?}"))
}

// ── StepRecordRing ────────────────────────────────────────────────────────────

/// Per-core bounded ring for [`StoredStepRecord`].  Drop-oldest on
/// count or byte overflow.
pub struct StepRecordRing {
    records: VecDeque<StoredStepRecord>,
    total_bytes: usize,
    max_records: usize,
    max_bytes: usize,
}

impl StepRecordRing {
    #[must_use]
    pub fn new(max_records: usize, max_bytes: usize) -> Self {
        Self {
            records: VecDeque::with_capacity(max_records),
            total_bytes: 0,
            max_records,
            max_bytes,
        }
    }

    /// Push a record, dropping oldest entries until both budgets are
    /// satisfied.  Returns the list of evicted records so the
    /// controller can clean its `action_id` / `publish_id` indices.
    pub fn push(&mut self, record: StoredStepRecord) -> Vec<StoredStepRecord> {
        let record_bytes = record.estimated_bytes();
        let mut evicted = Vec::new();

        // Evict until both count and byte budgets accommodate the new record.
        while (self.records.len() + 1 > self.max_records)
            || (self.total_bytes + record_bytes > self.max_bytes && !self.records.is_empty())
        {
            let Some(old) = self.records.pop_front() else {
                break;
            };
            self.total_bytes = self.total_bytes.saturating_sub(old.estimated_bytes());
            evicted.push(old);
        }

        self.total_bytes += record_bytes;
        self.records.push_back(record);
        evicted
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Iterate records oldest-first.
    pub fn iter(&self) -> impl Iterator<Item = &StoredStepRecord> {
        self.records.iter()
    }
}

// ── HttpExchangeRing ──────────────────────────────────────────────────────────

/// Cross-core bounded ring for [`StoredHttpExchange`].  Drop-oldest on
/// count or byte overflow.
pub struct HttpExchangeRing {
    exchanges: VecDeque<StoredHttpExchange>,
    total_bytes: usize,
    max_records: usize,
    max_bytes: usize,
}

impl HttpExchangeRing {
    #[must_use]
    pub fn new(max_records: usize, max_bytes: usize) -> Self {
        Self {
            exchanges: VecDeque::with_capacity(max_records),
            total_bytes: 0,
            max_records,
            max_bytes,
        }
    }

    pub fn push(&mut self, exchange: StoredHttpExchange) -> Vec<StoredHttpExchange> {
        let bytes = exchange.estimated_bytes();
        let mut evicted = Vec::new();

        while (self.exchanges.len() + 1 > self.max_records)
            || (self.total_bytes + bytes > self.max_bytes && !self.exchanges.is_empty())
        {
            let Some(old) = self.exchanges.pop_front() else {
                break;
            };
            self.total_bytes = self.total_bytes.saturating_sub(old.estimated_bytes());
            evicted.push(old);
        }

        self.total_bytes += bytes;
        self.exchanges.push_back(exchange);
        evicted
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.exchanges.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.exchanges.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &StoredHttpExchange> {
        self.exchanges.iter()
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;
    use windlass_machine::{CoreId, StepKind};

    use crate::stored::StoredEventCause;

    fn small_record(byte_padding: usize) -> StoredStepRecord {
        // Pad the event_variant string to drive the byte budget.
        let pad = "x".repeat(byte_padding);
        StoredStepRecord {
            step_id: Uuid::new_v4(),
            core: CoreId::Vpn,
            recorded_at: Utc::now(),
            duration_ms: 1,
            kind: StepKind::Event,
            event_variant: pad,
            event: serde_json::Value::Null,
            event_cause: StoredEventCause::External(crate::stored::StoredExternalCause::Init),
            state_after: serde_json::Value::Null,
            actions: Vec::new(),
            publishes: Vec::new(),
        }
    }

    #[test]
    fn step_ring_evicts_oldest_when_count_budget_full() {
        let mut ring = StepRecordRing::new(3, 1024 * 1024);
        let evicted = ring.push(small_record(0));
        assert!(evicted.is_empty());
        ring.push(small_record(0));
        ring.push(small_record(0));
        assert_eq!(ring.len(), 3);
        let evicted = ring.push(small_record(0));
        assert_eq!(evicted.len(), 1, "fourth push should evict the oldest");
        assert_eq!(ring.len(), 3);
    }

    #[test]
    fn step_ring_evicts_when_byte_budget_full() {
        // 1024-byte limit, ~150-byte records — fits ~6.
        let mut ring = StepRecordRing::new(1000, 1024);
        for _ in 0..6 {
            ring.push(small_record(0));
        }
        let before = ring.len();
        let evicted = ring.push(small_record(2000));
        assert!(!evicted.is_empty(), "huge push should evict to fit");
        assert!(ring.len() <= before);
    }

    #[test]
    fn parse_byte_budget_accepts_iec_suffixes() {
        use super::parse_byte_budget;
        assert_eq!(parse_byte_budget("123").unwrap(), 123);
        assert_eq!(parse_byte_budget("64KiB").unwrap(), 64 * 1024);
        assert_eq!(parse_byte_budget("4MiB").unwrap(), 4 * 1024 * 1024);
        assert_eq!(parse_byte_budget("256kib").unwrap(), 256 * 1024);
        assert_eq!(parse_byte_budget(" 8MiB ").unwrap(), 8 * 1024 * 1024);
        assert_eq!(parse_byte_budget("1024B").unwrap(), 1024);
    }

    #[test]
    fn parse_byte_budget_rejects_invalid() {
        use super::parse_byte_budget;
        assert!(parse_byte_budget("").is_err());
        assert!(parse_byte_budget("MiB").is_err());
        assert!(parse_byte_budget("abc").is_err());
    }

    #[test]
    fn http_ring_evicts_oldest_when_count_budget_full() {
        let mut ring = HttpExchangeRing::new(2, 1024 * 1024);
        for _ in 0..2 {
            ring.push(StoredHttpExchange {
                exchange_id: Uuid::new_v4(),
                action_id: None,
                core: CoreId::Mam,
                at: Utc::now(),
                method: "GET".into(),
                url: "https://example/".into(),
                request_headers: Vec::new(),
                request_body: crate::stored::BodyCapture::None,
                response_status: 200,
                response_headers: Vec::new(),
                response_body: crate::stored::BodyCapture::None,
                duration_ms: 0,
            });
        }
        let evicted = ring.push(StoredHttpExchange {
            exchange_id: Uuid::new_v4(),
            action_id: None,
            core: CoreId::Mam,
            at: Utc::now(),
            method: "GET".into(),
            url: "https://example/".into(),
            request_headers: Vec::new(),
            request_body: crate::stored::BodyCapture::None,
            response_status: 200,
            response_headers: Vec::new(),
            response_body: crate::stored::BodyCapture::None,
            duration_ms: 0,
        });
        assert_eq!(evicted.len(), 1);
    }
}
