use std::sync::Arc;

use tokio::sync::mpsc;
use windlass_domain_core::WindlassMachine;
use windlass_machine::Command;
use windlass_observability::ObservabilityController;

/// Shared state threaded through every axum handler via [`axum::extract::State`].
#[derive(Clone)]
pub struct AppState {
    /// Channel for dispatching commands to the domain runtime
    /// (e.g. `WindlassCommand::ManualDownload`).
    pub domain_command_tx: mpsc::UnboundedSender<Command<WindlassMachine>>,
    /// The live observability backend.  Routes call into this for the
    /// SSE stream, pause/resume/step, and breakpoint management.
    /// Constructed once at startup and threaded into every
    /// `ServiceRuntime::spawn` so gates and tap calls flow through it.
    pub observability: Arc<ObservabilityController>,
    /// Postgres connection pool for reading persistent state from the web layer.
    pub db_pool: windlass_db::DbPool,
}
