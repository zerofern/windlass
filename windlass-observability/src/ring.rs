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
                request_body: crate::stored::BodyCapture::None,
                response_status: 200,
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
            request_body: crate::stored::BodyCapture::None,
            response_status: 200,
            response_body: crate::stored::BodyCapture::None,
            duration_ms: 0,
        });
        assert_eq!(evicted.len(), 1);
    }
}
