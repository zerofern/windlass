use crate::actions::Action;
use crate::events::Event;
use crate::types::SystemState;
use serde::Serialize;

#[derive(Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum Observation {
    /// Emitted by the intake task the moment an event enters the system,
    /// before the main loop has had a chance to process it.
    EventArrived(Event),
    /// Emitted by the main loop when an event is about to be processed
    /// (after any debug-mode pause has been released).
    EventReceived(Event),
    StateSnapshot(SystemState),
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
