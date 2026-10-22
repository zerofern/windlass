#![warn(clippy::all, clippy::pedantic, clippy::nursery)]
#![allow(clippy::module_name_repetitions)]

//! Imperative shell for the in-process `WireGuard` tunnel.
//!
//! Sibling to [`windlass_tunnel_core`].  Translates the typed
//! [`windlass_tunnel_core::TunnelAction`] requests the sans-IO core
//! produces into the privileged I/O that brings up the kernel
//! `WireGuard` interface, installs the nftables kill switch, talks
//! NAT-PMP to the `ProtonVPN` gateway, polls handshake age, and runs
//! the leak probe — then reports back via typed
//! [`windlass_tunnel_core::TunnelEvent`]s.
//!
//! See `docs/vpn-ownership.md` for the design rationale and the
//! acceptance criteria the privileged work must satisfy.
//!
//! ## Module map
//!
//! - [`command`] — `tokio::process` wrapper for the subprocess-based
//!   `wg`, `ip`, and `nft` calls.  Single chokepoint so observability
//!   sees every privileged invocation.  The migration path to
//!   in-process netlink ([`rtnetlink`](https://crates.io/crates/rtnetlink) +
//!   [`wireguard-uapi`](https://crates.io/crates/wireguard-uapi)) goes
//!   through this module: every call site uses [`Runner`] today and
//!   would swap to a netlink-handle later without touching the
//!   state machine or the [`TunnelShell`].
//! - [`handshake`] — parses `wg show <iface> latest-handshakes` and
//!   computes age in seconds against `chrono::Utc::now()`.  Pure
//!   parser; the subprocess call lives in [`shell`].
//! - [`natpmp`] — async UDP client for the
//!   [`windlass_tunnel_core::natpmp`] codec.  Sends the encoded
//!   request to the `ProtonVPN` gateway, awaits the response within
//!   a configurable timeout, returns the typed
//!   [`windlass_tunnel_core::NatPmpLease`] or a typed error.
//! - [`probe`] — leak probe.  Enumerates kernel interfaces (via
//!   `ip -j link` for now) and confirms only the tunnel interface
//!   and `lo` carry routes.
//! - [`shell`] — [`TunnelShell`] implementation of the
//!   [`windlass_machine::Shell`] trait.  Receives `TunnelAction`s,
//!   spawns the I/O, and emits `TunnelEvent`s back via the
//!   runtime's `event_tx`.

pub mod command;
pub mod handshake;
pub mod natpmp;
pub mod probe;
pub mod shell;

pub use command::{CommandError, CommandOutcome, Runner, SystemRunner};
pub use handshake::{HandshakeAge, HandshakeParseError, latest_handshake_age};
pub use natpmp::{NatPmpClient, NatPmpClientError};
pub use probe::{InterfaceSnapshot, LeakProbeReport, ProbeError};
pub use shell::{TunnelShell, TunnelShellConfig};
