use std::sync::Arc;

use crate::types::SystemState;
use serde::Serialize;
use windlass_types::HttpExchange;

/// Type-erased callback for HTTP observation. Injected into clients at
/// construction; the implementation in `windlass-debug` routes to the debug
/// exchange channel when debug mode is active.
pub type HttpObserver = Arc<dyn Fn(HttpExchange) + Send + Sync>;

#[derive(Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum Observation {
    StateSnapshot(Box<SystemState>),
    /// Emitted when debug mode is enabled or disabled, including automatic
    /// entry triggered by the MAM rate-limit guardrail.
    DebugModeChanged(bool),
}
