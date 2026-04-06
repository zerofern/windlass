use crate::actions::Action;
use crate::events::Event;
use crate::types::SystemState;
use serde::Serialize;

#[derive(Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum Observation {
    StateSnapshot(SystemState),
    EventReceived(Event),
    ActionDispatched(Action),
    /// Emitted by HTTP clients when debug mode is active.
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
