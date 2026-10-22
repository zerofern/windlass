//! Disk shell — pure pass-through for `DiskMachine`.
//!
//! `DiskMachine` is a sans-I/O decider with no `DiskAction` variants — it
//! only consumes `DiskEvent::DiskSpaceObserved { free_bytes }` events and
//! publishes `BelowFloor` / `AboveFloor`.  The shell therefore has no
//! actions to dispatch; its sole purpose is to satisfy the `Shell` trait
//! so the runtime can spawn the machine alongside the others in
//! `init_shell`.
//!
//! Events flow in from the `service_events.rs` bridge: when the legacy
//! disk-poll fires `Event::DiskSpaceObserved`, the bridge translates to
//! `DiskEvent::DiskSpaceObserved` and the runtime delivers it to this
//! shell's event channel, which feeds it back to the machine.
use tokio::sync::mpsc::UnboundedSender;

use windlass_disk_core::{DiskAction, DiskEvent};
use windlass_machine::{Shell, Timed};

pub struct DiskShell;

impl Shell for DiskShell {
    type Config = ();
    type Event = DiskEvent;
    type Action = DiskAction;

    async fn new(_config: Self::Config, _event_tx: UnboundedSender<Timed<DiskEvent>>) -> Self {
        Self
    }

    fn dispatch(&mut self, action: DiskAction, _event_tx: &UnboundedSender<Timed<DiskEvent>>) {
        // `DiskAction` is uninhabited — this match is exhaustive and
        // unreachable.  Pattern match defensively so future variants
        // surface as compile errors.
        match action {}
    }
}
