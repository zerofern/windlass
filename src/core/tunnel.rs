//! Prove the VPN tunnel carries traffic, and move to another Proton VPN server
//! when it does not, within MAM's limit on how often the VPN IP may change.

use std::{
    net::IpAddr,
    time::{Duration, Instant, SystemTime},
};

use nutype::nutype;
use serde::Serialize;

use super::{Action, ClockReading, TimerId, TimerName, timer::Timers};

/// How often a working tunnel is checked.
pub const CHECK_INTERVAL: Duration = Duration::from_mins(15);
/// How soon a failed check is repeated.
pub const RETRY_AFTER: Duration = Duration::from_mins(1);
/// Consecutive failed checks before Windlass moves to another server.
pub const FAILURES_BEFORE_SWITCH: u32 = 3;
/// MAM requires the VPN IP to change less than once an hour.
pub const SWITCH_SPACING: Duration = Duration::from_hours(1);
/// A suspected problem does not repeat a check started this recently.
const SUSPICION_SPACING: Duration = Duration::from_secs(30);

/// A configured Proton VPN server: the instance name of its NixOS unit
/// `windlass-wireguard@<name>.service`.
#[nutype(
    validate(predicate = |name: &str| {
        (1..=64).contains(&name.len())
            && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    }),
    derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Display, AsRef, Serialize)
)]
pub struct VpnServer(String);

/// Why an exit-IP check failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ExitIpFailure {
    /// No connection, or no answer in time.
    Unreachable,
    /// The service answered with an unexpected HTTP status.
    Rejected {
        /// The HTTP status code.
        status: u16,
    },
    /// The answer was not an IP address.
    Malformed,
}

/// A server switch, saved so that the spacing survives a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VpnSwitch {
    /// The server switched to.
    pub server: VpnServer,
    /// When the switch started.
    pub at: SystemTime,
}

/// A tunnel command, check result, or storage result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum TunnelEvent {
    /// Start checking the tunnel; sent when an exit-IP service is configured.
    Enable {
        /// The servers Windlass may switch between, in order.
        servers: Vec<VpnServer>,
        /// The server connected now, if known.
        current: Option<VpnServer>,
    },
    /// The exit-IP service saw this address.
    ExitIpObserved {
        /// The tunnel's public address.
        ip: IpAddr,
    },
    /// The exit-IP check failed.
    ExitIpCheckFailed {
        /// Why.
        reason: ExitIpFailure,
    },
    /// The last switch was read from storage.
    LastSwitchRead {
        /// The last switch, if there was one.
        last_switch: Option<VpnSwitch>,
    },
    /// Reading the last switch failed; switches stay blocked.
    LastSwitchReadFailed,
    /// The switch about to start was saved.
    SwitchSaved,
    /// Saving the switch failed; it does not start.
    SwitchSaveFailed,
    /// The new server's unit started.
    Switched,
    /// Starting the new server's unit failed.
    SwitchFailed,
}

/// Whether traffic is known to flow through the tunnel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    /// No check has finished since startup or the last switch.
    Unknown,
    /// The latest check succeeded.
    Available {
        /// The tunnel's public address.
        exit_ip: IpAddr,
        /// When the check succeeded.
        checked_at: SystemTime,
    },
    /// The latest check failed.
    Unavailable {
        /// When checks started failing.
        since: SystemTime,
    },
}

/// The next or outstanding check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// A check is running.
    Outstanding,
    /// The next check waits for this timer.
    Waiting(TimerId),
    /// Checks wait for a server switch to finish.
    Switching,
}

/// What Windlass knows about the last switch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LastSwitch {
    /// Storage is not open yet.
    #[default]
    Unread,
    /// Reading it.
    Reading,
    /// Read: the last switch, if there was one.
    Known(Option<VpnSwitch>),
    /// It could not be read; switches stay blocked.
    ReadFailed,
}

