use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use tokio::sync::mpsc;
use tracing::Level;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

use crate::types::LogEntry;

/// A `tracing_subscriber` layer that forwards log events to the debug system.
///
/// When debug mode is off the layer returns immediately (single atomic load —
/// negligible overhead). When on, it captures the level, target, and message
/// of every `tracing` event and sends them to the main loop via `log_tx`.
///
/// The channel is bounded and `try_send` is used so that a slow debug
/// consumer never blocks the application.
pub struct DebugLogLayer {
    log_tx: mpsc::Sender<LogEntry>,
    debug_mode: Arc<AtomicBool>,
}

impl DebugLogLayer {
    #[must_use]
    pub fn new(log_tx: mpsc::Sender<LogEntry>, debug_mode: Arc<AtomicBool>) -> Self {
        Self { log_tx, debug_mode }
    }
}

impl<S: tracing::Subscriber> Layer<S> for DebugLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if !self.debug_mode.load(Ordering::Relaxed) {
            return;
        }

        let meta = event.metadata();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let _ = self.log_tx.try_send(LogEntry {
            at: Utc::now(),
            level: level_str(meta.level()).to_owned(),
            target: meta.target().to_owned(),
            message: visitor.message,
        });
    }
}

fn level_str(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARN",
        Level::INFO => "INFO",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "TRACE",
    }
}

/// Extracts the `message` field from a `tracing::Event`.
#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_owned();
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }
}
