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

// ── Secret slots ──────────────────────────────────────────────────────────────

/// Server-side holder for one cleartext secret captured into a stored
/// record.  Lives only inside the ring; the wire serializer (below)
/// flips it to [`WireRedacted`] so cleartext never leaves the process
/// over SSE.  The matching cleartext is exposed only through the
/// `POST /api/v1/observability/reveal/{reveal_id}` endpoint, which
/// scans the rings for `reveal_id` and returns `410 Gone` once the
/// parent record is evicted.
///
/// See `docs/observability-redesign.md` "Secrets (Decision 14 detail)".
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ServerSecretSlot {
    /// The cleartext value.  Never serialized — the hand-rolled
    /// `Serialize` impl below emits [`WireRedacted`] instead.
    pub cleartext: String,
    /// Unguessable single-field handle the UI passes back to the
    /// reveal endpoint.
    pub reveal_id: Uuid,
}

impl ServerSecretSlot {
    /// Wrap `cleartext` in a new slot with a fresh `reveal_id`.
    #[must_use]
    pub fn new(cleartext: impl Into<String>) -> Self {
        Self {
            cleartext: cleartext.into(),
            reveal_id: Uuid::new_v4(),
        }
    }
}

/// Hand-rolled serializer: `ServerSecretSlot` always serializes as
/// `WireRedacted { redacted: true, reveal_id }`.  Cleartext is
/// dropped on the floor — there is no opt-in to leak it.  Reveal is
/// the only path to cleartext on the wire.
impl Serialize for ServerSecretSlot {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut sm = s.serialize_struct("WireRedacted", 2)?;
        sm.serialize_field("redacted", &true)?;
        sm.serialize_field("reveal_id", &self.reveal_id)?;
        sm.end()
    }
}

/// The wire form `ServerSecretSlot` serializes to.  Defined as a
/// separate type so deserialize paths (tests, transport round-trips)
/// can target it explicitly; the server never *constructs* one
/// directly — the `Serialize` impl on [`ServerSecretSlot`] is the
/// only producer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireRedacted {
    pub redacted: bool,
    pub reveal_id: Uuid,
}

/// A header value (or other captured field) that is either plaintext
/// passed through unchanged, or a secret slot that serializes to
/// `WireRedacted`.  Stored alongside the rest of the record; the
/// serializer chooses the right wire form automatically.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum MaybeSecret {
    Plain(String),
    Secret(ServerSecretSlot),
}

impl MaybeSecret {
    /// Build a plain `MaybeSecret` from any string-like value.
    #[must_use]
    pub fn plain(v: impl Into<String>) -> Self {
        Self::Plain(v.into())
    }

    /// Build a secret `MaybeSecret` from cleartext.  Mints a fresh
    /// `reveal_id`.
    #[must_use]
    pub fn secret(cleartext: impl Into<String>) -> Self {
        Self::Secret(ServerSecretSlot::new(cleartext))
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

#[cfg(test)]
mod secret_tests {
    use super::{MaybeSecret, ServerSecretSlot};

    #[test]
    fn server_secret_slot_serializes_to_wire_redacted() {
        let slot = ServerSecretSlot::new("super-secret-cookie-value");
        let json = serde_json::to_value(&slot).unwrap();
        assert_eq!(json["redacted"], serde_json::Value::Bool(true));
        assert!(json.get("reveal_id").is_some());
        assert!(json.get("cleartext").is_none(), "cleartext must not leak");
        let serialized = serde_json::to_string(&slot).unwrap();
        assert!(
            !serialized.contains("super-secret-cookie-value"),
            "cleartext leaked in serialized form: {serialized}"
        );
    }

    #[test]
    fn maybe_secret_plain_passes_through() {
        let plain = MaybeSecret::plain("application/json");
        let json = serde_json::to_value(&plain).unwrap();
        assert_eq!(json, serde_json::Value::String("application/json".into()));
    }

    #[test]
    fn maybe_secret_secret_redacts() {
        let s = MaybeSecret::secret("bearer-token-abc");
        let serialized = serde_json::to_string(&s).unwrap();
        assert!(serialized.contains("\"redacted\":true"));
        assert!(!serialized.contains("bearer-token-abc"));
    }

    #[test]
    fn reveal_ids_are_unique_per_slot() {
        let a = ServerSecretSlot::new("v");
        let b = ServerSecretSlot::new("v");
        assert_ne!(a.reveal_id, b.reveal_id, "each slot mints a fresh id");
    }
}
