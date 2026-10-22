use std::future::Future;

use tokio::sync::mpsc::UnboundedSender;

use crate::machine::Timed;

/// Imperative shell that executes actions produced by a machine.
///
/// Shells own I/O and async runtime concerns. Any result a machine needs to see
/// should be sent back as a timed event through `event_tx`.
pub trait Shell: Sized {
    type Config;
    type Event: Send + 'static;
    type Action: Send + 'static;

    fn new(
        config: Self::Config,
        event_tx: UnboundedSender<Timed<Self::Event>>,
    ) -> impl Future<Output = Self> + Send;

    fn dispatch(&mut self, action: Self::Action, event_tx: &UnboundedSender<Timed<Self::Event>>);
}
