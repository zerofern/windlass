//! Stub [`windlass_machine::Shell`] for [`windlass_vpn_core::VpnMachine`].
//!
//! Phase 5 of the vpn branch (`docs/vpn-ownership.md`) retired the
//! Gluetun-aware implementation that used to live here:
//! file watching, `ifconfig.co` + MAM `/json/jsonIp.php` cross-checks,
//! and the Gluetun HTTP proxy.  Those concerns are now owned by
//! [`windlass_tunnel_core`] + [`windlass_net`] which talk to the
//! kernel `WireGuard` interface directly.
//!
//! [`VpnMachine`] still runs in tunnel mode as a thin state
//! translator: the bridge in `init.rs` synthesizes [`VpnEvent`]s from
//! [`windlass_tunnel_core::TunnelPublish`]es so the domain core's
//! existing [`windlass_vpn_core::VpnPublish`] consumers keep
//! working unchanged.  This stub shell satisfies the runtime's
//! requirement that every machine has a paired shell; every
//! [`VpnAction`] it receives is now a no-op because the actual I/O
//! lives in [`windlass_net::TunnelShell`].

use tokio::sync::mpsc::UnboundedSender;
use windlass_machine::{Shell, Timed};
use windlass_vpn_core::{VpnAction, VpnEvent};

pub struct VpnShellConfig;

pub struct VpnShell;

impl Shell for VpnShell {
    type Config = VpnShellConfig;
    type Event = VpnEvent;
    type Action = VpnAction;

    async fn new(_config: Self::Config, _event_tx: UnboundedSender<Timed<Self::Event>>) -> Self {
        Self
    }

    fn dispatch(&mut self, _action: VpnAction, _event_tx: &UnboundedSender<Timed<VpnEvent>>) {
        // All VpnActions (InspectContainer, ReadPortFiles, VerifyPublicIp,
        // VerifyMamIp, StartMonitoring, ScheduleTimer) are dead in tunnel
        // mode: the tunnel core owns interface state, port forwarding,
        // and leak detection.  Receiving an action here just means the
        // VpnMachine produced one — we drop it because nothing privileged
        // needs to run.  Future cleanup will trim these variants out of
        // VpnAction entirely.
    }
}