/// A failover in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failover {
    /// Saving the switch before starting it.
    Saving(VpnSwitch),
    /// Starting the new server's unit.
    Switching(VpnServer),
}

/// When the next automatic switch may start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchAllowance {
    /// Now.
    Now,
    /// Not before this time.
    After(SystemTime),
    /// Not until the last switch is known.
    Unknown,
    /// Never: there is no other server.
    NoAlternative,
}

/// The tunnel checks in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    /// The servers Windlass may switch between.
    pub servers: Vec<VpnServer>,
    /// The server connected now, if known.
    pub current: Option<VpnServer>,
    /// Whether traffic flows through the tunnel.
    pub protection: Protection,
    /// The next or outstanding check.
    pub check: Check,
    /// Consecutive failed checks.
    pub failures: u32,
    /// Why the latest check failed, while checks fail.
    pub last_failure: Option<ExitIpFailure>,
    /// A failover in progress.
    pub failover: Option<Failover>,
    last_check: Instant,
}

/// The core's view of the VPN tunnel.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tunnel {
    /// The last server switch.
    pub last_switch: LastSwitch,
    /// Tunnel checks, when an exit-IP service is configured.
    pub watch: Option<Watch>,
}

impl Tunnel {
    pub(super) const fn new() -> Self {
        Self {
            last_switch: LastSwitch::Unread,
            watch: None,
        }
    }

    pub(super) fn start_reading(&mut self) -> Vec<Action> {
        self.last_switch = LastSwitch::Reading;
        vec![Action::ReadLastVpnSwitch]
    }

    /// The server connected now, if known.
    #[must_use]
    pub fn current(&self) -> Option<&VpnServer> {
        self.watch.as_ref().and_then(|watch| watch.current.as_ref())
    }

    /// When the next automatic switch may start.
    #[must_use]
    pub fn switch_allowance(&self) -> SwitchAllowance {
        if self.watch.as_ref().and_then(Watch::next_server).is_none() {
            return SwitchAllowance::NoAlternative;
        }
        match &self.last_switch {
            LastSwitch::Known(None) => SwitchAllowance::Now,
            LastSwitch::Known(Some(last)) => {
                SwitchAllowance::After(last.at.checked_add(SWITCH_SPACING).unwrap_or(last.at))
            }
            LastSwitch::Unread | LastSwitch::Reading | LastSwitch::ReadFailed => {
                SwitchAllowance::Unknown
            }
        }
    }

