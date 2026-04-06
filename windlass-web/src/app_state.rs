use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use windlass_core::events::Event;
use windlass_core::types::SystemState;
use windlass_types::DebugGate;

/// Shared state threaded through every axum handler via [`axum::extract::State`].
#[derive(Clone)]
pub struct AppState {
    /// Channel for injecting events into the core event loop.
    pub event_tx: mpsc::Sender<Event>,
    /// Latest [`SystemState`] written by the event loop after each transition.
    pub state: Arc<RwLock<SystemState>>,
    /// Freeze flag — when set, the event loop drops all incoming events.
    pub debug_gate: DebugGate,
}
