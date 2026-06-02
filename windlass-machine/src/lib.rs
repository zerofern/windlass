#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

pub mod machine;
pub mod pubsub;
pub mod runtime;
pub mod shell;

pub use machine::{CommandOutcome, EventCause, ExternalCause, Machine, Outcome, Timed};
pub use pubsub::{HasTopic, SubscriberReg, TopicFanout};
pub use runtime::{Command, ServiceHandles, ServiceRuntime, spawn};
pub use shell::Shell;
