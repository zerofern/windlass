//! Runtime-side observability hook trait and supporting types.
//!
//! Lives in `windlass-machine` (rather than `windlass-observability`) so
//! [`crate::runtime::ServiceRuntime`] can call into it without the
//! observability crate having to depend on the runtime. The live impl
//! ([`windlass_observability::ObservabilityController`]) is in the
//! observability crate.
//!
//! See `docs/observability-redesign.md` "Architecture / `RuntimeTap`"
//! for the contract; in particular EC-1 (`observed_*` must not block)
//! and EC-8 (`reserve_step_ids` runs before `apply`).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::machine::EventCause;

// ── CoreId ────────────────────────────────────────────────────────────────────

/// Identifies a per-system service runtime. Closed enum, lowercased on
/// the SSE wire so the frontend sees stable identifiers.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoreId {
    Vpn,
    Qbit,
    Mam,
    Db,
    Disk,
    Docker,
    Domain,
}

impl CoreId {
    /// All seven cores in cores-rail display order.
    #[must_use]
    pub const fn all() -> [Self; 7] {
        [
            Self::Vpn,
            Self::Qbit,
            Self::Mam,
            Self::Db,
            Self::Disk,
            Self::Docker,
            Self::Domain,
        ]
    }
}

impl std::fmt::Display for CoreId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Vpn => "vpn",
            Self::Qbit => "qbit",
            Self::Mam => "mam",
            Self::Db => "db",
            Self::Disk => "disk",
            Self::Docker => "docker",
            Self::Domain => "domain",
        };
        f.write_str(name)
    }
}

// ── StepKind ──────────────────────────────────────────────────────────────────

/// Distinguishes an event-driven step from a command-driven step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepKind {
    Event,
    Command { response: CommandResponseStatus },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandResponseStatus {
    /// `oneshot::send` succeeded.
    Sent,
    /// Caller dropped the receiver before we replied.
    ReceiverDropped,
}

// ── CoreStatus ────────────────────────────────────────────────────────────────

/// Per-core lifecycle state, broadcast over SSE so the cores rail
/// can render the right Pause/Step affordance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CoreStatus {
    Running,
    PauseRequested,
    ParkedAtEvent {
        variant: String,
        since: DateTime<Utc>,
    },
    ParkedAtOutcome {
        source_variant: String,
        since: DateTime<Utc>,
    },
    ParkedAtHttp {
        method: String,
        url: String,
        since: DateTime<Utc>,
    },
    Stepping,
}

// ── Borrowed gate views ───────────────────────────────────────────────────────

pub struct EventGateView<'a> {
    pub variant: &'a str,
    pub cause: &'a EventCause,
    pub event: &'a serde_json::Value,
}

pub struct OutcomeGateView<'a> {
    pub source_event_variant: &'a str,
    pub action_variants: &'a [&'a str],
    pub action_ids: &'a [Uuid],
    pub publish_variants: &'a [&'a str],
    pub publish_ids: &'a [Uuid],
}

pub struct StepRecordView<'a> {
    pub step_id: Uuid,
    pub core: CoreId,
    pub recorded_at: DateTime<Utc>,
    pub duration: Duration,
    pub kind: StepKind,
    pub event_variant: &'a str,
    pub event: &'a serde_json::Value,
    pub event_cause: &'a EventCause,
    pub state_after: &'a serde_json::Value,
    pub action_ids: &'a [Uuid],
    pub action_variants: &'a [&'a str],
    pub publish_ids: &'a [Uuid],
    pub publish_variants: &'a [&'a str],
}

// ── RuntimeTap ────────────────────────────────────────────────────────────────

#[async_trait]
pub trait RuntimeTap: Send + Sync {
    /// Park until released. Returns immediately when this core's pause
    /// flag is not set and no matching event-variant breakpoint is
    /// active. EC-2: gates are the only place a tap may park.
    async fn gate_event(&self, core: CoreId, view: &EventGateView<'_>);

    /// Park between `handle` and `apply`, with the outcome visible.
    async fn gate_outcome(&self, core: CoreId, view: &OutcomeGateView<'_>);

    /// Register the action/publish IDs against `step_id` BEFORE
    /// dispatch (EC-8). Must not block / panic / fail.
    fn reserve_step_ids(
        &self,
        core: CoreId,
        step_id: Uuid,
        action_ids: &[Uuid],
        publish_ids: &[Uuid],
    );

    /// Fire-and-forget post-dispatch record. EC-1: must not block,
    /// must drop on overflow.
    fn observed_step(&self, core: CoreId, view: &StepRecordView<'_>);
}

// ── NullRuntimeTap ────────────────────────────────────────────────────────────

/// No-op `RuntimeTap` used when observability is not attached.
/// Every method returns immediately; the runtime pays a single
/// indirection per call.
pub struct NullRuntimeTap;

#[async_trait]
impl RuntimeTap for NullRuntimeTap {
    async fn gate_event(&self, _core: CoreId, _view: &EventGateView<'_>) {}
    async fn gate_outcome(&self, _core: CoreId, _view: &OutcomeGateView<'_>) {}
    fn reserve_step_ids(
        &self,
        _core: CoreId,
        _step_id: Uuid,
        _action_ids: &[Uuid],
        _publish_ids: &[Uuid],
    ) {
    }
    fn observed_step(&self, _core: CoreId, _view: &StepRecordView<'_>) {}
}

impl NullRuntimeTap {
    /// Convenience: `Arc<dyn RuntimeTap>` slot for [`crate::runtime::spawn`].
    #[must_use]
    pub fn arc() -> Arc<dyn RuntimeTap> {
        Arc::new(Self)
    }
}
