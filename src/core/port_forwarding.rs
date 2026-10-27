use std::{
    num::NonZeroU16,
    time::{Duration, Instant, SystemTime},
};

use derive_more::Display;
use serde::Serialize;

use crate::core::FailureCount;

use super::{Action, ClockReading, TimerId, TimerName, timer::Timers};

/// The lease lifetime requested from Proton VPN's NAT-PMP gateway.
pub const LEASE_LIFETIME: Duration = Duration::from_mins(1);
/// How soon a failed request is retried.
const RETRY_AFTER: Duration = Duration::from_secs(5);

/// A TCP or UDP port number other than zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Port(NonZeroU16);

impl Port {
    /// A port, unless the number is zero.
    #[must_use]
    pub const fn new(number: u16) -> Option<Self> {
        match NonZeroU16::new(number) {
            Some(number) => Some(Self(number)),
            None => None,
        }
    }

    /// The port number.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

/// The NAT-PMP result codes other than success (RFC 6886, section 3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, Serialize)]
pub enum NatPmpRefusal {
    /// 1: the gateway does not speak this NAT-PMP version.
    #[display("unsupported version")]
    UnsupportedVersion,
    /// 2: mapping is turned off or not allowed.
    #[display("not authorized")]
    NotAuthorized,
    /// 3: the gateway has no working network connection.
    #[display("network failure")]
    NetworkFailure,
    /// 4: the gateway cannot hold another mapping.
    #[display("out of resources")]
    OutOfResources,
    /// 5: the gateway does not support this request.
    #[display("unsupported opcode")]
    UnsupportedOpcode,
}

/// Why the gateway did not grant a mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MappingFailure {
    /// The gateway did not answer in time.
    NoResponse,
    /// The gateway answered with a NAT-PMP result code other than success.
    Refused {
        /// Why, as the result code says.
        reason: NatPmpRefusal,
    },
    /// The answer was not a valid NAT-PMP response.
    Malformed,
}

/// A port-forwarding command or a NAT-PMP result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PortForwardingEvent {
    /// Start requesting a forwarded port; sent when a gateway is configured.
    Enable,
    /// The gateway granted TCP and UDP mappings.
    Mapped {
        /// The public TCP port.
        tcp: Port,
        /// The public UDP port.
        udp: Port,
        /// How long the gateway keeps the mappings.
        lifetime: Duration,
    },
    /// The gateway did not grant the mappings.
    MappingFailed {
        /// Why not.
        reason: MappingFailure,
    },
}

/// A granted lease on the forwarded ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    /// The forwarded TCP port; qBittorrent listens on it.
    pub tcp: Port,
    /// The forwarded UDP port.
    pub udp: Port,
    /// When the lease was last granted or renewed.
    pub renewed_at: SystemTime,
    expires: Instant,
}

/// Whether a lease request is outstanding or waiting for its timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseRequest {
    /// The shell is asking the gateway.
    Outstanding,
    /// The next request waits for this timer.
    Waiting(TimerId),
}

/// The core's view of Proton VPN port forwarding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PortForwarding {
    /// No NAT-PMP gateway is configured.
    #[default]
    Disabled,
    /// Leases are requested and renewed.
    Enabled {
        /// The current lease, until it expires.
        lease: Option<Lease>,
        /// The next or outstanding request.
        request: LeaseRequest,
        /// Requests that failed since the last success.
        failures: FailureCount,
        /// Why the latest request failed, if it did.
        last_failure: Option<MappingFailure>,
    },
}

impl PortForwarding {
    /// The port qBittorrent should listen on, while a lease is valid.
    #[must_use]
    pub const fn forwarded_port(&self) -> Option<Port> {
        match self {
            Self::Enabled {
                lease: Some(lease), ..
            } => Some(lease.tcp),
            Self::Enabled { lease: None, .. } | Self::Disabled => None,
        }
    }

    pub(super) fn handle(
        &mut self,
        now: ClockReading,
        event: PortForwardingEvent,
        timers: &mut Timers,
    ) -> Vec<Action> {
        let request = Action::RequestPortMapping {
            lifetime: LEASE_LIFETIME,
        };
        match (&mut *self, event) {
            (Self::Disabled, PortForwardingEvent::Enable) => {
                *self = Self::Enabled {
                    lease: None,
                    request: LeaseRequest::Outstanding,
                    failures: FailureCount::NONE,
                    last_failure: None,
                };
                vec![request]
            }
            (
                Self::Enabled {
                    lease,
                    request: request_state @ LeaseRequest::Outstanding,
                    failures,
                    last_failure,
                },
                PortForwardingEvent::Mapped { tcp, udp, lifetime },
            ) if !lifetime.is_zero() => {
                *lease = Some(Lease {
                    tcp,
                    udp,
                    renewed_at: now.wall,
                    expires: now.monotonic.checked_add(lifetime).unwrap_or(now.monotonic),
                });
                *failures = FailureCount::NONE;
                *last_failure = None;
                // Renew with a quarter of the lifetime to spare, as Proton advises.
                let (id, deadline) = timers.schedule(now.monotonic, lifetime * 3 / 4);
                *request_state = LeaseRequest::Waiting(id);
                vec![Action::StartTimer {
                    name: TimerName::PortLease,
                    id,
                    deadline,
                }]
            }
            (
                Self::Enabled {
                    request: request_state @ LeaseRequest::Outstanding,
                    failures,
                    last_failure,
                    ..
                },
                PortForwardingEvent::Mapped { .. } | PortForwardingEvent::MappingFailed { .. },
            ) => {
                *failures = failures.after_another();
                *last_failure = Some(match event {
                    PortForwardingEvent::MappingFailed { reason } => reason,
                    _ => MappingFailure::Malformed,
                });
                let (id, deadline) = timers.schedule(now.monotonic, RETRY_AFTER);
                *request_state = LeaseRequest::Waiting(id);
                vec![Action::StartTimer {
                    name: TimerName::PortLease,
                    id,
                    deadline,
                }]
            }
            _ => Vec::new(),
        }
    }

    /// Request a lease when this timer is the current one.
    pub(super) fn timer_expired(&mut self, id: TimerId) -> Vec<Action> {
        match self {
            Self::Enabled { request, .. } if *request == LeaseRequest::Waiting(id) => {
                *request = LeaseRequest::Outstanding;
                vec![Action::RequestPortMapping {
                    lifetime: LEASE_LIFETIME,
                }]
            }
            _ => Vec::new(),
        }
    }

    /// Forget the lease and ask for a new one now: the gateway changed.
    pub(super) fn renew(&mut self) -> Vec<Action> {
        match self {
            Self::Enabled {
                lease,
                request: request @ LeaseRequest::Waiting(_),
                ..
            } => {
                *lease = None;
                *request = LeaseRequest::Outstanding;
                vec![Action::RequestPortMapping {
                    lifetime: LEASE_LIFETIME,
                }]
            }
            // An outstanding request already reaches the new gateway.
            Self::Enabled { lease, .. } => {
                *lease = None;
                Vec::new()
            }
            Self::Disabled => Vec::new(),
        }
    }

    /// Forget a lease once its lifetime has passed.
    pub(super) fn expire(&mut self, now: Instant) {
        if let Self::Enabled { lease, .. } = self
            && lease.is_some_and(|lease| lease.expires <= now)
        {
            *lease = None;
        }
    }
}
