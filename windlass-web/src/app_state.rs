use std::sync::Arc;
use tokio::sync::{RwLock, broadcast, mpsc};
use windlass_core::Observation;
use windlass_core::events::Event;
use windlass_core::types::SystemState;
use windlass_debug::DebugController;

/// Shared state threaded through every axum handler via [`axum::extract::State`].
#[derive(Clone)]
pub struct AppState {
    /// Channel for injecting events into the core event loop.
    pub event_tx: mpsc::Sender<Event>,
    /// Latest [`SystemState`] written by the event loop after each transition.
    pub state: Arc<RwLock<SystemState>>,
    /// Debug controller — freeze flag, event/action queues, breakpoints, step permits.
    pub debug_ctrl: DebugController,
    /// Broadcast channel for live observations streamed to SSE clients.
    pub observations: broadcast::Sender<Observation>,
    /// URL of the chaos controller, if running (dev stack only).
    /// Set via `CHAOS_URL` env var. `None` in production.
    pub chaos_url: Option<String>,
}
