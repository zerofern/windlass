//! [`windlass_machine::Shell`] for [`windlass_vpn_core::VpnMachine`].
//!
//! Intentionally inert: the machine is a pure translator driven by the
//! tunnel bridge (`tunnel_bridge.rs`), emits no actions, and all
//! privileged `WireGuard` I/O lives in [`windlass_tunnel_core`] +
//! [`windlass_net`].  The legacy Gluetun mode (container polling,
//! IP/port file watching) was removed with the rest of the
//! Gluetun-aware code paths.

use tokio::sync::mpsc::UnboundedSender;
use windlass_machine::{Shell, Timed};
use windlass_vpn_core::{VpnAction, VpnEvent};

pub struct VpnShell;

impl Shell for VpnShell {
    type Config = ();
    type Event = VpnEvent;
    type Action = VpnAction;

    async fn new((): Self::Config, _event_tx: UnboundedSender<Timed<Self::Event>>) -> Self {
        Self
    }

    fn dispatch(&mut self, action: VpnAction, _event_tx: &UnboundedSender<Timed<VpnEvent>>) {
        // `VpnAction` is uninhabited; this match proves at the type
        // level that the machine can never ask the shell for I/O.
        match action {}
    }
}
