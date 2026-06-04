#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

pub mod causal;
pub mod machine;
pub mod pubsub;
pub mod runtime;
pub mod shell;
pub mod tap;

pub use machine::{
    ActionEnvelope, CommandOutcome, EventCause, ExternalCause, Machine, Outcome, PublishEnvelope,
    StoredEventCause, StoredExternalCause, Timed, variant_name,
};
pub use pubsub::{HasTopic, SubscriberReg, TopicFanout};
pub use runtime::{Command, ServiceHandles, ServiceRuntime, spawn};
pub use shell::Shell;
pub use tap::{
    CommandResponseStatus, CoreId, CoreStatus, EventGateView, NullRuntimeTap, OutcomeGateView,
    RuntimeTap, StepKind, StepRecordView,
};
