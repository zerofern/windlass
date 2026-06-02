//! Owned, wire-shaped records that live in the rings and on the SSE
//! stream.  These are the §37pre-locked types described in
//! `docs/observability-redesign.md` under "Borrowed views vs owned
//! stored records".

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use windlass_machine::{CoreId, StepKind};

// ── Causal chain (wire-side mirror of `windlass_machine::EventCause`) ─────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum StoredExternalCause {
    Timer { name: String },
    FileWatcher { path: String },
    DockerEvent { kind: String },
    ManualCommand,
    Init,
    Unknown,
}

/// Wire/storage counterpart of [`windlass_machine::EventCause`].  Uses
/// `String` instead of `&'static str` / `PathBuf` so the frontend sees
/// owned data and the `PathBuf` serialization choice doesn't leak.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredEventCause {
    Action { id: Uuid },
    Publish { id: Uuid },
    External(StoredExternalCause),
}

impl From<&windlass_machine::EventCause> for StoredEventCause {
    fn from(cause: &windlass_machine::EventCause) -> Self {
        match cause {
            windlass_machine::EventCause::Action(id) => Self::Action { id: *id },
            windlass_machine::EventCause::Publish(id) => Self::Publish { id: *id },
            windlass_machine::EventCause::External(ext) => Self::External(ext.into()),
        }
    }
}

impl From<&windlass_machine::ExternalCause> for StoredExternalCause {
    fn from(cause: &windlass_machine::ExternalCause) -> Self {
        match cause {
            windlass_machine::ExternalCause::Timer { name } => Self::Timer {
                name: (*name).to_owned(),
            },
            windlass_machine::ExternalCause::FileWatcher { path } => Self::FileWatcher {
                path: path.to_string_lossy().into_owned(),
            },
            windlass_machine::ExternalCause::DockerEvent { kind } => Self::DockerEvent {
                kind: (*kind).to_owned(),
            },
            windlass_machine::ExternalCause::ManualCommand => Self::ManualCommand,
            windlass_machine::ExternalCause::Init => Self::Init,
            windlass_machine::ExternalCause::Unknown => Self::Unknown,
        }
    }
}

// ── Body capture ──────────────────────────────────────────────────────────────

/// What kind of body the capture is for.  Set from `Content-Type`
/// (request) or the response content type when populating.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BodyKind {
    Json,
    Text,
    Form,
    /// Binary bodies record only their length.
    Binary,
}

/// Captured HTTP body, with byte-budget enforcement at capture time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BodyCapture {
    Inline(serde_json::Value),
    Text(String),
    /// Binary body — only the length is captured.
    Bytes(usize),
    /// Oversized body — truncated to `captured`, original size preserved.
    Truncated {
        body_kind: BodyKind,
        captured: serde_json::Value,
        original_len: usize,
    },
    None,
}

impl BodyCapture {
    /// Capture a text body, enforcing `max_bytes`.  Bodies over the cap
    /// become [`BodyCapture::Truncated`] with `original_len` preserved.
    /// Returns `(capture, truncated)` so the caller can advance the
    /// truncation counter.
    #[must_use]
    pub fn from_text(body: &str, max_bytes: usize) -> (Self, bool) {
        let original_len = body.len();
        if original_len <= max_bytes {
            return (Self::Text(body.to_owned()), false);
        }
        let captured: String = body.chars().take(max_bytes).collect();
        (
            Self::Truncated {
                body_kind: BodyKind::Text,
                captured: serde_json::Value::String(captured),
                original_len,
            },
            true,
        )
    }
}

// ── Step record ───────────────────────────────────────────────────────────────

/// Owned, ring-storable step record — one event + its actions +
/// publishes + state-after + cause + timing.  See
/// `docs/observability-redesign.md` "Stored records".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredStepRecord {
    pub step_id: Uuid,
    pub core: CoreId,
    pub recorded_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub kind: StepKind,
    pub event_variant: String,
    pub event: serde_json::Value,
    pub event_cause: StoredEventCause,
    pub state_after: serde_json::Value,
    pub actions: Vec<StoredAction>,
    pub publishes: Vec<StoredPublish>,
}

impl StoredStepRecord {
    /// Approximate byte size for ring-budget accounting.  JSON
    /// representation is the proxy.
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        // Avoid allocating the full JSON for byte accounting; sum the
        // sizes of the variable parts.
        let actions_bytes: usize = self.actions.iter().map(StoredAction::estimated_bytes).sum();
        let publishes_bytes: usize = self
            .publishes
            .iter()
            .map(StoredPublish::estimated_bytes)
            .sum();
        128 + self.event_variant.len()
            + json_size(&self.event)
            + json_size(&self.state_after)
            + actions_bytes
            + publishes_bytes
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredAction {
    pub action_id: Uuid,
    pub variant: String,
    pub payload: serde_json::Value,
}

impl StoredAction {
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        64 + self.variant.len() + json_size(&self.payload)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredPublish {
    pub publish_id: Uuid,
    pub topic: String,
    pub variant: String,
    pub payload: serde_json::Value,
}

impl StoredPublish {
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        64 + self.topic.len() + self.variant.len() + json_size(&self.payload)
    }
}

// ── HTTP exchange ─────────────────────────────────────────────────────────────

/// Owned, ring-storable HTTP exchange.  Joins back to its originating
/// step via `action_id` (looked up in the controller's index).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredHttpExchange {
    pub exchange_id: Uuid,
    pub action_id: Option<Uuid>,
    pub core: CoreId,
    pub at: DateTime<Utc>,
    pub method: String,
    pub url: String,
    pub request_body: BodyCapture,
    pub response_status: u16,
    pub response_body: BodyCapture,
    pub duration_ms: u64,
}

impl StoredHttpExchange {
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        128 + self.method.len()
            + self.url.len()
            + body_capture_size(&self.request_body)
            + body_capture_size(&self.response_body)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

#[allow(clippy::cast_possible_truncation)] // sizes always fit in usize on our targets
fn json_size(v: &serde_json::Value) -> usize {
    serde_json::to_string(v)
        .map(|s| s.len())
        .unwrap_or_default()
}

fn body_capture_size(b: &BodyCapture) -> usize {
    match b {
        BodyCapture::Inline(v) | BodyCapture::Truncated { captured: v, .. } => json_size(v),
        BodyCapture::Text(s) => s.len(),
        BodyCapture::Bytes(_) => 16,
        BodyCapture::None => 0,
    }
}

// ── Duration conversion ──────────────────────────────────────────────────────

impl StoredStepRecord {
    /// Convert from a borrowed `StepRecordView` to an owned
    /// [`StoredStepRecord`].  The runtime-side view holds borrows; the
    /// controller calls `from_view` to copy into the ring.
    #[must_use]
    pub fn duration_ms_from(duration: Duration) -> u64 {
        // saturating cast — durations under u64::MAX millis (≈ 584M
        // years) round-trip exactly.
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
    }
}
