use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};
use windlass_core::Observation;
use windlass_core::events::Event;
use windlass_debug::DebugController;
use windlass_domain_core::WindlassMachine;
use windlass_machine::Command;
use windlass_observability::ObservabilityController;

/// Shared state threaded through every axum handler via [`axum::extract::State`].
#[derive(Clone)]
pub struct AppState {
    /// Channel for injecting events into the core event loop.
    pub event_tx: mpsc::Sender<Event>,
    /// §36 step 5: channel for dispatching commands to the domain
    /// runtime (e.g. `WindlassCommand::ManualDownload`).
    pub domain_command_tx: mpsc::UnboundedSender<Command<WindlassMachine>>,
    /// Legacy debug controller — vestigial after §37d closeout; kept
    /// only so the existing `stream`/`health` routes still compile
    /// until §37j retires them.
    pub debug_ctrl: DebugController,
    /// §37h: live observability backend.  Routes call into this for
    /// SSE subscriptions, pause/resume/step, and breakpoint
    /// management.  Threaded into every `ServiceRuntime::spawn` so
    /// gates and tap calls flow through it.
    pub observability: Arc<ObservabilityController>,
    /// Broadcast channel for live observations streamed to SSE clients.
    pub observations: broadcast::Sender<Observation>,
    /// URL of the chaos controller, if running (dev stack only).
    /// Set via `CHAOS_URL` env var. `None` in production.
    pub chaos_url: Option<String>,
    /// Postgres connection pool for reading persistent state from the web layer.
    pub db_pool: windlass_db::DbPool,
}