    pub(super) fn handle(
        &mut self,
        now: ClockReading,
        event: TunnelEvent,
        timers: &mut Timers,
    ) -> Vec<Action> {
        let allowance = self.switch_allowance();
        match (&mut self.watch, event) {
            (None, TunnelEvent::Enable { servers, current }) => {
                self.watch = Some(Watch {
                    servers,
                    current,
                    protection: Protection::Unknown,
                    check: Check::Outstanding,
                    failures: 0,
                    last_failure: None,
                    failover: None,
                    last_check: now.monotonic,
                });
                vec![Action::CheckExitIp]
            }
            (_, TunnelEvent::LastSwitchRead { last_switch }) => {
                if self.last_switch == LastSwitch::Reading {
                    self.last_switch = LastSwitch::Known(last_switch);
                }
                Vec::new()
            }
            (_, TunnelEvent::LastSwitchReadFailed) => {
                if self.last_switch == LastSwitch::Reading {
                    self.last_switch = LastSwitch::ReadFailed;
                }
                Vec::new()
            }
            (Some(watch), TunnelEvent::ExitIpObserved { ip })
                if watch.check == Check::Outstanding =>
            {
                watch.protection = Protection::Available {
                    exit_ip: ip,
                    checked_at: now.wall,
                };
                watch.failures = 0;
                watch.last_failure = None;
                watch.wait(now, timers, CHECK_INTERVAL)
            }
            (Some(watch), TunnelEvent::ExitIpCheckFailed { reason })
                if watch.check == Check::Outstanding =>
            {
                if !matches!(watch.protection, Protection::Unavailable { .. }) {
                    watch.protection = Protection::Unavailable { since: now.wall };
                }
                watch.failures = watch.failures.saturating_add(1);
                watch.last_failure = Some(reason);
                let due = allowance == SwitchAllowance::Now
                    || matches!(allowance, SwitchAllowance::After(at) if at <= now.wall);
                if watch.failures >= FAILURES_BEFORE_SWITCH
                    && due
                    && let Some(server) = watch.next_server()
                {
                    watch.fail_over(now, server)
                } else {
                    watch.wait(now, timers, RETRY_AFTER)
                }
            }
            (Some(watch), TunnelEvent::SwitchSaved) => match watch.failover.take() {
                Some(Failover::Saving(switch)) => {
                    let server = switch.server.clone();
                    self.last_switch = LastSwitch::Known(Some(switch));
                    watch.failover = Some(Failover::Switching(server.clone()));
                    vec![Action::SwitchVpnServer { server }]
                }
                other => {
                    watch.failover = other;
                    Vec::new()
                }
            },
            (Some(watch), TunnelEvent::Switched) => match watch.failover.take() {
                Some(Failover::Switching(server)) => {
                    watch.current = Some(server);
                    watch.protection = Protection::Unknown;
                    watch.failures = 0;
                    watch.last_failure = None;
                    watch.check_now(now)
                }
                other => {
                    watch.failover = other;
                    Vec::new()
                }
            },
            (Some(watch), TunnelEvent::SwitchSaveFailed | TunnelEvent::SwitchFailed)
                if watch.failover.is_some() =>
            {
                watch.failover = None;
                watch.wait(now, timers, RETRY_AFTER)
            }
            _ => Vec::new(),
        }
    }

    /// Check now when this timer is the current one.
    pub(super) fn timer_expired(&mut self, now: ClockReading, id: TimerId) -> Vec<Action> {
        match &mut self.watch {
            Some(watch) if watch.check == Check::Waiting(id) => watch.check_now(now),
            _ => Vec::new(),
        }
    }

    /// Something suggests the tunnel fails: check it now, unless a check is
    /// running, a switch is in progress, or a check started moments ago.
    pub(super) fn suspect(&mut self, now: ClockReading) -> Vec<Action> {
        match &mut self.watch {
            Some(watch)
                if matches!(watch.check, Check::Waiting(_))
                    && now.monotonic.duration_since(watch.last_check) >= SUSPICION_SPACING =>
            {
                watch.check_now(now)
            }
            _ => Vec::new(),
        }
    }
}

impl Watch {
    fn check_now(&mut self, now: ClockReading) -> Vec<Action> {
        self.check = Check::Outstanding;
        self.last_check = now.monotonic;
        vec![Action::CheckExitIp]
    }

    fn wait(&mut self, now: ClockReading, timers: &mut Timers, after: Duration) -> Vec<Action> {
        let (id, deadline) = timers.schedule(now.monotonic, after);
        self.check = Check::Waiting(id);
        vec![Action::StartTimer {
            name: TimerName::TunnelCheck,
            id,
            deadline,
        }]
    }

    /// The server after the current one, in configured order.
    #[must_use]
    pub fn next_server(&self) -> Option<VpnServer> {
        let current = self.current.as_ref();
        let start = current
            .and_then(|current| self.servers.iter().position(|server| server == current))
            .map_or(0, |position| position + 1);
        self.servers
            .iter()
            .cycle()
            .skip(start)
            .take(self.servers.len())
            .find(|server| Some(*server) != current)
            .cloned()
    }

    /// Save, then start, a switch to this server.
    fn fail_over(&mut self, now: ClockReading, server: VpnServer) -> Vec<Action> {
        let switch = VpnSwitch {
            server,
            at: now.wall,
        };
        self.check = Check::Switching;
        self.failover = Some(Failover::Saving(switch.clone()));
        vec![Action::SaveVpnSwitch { switch }]
    }
}
