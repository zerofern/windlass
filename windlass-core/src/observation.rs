use crate::types::SystemState;
use serde::Serialize;

/// §36 step 9a: re-export so existing call sites in `windlass-debug` and
/// the legacy shell wiring keep working; the canonical definition lives
/// in `windlass-types` so the clients can drop the legacy-core dep.
pub use windlass_types::HttpObserver;

#[derive(Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum Observation {
    StateSnapshot(Box<SystemState>),
    /// Emitted when debug mode is enabled or disabled, including automatic
    /// entry triggered by the MAM rate-limit guardrail.
    DebugModeChanged(bool),
}
