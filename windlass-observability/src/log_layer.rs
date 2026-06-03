//! Tracing layer that forwards log events into the observability
//! SSE broadcast as [`SseMessage::Log`].
//!
//! The layer holds a weak reference to the controller so dropping
//! the controller during shutdown doesn't keep a dangling tracing
//! subscriber alive.  On every tracing event the layer captures the
//! level, target, and message and emits a `Log` SSE message.

use std::sync::Weak;

use chrono::Utc;
use tracing::Level;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

use crate::ObservabilityController;
use crate::sse::{SseMessage, StoredLogLine};

/// A `tracing_subscriber` layer that publishes log events into the
/// observability SSE stream.
pub struct ObservabilityLogLayer {
    controller: Weak<ObservabilityController>,
}

impl ObservabilityLogLayer {
    #[must_use]
    pub const fn new(controller: Weak<ObservabilityController>) -> Self {
        Self { controller }
    }
}

impl<S: tracing::Subscriber> Layer<S> for ObservabilityLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let Some(ctrl) = self.controller.upgrade() else {
            return;
        };
        let meta = event.metadata();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        ctrl.publish_log(SseMessage::Log(StoredLogLine {
            at: Utc::now(),
            level: level_str(*meta.level()).to_owned(),
            target: meta.target().to_owned(),
            message: visitor.message,
        }));
    }
}

const fn level_str(level: Level) -> &'static str {
    match level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARN",
        Level::INFO => "INFO",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "TRACE",
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            value.clone_into(&mut self.message);
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }
}
