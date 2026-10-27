//! Keeping qBittorrent connectable (B-0011 A6). When MAM reports the client
//! not connectable, Windlass checks and repairs what it can, then lets a later
//! reading show whether that worked.

use std::{
    net::IpAddr,
    time::{Duration, Instant, SystemTime},
};

use serde::Serialize;

use crate::core::{NetworkInterface, Port};

/// A reading taken this long after a repair shows whether it worked; MAM caches
/// what it reports about clients for 30 minutes.
pub const REPAIR_SETTLES_AFTER: Duration = Duration::from_mins(30);

/// What the rest of Windlass knows about the connection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Connection {
    /// The port qBittorrent listens on.
    pub listen_port: Option<Port>,
    /// The port the VPN forwards.
    pub forwarded_port: Option<Port>,
    /// The tunnel's exit IP, from the latest successful check.
    pub tunnel_ip: Option<IpAddr>,
    /// The network interface qBittorrent is bound to.
    pub interface: Option<NetworkInterface>,
}

/// qBittorrent's listen port against the forwarded port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PortFinding {
    /// qBittorrent listens on the forwarded port.
    Matches(Port),
    /// It listens elsewhere; Windlass is setting the forwarded port.
    Differs {
        /// The port qBittorrent listens on, if known.
        listening: Option<Port>,
        /// The forwarded port.
        forwarded: Port,
    },
    /// The VPN forwards no port.
    NoForwardedPort,
}

/// The IP MAM sees against the tunnel's exit IP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum IpFinding {
    /// They match.
    Matches(IpAddr),
    /// They differ; the seedbox registration follows the IP MAM sees.
    Differs {
        /// The IP MAM sees.
        mam: IpAddr,
        /// The tunnel's exit IP.
        tunnel: IpAddr,
    },
    /// One of them is not known.
    Unknown,
}

/// What Windlass found when it checked the connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Findings {
    /// qBittorrent's listen port against the forwarded port.
    pub listen_port: PortFinding,
    /// Whether a new forwarded-port lease was asked for.
    pub lease_renewed: bool,
    /// The IP MAM sees against the tunnel's.
    pub exit_ip: IpFinding,
    /// Whether MAM had confirmed the seedbox registration for the IP it sees;
    /// Windlass registers again when allowed either way.
    pub registered: bool,
    /// The network interface qBittorrent is bound to, if any.
    pub interface: Option<NetworkInterface>,
}

impl Findings {
    /// Check the connection as the rest of Windlass knows it.
    pub(super) fn check(connection: &Connection, mam_ip: Option<IpAddr>, registered: bool) -> Self {
        let listen_port = match (connection.listen_port, connection.forwarded_port) {
            (_, None) => PortFinding::NoForwardedPort,
            (Some(listening), Some(forwarded)) if listening == forwarded => {
                PortFinding::Matches(forwarded)
            }
            (listening, Some(forwarded)) => PortFinding::Differs {
                listening,
                forwarded,
            },
        };
        let exit_ip = match (mam_ip, connection.tunnel_ip) {
            (Some(mam), Some(tunnel)) if mam == tunnel => IpFinding::Matches(mam),
            (Some(mam), Some(tunnel)) => IpFinding::Differs { mam, tunnel },
            _ => IpFinding::Unknown,
        };
        Self {
            listen_port,
            lease_renewed: connection.forwarded_port.is_some(),
            exit_ip,
            registered,
            interface: connection.interface.clone(),
        }
    }
}

/// Whether MAM can reach qBittorrent, and what Windlass did about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Connectability {
    /// MAM reaches qBittorrent, or has nothing to test.
    #[default]
    Fine,
    /// MAM could not reach it; Windlass repaired what it could and waits for a
    /// reading taken long enough after.
    Repaired {
        /// When, by the process clock.
        at: Instant,
        /// When, by the calendar.
        wall: SystemTime,
        /// What the check found.
        findings: Findings,
    },
    /// MAM still could not reach it after the repair; the owner is told.
    Unresolved {
        /// When the repair was made.
        repaired: SystemTime,
        /// What the check found.
        findings: Findings,
    },
}

impl Connectability {
    /// Follow the latest reading: when it was taken and whether MAM could not
    /// reach qBittorrent. Whether a repair is wanted now.
    pub(super) fn follow(&mut self, reading: Option<(Instant, bool)>) -> bool {
        let Some((taken, unreachable)) = reading else {
            return false;
        };
        match self {
            Self::Fine => unreachable,
            Self::Repaired { at, wall, findings } => {
                let settled = at
                    .checked_add(REPAIR_SETTLES_AFTER)
                    .is_some_and(|settled| taken >= settled);
                if settled {
                    *self = if unreachable {
                        Self::Unresolved {
                            repaired: *wall,
                            findings: findings.clone(),
                        }
                    } else {
                        Self::Fine
                    };
                }
                false
            }
            Self::Unresolved { .. } => {
                if !unreachable {
                    *self = Self::Fine;
                }
                false
            }
        }
    }

    pub(super) fn repaired(&mut self, now: Instant, wall: SystemTime, findings: Findings) {
        *self = Self::Repaired {
            at: now,
            wall,
            findings,
        };
    }
}
