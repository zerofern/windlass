#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

pub mod actions;
pub mod events;
pub mod observation;
pub mod types;

pub use observation::Observation;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod prop_tests;

use actions::Action;
use events::Event;
use std::time::Duration;
use tracing::{debug, error, info, warn};
use types::{MamState, QbitState, RunMode, SystemState, VpnState};
use windlass_types::{AlertPriority, Backoff, Interval, MamStatus, RetryCount, WakeupId};

const HARD_RECOVERY_LIMIT: RetryCount = RetryCount(3);
const QBIT_SYNC_RETRY_LIMIT: RetryCount = RetryCount(3);
const HEARTBEAT_INTERVAL: Interval = Interval(Duration::from_secs(45 * 60));
const DISK_CHECK_INTERVAL: Interval = Interval(Duration::from_secs(6 * 60 * 60));
const TORRENT_CHECK_INTERVAL: Interval = Interval(Duration::from_secs(5 * 60));
const PORT_READ_RETRY_DELAY: Backoff = Backoff(Duration::from_millis(500));
const QBIT_AUTH_BACKOFF_BASE: Backoff = Backoff(Duration::from_secs(2));
const QBIT_SYNC_BACKOFF: Backoff = Backoff(Duration::from_secs(2));
/// Short fixed delay for connection-refused retries during container startup.
const QBIT_CONNECTION_RETRY_DELAY: Backoff = Backoff(Duration::from_secs(5));

