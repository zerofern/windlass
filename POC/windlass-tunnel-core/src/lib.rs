#![warn(clippy::all, clippy::pedantic, clippy::nursery)]
#![allow(clippy::module_name_repetitions)]

//! Pure, sans-IO state machine and protocol codecs for the in-process
//! `WireGuard` tunnel owned by Windlass in-process.
//!
//! See `docs/vpn-ownership.md` for the design rationale, objectives,
//! external requirements, and acceptance criteria.
//!
//! ## Module map
//!
//! - [`config`] — parser for the `ProtonVPN`-generated `wg.conf` file.
//!   Produces a validated [`config::WgConfig`] or a typed
//!   [`config::WgConfigError`].
//! - [`natpmp`] — RFC 6886 / `ProtonVPN` dialect codec.  Pure
//!   request/response byte encoding and decoding.  The UDP socket I/O
//!   lives in the shell crate.
//! - [`machine`] — the [`machine::TunnelMachine`] state machine.
//!   Consumes typed events, produces typed actions and publishes,
//!   carries the tunnel's authoritative state.

pub mod config;
pub mod machine;
pub mod natpmp;

pub use config::{Endpoint, PeerConfig, WgConfig, WgConfigError};
pub use machine::{
    ExitIpFailure, FirewallInstallFailure, InterfaceConfigureFailure, LeakProbeOutcome,
    NatPmpFailure, NatPmpFailureThreshold, PeerCount, PortRenewalBasisPoints,
    StallCountBeforeRotate, TunnelAction, TunnelCommand, TunnelConfig, TunnelEvent, TunnelHealth,
    TunnelMachine, TunnelPublish, TunnelResponse, TunnelTimer, TunnelTopic,
};
pub use natpmp::{NatPmpDecodeError, NatPmpLease, NatPmpRequest, NatPmpResponseCode};
