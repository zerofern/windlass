use serde::Serialize;
use crate::actions::Action;
use crate::events::Event;
use crate::types::SystemState;

#[derive(Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum Observation {
    StateSnapshot(SystemState),
    EventReceived(Event),
    ActionDispatched(Action),
}
