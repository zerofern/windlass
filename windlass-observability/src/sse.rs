//! SSE message envelope and supporting types.
//!
//! The `/observability` SSE handler (lands in §37h) wraps every event
//! body in a [`SseMessage`] before forwarding to the client.  The
//! seven variants — `Hello`, `Step`, `HttpExchange`, `Log`,
//! `CoreStatus`, `Evicted`, `Loss` — are the §37pre B9 lock.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use windlass_machine::{CoreId, CoreStatus};

use crate::stored::{StoredHttpExchange, StoredStepRecord};

/// SSE wire envelope.  See `docs/observability-37pre-checklist.md` §B9.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SseMessage {
    Hello(HelloSnapshot),
    Step(StoredStepRecord),
    HttpExchange(StoredHttpExchange),
    Log(StoredLogLine),
    CoreStatus { core: CoreId, status: CoreStatus },
    Evicted(EvictedIds),
    Loss(LossCounters),
}

/// Sent once on connect, before any incremental messages.  Boots the
/// frontend's local store from a single payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloSnapshot {
    pub protocol_version: u32,
    pub cores: Vec<(CoreId, CoreStatus)>,
    pub steps: Vec<StoredStepRecord>,
    pub http: Vec<StoredHttpExchange>,
    pub logs: Vec<StoredLogLine>,
    pub loss: LossCounters,
    pub active_breakpoints: Vec<Breakpoint>,
}

/// IDs that have just left a ring (or had their reveal slot expire).
/// Emitted by the controller on every eviction so the frontend can
/// drop dangling references / revealed-secret state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvictedIds {
    pub step_ids: Vec<Uuid>,
    pub action_ids: Vec<Uuid>,
    pub publish_ids: Vec<Uuid>,
    pub reveal_ids: Vec<Uuid>,
}

impl EvictedIds {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.step_ids.is_empty()
            && self.action_ids.is_empty()
            && self.publish_ids.is_empty()
            && self.reveal_ids.is_empty()
    }
}

/// Per-core loss counters (dropped step records, truncated bodies).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoreCounters {
    pub dropped_steps: u64,
    pub truncated_bodies: u64,
    pub reservation_failures: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HttpCounters {
    pub dropped_exchanges: u64,
    pub truncated_request_bodies: u64,
    pub truncated_response_bodies: u64,
}

/// Cross-cutting loss counters, surfaced in the SSE `Loss` event.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LossCounters {
    pub per_core: HashMap<CoreId, CoreCounters>,
    pub http: HttpCounters,
}

impl LossCounters {
    /// Per-core counter helper.  Returns a mutable handle so callers
    /// can `loss.core_mut(CoreId::Mam).dropped_steps += 1`.
    pub fn core_mut(&mut self, core: CoreId) -> &mut CoreCounters {
        self.per_core.entry(core).or_default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.per_core
            .values()
            .all(|c| c == &CoreCounters::default())
            && self.http == HttpCounters::default()
    }
}

/// A single log line captured by the tracing layer.  §37j replaces the
/// legacy `windlass_debug::LogEntry` with this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredLogLine {
    pub at: DateTime<Utc>,
    pub level: String,
    pub target: String,
    pub message: String,
}

/// An active variant- or URL-pattern breakpoint.  §37g introduces the
/// registry; §37f ships only the type so the SSE `Hello` payload has a
/// stable shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Breakpoint {
    EventVariant { variant: String },
    ActionVariant { variant: String },
    PublishVariant { variant: String },
    HttpUrlPattern { pattern: String },
}