/// The pure functional core. No I/O, no async, no side effects.
/// All state transitions and action scheduling happen here.
impl SystemState {
    // process_event is a top-level state machine handler. Its length reflects the number
    // of distinct events in the system, not incidental complexity.
    #[allow(clippy::too_many_lines)]
    pub fn process_event(&mut self, event: Event) -> Vec<Action> {
        // Fatal mode: only ManualReset can escape it.
        if matches!(self.run_mode, RunMode::Fatal { .. }) {
            if matches!(event, Event::ManualReset) {
                info!("manual reset: clearing fatal state and restarting Gluetun");
                self.run_mode = RunMode::Active;
                self.hard_recoveries = RetryCount(0);
                self.vpn = VpnState::Starting;
                return vec![Action::RestartGluetun];
            }
            debug!(?event, "fatal mode: ignoring event");
            return vec![];
        }

        let mut actions: Vec<Action> = vec![];

        match event {
            // ── Initialisation ────────────────────────────────────────────────
            Event::Init {
                is_gluetun_healthy,
                port_files,
            } => {
                info!(gluetun_healthy = is_gluetun_healthy, "initialising");
                actions.push(Action::ScheduleWakeup(
                    WakeupId::Heartbeat,
                    HEARTBEAT_INTERVAL.into(),
                ));
                actions.push(Action::ScheduleWakeup(
                    WakeupId::DiskCheck,
                    DISK_CHECK_INTERVAL.into(),
                ));
                actions.push(Action::ScheduleWakeup(
                    WakeupId::TorrentCheck,
                    TORRENT_CHECK_INTERVAL.into(),
                ));

                if is_gluetun_healthy {
                    match port_files {
                        Ok((ip, port)) => {
                            info!(ip = %ip.0, port = port.into_inner(), "boot: VPN already up, fast-forwarding");
                            self.vpn = VpnState::Connected { ip, port };
                            self.qbit = QbitState::Authenticating {
                                attempt: RetryCount(0),
                            };
                            actions.push(Action::AuthenticateQbit);
                        }
                        Err(e) => {
                            // Gluetun healthy but files not ready yet — watcher will fire soon.
                            debug!(err = %e, "boot: VPN files not yet readable, waiting for watcher");
                            self.vpn = VpnState::AwaitingTunnel;
                        }
                    }
                } else {
                    self.vpn = VpnState::DumpingLogs;
                    actions.push(Action::FetchAndDumpAllLogs);
                }
            }

            Event::ManualReset => {
                info!("manual reset: clearing recovery counter");
                self.hard_recoveries = RetryCount(0);
            }

            // ── Workflow A: VPN Drop Recovery ─────────────────────────────────
            Event::DockerGluetunDied => {
                match &self.vpn {
                    // Unexpected crash — dump logs then stop dependents.
                    VpnState::Connected { .. } | VpnState::AwaitingTunnel => {
                        warn!(vpn = %self.vpn, qbit = %self.qbit, "Gluetun died unexpectedly — beginning recovery");
                        self.vpn = VpnState::DumpingLogs;
                        actions.push(Action::FetchAndDumpAllLogs);
                        actions.push(Action::SendGotifyAlert(
                            AlertPriority::Critical,
                            "💀 Gluetun died unexpectedly. Dumping logs and recovering.".into(),
                        ));
                    }
                    // Intentional restart from Hard Recovery — skip the dump.
                    VpnState::Starting | VpnState::DumpingLogs => {
                        debug!("Gluetun died during planned recovery — stopping dependents");
                        actions.push(Action::StopDependentContainers);
                    }
                    VpnState::Stopped => {}
                }
                // qBit and MAM are unreachable until VPN is back.
                self.qbit = QbitState::Offline;
                self.mam = MamState::Unknown;
            }

            Event::LogsDumped => {
                // Fires after both unexpected crashes and Hard Recovery dumps.
                // Always stop dependents and restart Gluetun — the double-dump guard
                // in DockerGluetunDied ensures we don't loop.
                self.vpn = VpnState::Starting;
                actions.push(Action::StopDependentContainers);
                actions.push(Action::RestartGluetun);
            }

            Event::DockerGluetunHealthy => {
                info!("Gluetun healthy — starting dependent containers");
                self.vpn = VpnState::AwaitingTunnel;
                actions.push(Action::StartDependentContainers);
            }

            // ── Workflow B: Port Sync & Tracker Update ────────────────────────
            Event::PortFileReadResult(Ok((ip, port))) => {
                // No-op if content is identical to current state — the debounced
                // watcher sends this event on every write; the Core ignores no-change reads.
                if let VpnState::Connected {
                    ip: cur_ip,
                    port: cur_port,
                } = &self.vpn
                {
                    if *cur_ip == ip && *cur_port == port {
                        debug!(ip = %ip.0, port = port.into_inner(), "VPN files read: no change");
                        return actions;
                    }
                    info!(
                        ip = %ip.0, port = port.into_inner(),
                        old_ip = %cur_ip.0, old_port = cur_port.into_inner(),
                        "VPN reconnected with new address"
                    );
                } else {
                    info!(ip = %ip.0, port = port.into_inner(), "VPN tunnel established");
                }

                self.vpn = VpnState::Connected { ip, port };
                self.qbit = QbitState::Authenticating {
                    attempt: RetryCount(0),
                };
                actions.push(Action::AuthenticateQbit);
            }

            Event::PortFileReadResult(Err(e)) => {
                debug!(err = %e, "VPN port files not ready — scheduling retry");
                actions.push(Action::ScheduleWakeup(
                    WakeupId::RetryPortRead,
                    PORT_READ_RETRY_DELAY.into(),
                ));
            }

            Event::QbitAuthSuccess(cookie) => {
                info!("qBittorrent authenticated");
                if let VpnState::Connected { port, .. } = &self.vpn {
                    let target = *port;
                    self.qbit = QbitState::SyncingPort {
                        attempt: RetryCount(0),
                        cookie: cookie.clone(),
                        target,
                    };
                    actions.push(Action::SyncQbitPort(cookie, target));
                } else {
                    self.qbit = QbitState::Authenticated { cookie };
                }
            }

            // Container not yet reachable — normal during startup. Silent fixed-delay retry.
            Event::QbitConnectionRefused => {
                if matches!(self.qbit, QbitState::Authenticating { .. }) {
                    debug!(
                        delay_secs = QBIT_CONNECTION_RETRY_DELAY.0.as_secs(),
                        "qBittorrent not yet reachable — retrying"
                    );
                    actions.push(Action::ScheduleWakeup(
                        WakeupId::QbitAuthRetry,
                        QBIT_CONNECTION_RETRY_DELAY.into(),
                    ));
                } else {
                    debug!(qbit = %self.qbit, "stale QbitConnectionRefused — ignoring");
                }
            }

            // Credentials rejected — this is a configuration error, not a transient failure.
            Event::QbitAuthFailed => {
                error!(
                    "qBittorrent rejected credentials — check QBITTORRENT_USER / QBITTORRENT_PASS"
                );
                actions.push(Action::SendGotifyAlert(
                AlertPriority::Critical,
                "🔐 qBittorrent rejected credentials. Check QBITTORRENT_USER / QBITTORRENT_PASS."
                    .into(),
            ));
                self.qbit = QbitState::Authenticating {
                    attempt: RetryCount(0),
                };
                actions.push(Action::ScheduleWakeup(
                    WakeupId::QbitAuthRetry,
                    QBIT_AUTH_BACKOFF_BASE.into(),
                ));
            }

            Event::QbitApiError(_) => {
                let attempt = match &self.qbit {
                    QbitState::Authenticating { attempt } => *attempt,
                    _ => RetryCount(0),
                };
                let backoff = QBIT_AUTH_BACKOFF_BASE.exponential(attempt);
                self.qbit = QbitState::Authenticating {
                    attempt: attempt.increment(),
                };
                actions.push(Action::ScheduleWakeup(WakeupId::QbitAuthRetry, backoff));
            }

            Event::QbitPortSyncSuccess => {
                if let QbitState::SyncingPort { target, cookie, .. } = &self.qbit {
                    let port = *target;
                    let cookie = cookie.clone();
                    if let VpnState::Connected { ip, .. } = &self.vpn {
                        let ip = *ip;
                        info!(port = port.into_inner(), "qBittorrent port synced");
                        self.qbit = QbitState::Ready { port, cookie };
                        self.mam = MamState::SyncPending {
                            target_ip: ip,
                            target_port: port,
                        };
                        actions.push(Action::UpdateMam(ip));
                    }
                }
            }

            Event::QbitPortSyncFailed(err_code) => {
                if let QbitState::SyncingPort {
                    attempt,
                    cookie,
                    target,
                } = &self.qbit
                {
                    let attempt = *attempt;
                    let cookie = cookie.clone();
                    let target = *target;
                    if attempt < QBIT_SYNC_RETRY_LIMIT {
                        warn!(
                            port = target.into_inner(),
                            attempt = attempt.0,
                            "qBittorrent port sync failed — retrying"
                        );
                        self.qbit = QbitState::SyncingPort {
                            attempt: attempt.increment(),
                            cookie,
                            target,
                        };
                        actions.push(Action::ScheduleWakeup(
                            WakeupId::QbitSyncRetry,
                            QBIT_SYNC_BACKOFF.into(),
                        ));
                    } else {
                        warn!(
                            port = target.into_inner(),
                            err_code = err_code.0,
                            retries = attempt.0,
                            "qBittorrent port sync failed at retry limit — re-authenticating"
                        );
                        actions.push(Action::SendGotifyAlert(
                        AlertPriority::Warning,
                        format!(
                            "⚠️ qBittorrent rejecting port updates (HTTP {}) after {} attempts. Forcing re-auth.",
                            err_code.0,
                            QBIT_SYNC_RETRY_LIMIT.0,
                        ),
                    ));
                        self.qbit = QbitState::Authenticating {
                            attempt: RetryCount(0),
                        };
                        actions.push(Action::AuthenticateQbit);
                    }
                }
            }

            // ── MAM ───────────────────────────────────────────────────────────
            Event::MamUpdateSuccess => {
                if let VpnState::Connected { ip, port } = &self.vpn {
                    let (ip, port) = (*ip, *port);
                    info!(ip = %ip.0, port = port.into_inner(), "MAM seedbox registered — VPN recovery complete");
                    self.mam = MamState::Synced { port, ip };
                    actions.push(Action::SendGotifyAlert(
                        AlertPriority::Info,
                        "✅ VPN Recovered. Port synced.".into(),
                    ));
                }
            }

            Event::MamAsnMismatch(ip) => {
                warn!(ip = %ip.0, "MAM ASN mismatch — manual IP whitelist required");
                self.mam = MamState::AsnBlocked { ip };
                actions.push(Action::SendGotifyAlert(
                AlertPriority::Critical,
                format!(
                    "🚨 MAM ASN mismatch for {}. Log into MAM and whitelist the new IP manually.",
                    ip.0
                ),
            ));
            }

            // ── Workflow C: Heartbeat & Recovery ──────────────────────────────
            Event::MamStatusObserved(MamStatus::Connectable) => {
                debug!(mam = %self.mam, "MAM reports connectable — heartbeat OK");
                self.hard_recoveries = RetryCount(0);
                actions.push(Action::ScheduleWakeup(
                    WakeupId::Heartbeat,
                    HEARTBEAT_INTERVAL.into(),
                ));
            }

            Event::MamStatusObserved(MamStatus::NotConnectable | MamStatus::Unreachable) => {
                warn!(mam = %self.mam, qbit = %self.qbit, "MAM reports NOT connectable");
                // If ASN is blocked, a human must intervene. Don't attempt recovery.
                if let MamState::AsnBlocked { .. } = &self.mam {
                    debug!("ASN blocked — suppressing recovery");
                    return actions;
                }

                match &self.qbit {
                    // Soft recovery: assume qBit dropped the port, re-trigger Workflow B.
                    QbitState::Ready { .. } | QbitState::Authenticated { .. } => {
                        info!("soft recovery: re-triggering qBit auth");
                        self.qbit = QbitState::Authenticating {
                            attempt: RetryCount(0),
                        };
                        actions.push(Action::AuthenticateQbit);
                        actions.push(Action::ScheduleWakeup(
                            WakeupId::Heartbeat,
                            HEARTBEAT_INTERVAL.into(),
                        ));
                    }
                    // Soft recovery already in flight or qBit offline — escalate.
                    _ => {
                        let recoveries = self.hard_recoveries.increment();
                        self.hard_recoveries = recoveries;

                        if recoveries >= HARD_RECOVERY_LIMIT {
                            error!(
                                recoveries = recoveries.0,
                                limit = HARD_RECOVERY_LIMIT.0,
                                "FATAL: hard recovery limit reached — manual intervention required"
                            );
                            self.run_mode = RunMode::Fatal {
                                reason: "Hard recovery limit reached".into(),
                            };
                            actions.push(Action::SendGotifyAlert(
                            AlertPriority::Critical,
                            "💀 Windlass: hard recovery limit reached. Halting. Manual intervention required.".into(),
                        ));
                        } else {
                            warn!(
                                attempt = recoveries.0,
                                limit = HARD_RECOVERY_LIMIT.0,
                                "hard recovery: NAT frozen — restarting stack"
                            );
                            self.vpn = VpnState::DumpingLogs;
                            actions.push(Action::FetchAndDumpAllLogs);
                            actions.push(Action::SendGotifyAlert(
                                AlertPriority::Critical,
                                format!(
                                    "⚠️ NAT Frozen. Initiating Hard Recovery ({}/{}).",
                                    recoveries.0, HARD_RECOVERY_LIMIT.0,
                                ),
                            ));
                        }
                    }
                }
            }

            // ── Monitoring ────────────────────────────────────────────────────
            Event::DiskSpaceObserved(space) => {
                use uom::si::information::gigabyte;
                let gib = space.get::<gigabyte>();
                if gib < 50.0 {
                    warn!(space_gib = format_args!("{gib:.1}"), "disk space low");
                    actions.push(Action::SendGotifyAlert(
                        AlertPriority::Warning,
                        format!("💾 Low disk space: {gib:.1} GB remaining on /mnt/Data."),
                    ));
                } else {
                    debug!(space_gib = format_args!("{gib:.1}"), "disk space OK");
                }
                actions.push(Action::ScheduleWakeup(
                    WakeupId::DiskCheck,
                    DISK_CHECK_INTERVAL.into(),
                ));
            }

            // Shell sends the raw full list; Core diffs against known_torrents and
            // only alerts on names that haven't been seen before.
            Event::NewTorrentsObserved(current) => {
                let new_names: Vec<_> = current
                    .iter()
                    .filter(|name| !self.known_torrents.contains(*name))
                    .cloned()
                    .collect();
                self.known_torrents.extend(current);
                if new_names.is_empty() {
                    debug!("torrent check: no new torrents");
                } else {
                    info!(names = ?new_names, "new torrent(s) detected");
                    let list = new_names
                        .iter()
                        .map(|n| n.0.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    actions.push(Action::SendGotifyAlert(
                        AlertPriority::Info,
                        format!("🧲 New torrent(s) added: {list}"),
                    ));
                }
                actions.push(Action::ScheduleWakeup(
                    WakeupId::TorrentCheck,
                    TORRENT_CHECK_INTERVAL.into(),
                ));
            }

            // ── Wakeup dispatch ───────────────────────────────────────────────
            Event::Wakeup(id) => match id {
                WakeupId::Heartbeat => actions.push(Action::CheckMamConnectability),
                WakeupId::DiskCheck => actions.push(Action::CheckDiskSpace),
                WakeupId::TorrentCheck => {
                    if let QbitState::Ready { cookie, .. } = &self.qbit {
                        actions.push(Action::CheckNewTorrents(cookie.clone()));
                    }
                }
                WakeupId::QbitAuthRetry => {
                    if matches!(self.qbit, QbitState::Authenticating { .. }) {
                        actions.push(Action::AuthenticateQbit);
                    } else {
                        debug!(qbit = %self.qbit, "QbitAuthRetry wakeup: no longer authenticating — ignoring");
                    }
                }
                WakeupId::QbitSyncRetry => {
                    if let QbitState::SyncingPort { cookie, target, .. } = &self.qbit {
                        actions.push(Action::SyncQbitPort(cookie.clone(), *target));
                    }
                }
                WakeupId::RetryPortRead => actions.push(Action::ReadPortFiles),
            },

            Event::MamRateLimitViolation => {
                // Handled by the shell event loop before reaching the core.
                warn!(
                    "MamRateLimitViolation reached core — this should be intercepted by the shell"
                );
            }
        }

        actions
    }
}
