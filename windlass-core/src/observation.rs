use std::sync::Arc;

use crate::types::SystemState;
use serde::Serialize;

/// Type-erased callback for HTTP observation. Injected into clients at
/// construction; the implementation in `windlass-debug` routes to the SSE
/// channel when debug mode is active.
pub type HttpObserver = Arc<dyn Fn(Observation) + Send + Sync>;

#[derive(Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum Observation {
    StateSnapshot(SystemState),
    /// Emitted when debug mode is enabled or disabled, including automatic
    /// entry triggered by the MAM rate-limit guardrail.
    DebugModeChanged(bool),
    /// Emitted by HTTP clients on every response.
    /// Carries the full request/response detail for the SSE log view.
    HttpExchange {
        /// Which client emitted this: `"qbit"`, `"mam"`, or `"gotify"`.
        module: String,
        method: String,
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        request_body: Option<String>,
        response_status: u16,
        response_body: String,
    },
}
