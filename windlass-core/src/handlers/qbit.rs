use crate::actions::Action;
use crate::types::{MamState, QbitState, SystemState, VpnState};
use tracing::{debug, error, info, warn};
use windlass_types::{AlertPriority, AuthCookie, HttpStatusCode, RetryCount, WakeupId};

use super::{
    QBIT_AUTH_BACKOFF_BASE, QBIT_CONNECTION_RETRY_DELAY, QBIT_SYNC_BACKOFF, QBIT_SYNC_RETRY_LIMIT,
};

impl SystemState {
    pub(crate) fn on_qbit_auth_success(&mut self, cookie: AuthCookie) -> Vec<Action> {
        info!("qBittorrent authenticated");
        if let VpnState::Connected { port, .. } = &self.vpn {
            let target = *port;
            self.qbit = QbitState::SyncingPort {
                attempt: RetryCount(0),
                cookie: cookie.clone(),
                target,
            };
            self.mark_changed();
            vec![Action::SyncQbitPort(cookie, target)]
        } else {
            self.qbit = QbitState::Authenticated { cookie };
            self.mark_changed();
            vec![]
        }
    }

    // Container not yet reachable — normal during startup. Silent fixed-delay retry.
    pub(crate) fn on_qbit_connection_refused(&self) -> Vec<Action> {
        if matches!(self.qbit, QbitState::Authenticating { .. }) {
            debug!(
                delay_secs = QBIT_CONNECTION_RETRY_DELAY.0.as_secs(),
                "qBittorrent not yet reachable — retrying"
            );
            vec![Action::ScheduleWakeup(
                WakeupId::QbitAuthRetry,
                QBIT_CONNECTION_RETRY_DELAY.into(),
            )]
        } else {
            debug!(qbit = %self.qbit, "stale QbitConnectionRefused — ignoring");
            vec![]
        }
    }

    // Credentials rejected — this is a configuration error, not a transient failure.
    pub(crate) fn on_qbit_auth_failed(&mut self) -> Vec<Action> {
        error!("qBittorrent rejected credentials — check QBITTORRENT_USER / QBITTORRENT_PASS");
        self.qbit = QbitState::Authenticating {
            attempt: RetryCount(0),
        };
        self.mark_changed();
        vec![
            Action::SendAlert {
                priority: AlertPriority::Critical,
                title: "qBit auth failed".into(),
                body: "🔐 qBittorrent rejected credentials. Check QBITTORRENT_USER / QBITTORRENT_PASS.".into(),
            },
            Action::ScheduleWakeup(WakeupId::QbitAuthRetry, QBIT_AUTH_BACKOFF_BASE.into()),
        ]
    }

    pub(crate) fn on_qbit_api_error(&mut self, _code: HttpStatusCode) -> Vec<Action> {
        let attempt = match &self.qbit {
            QbitState::Authenticating { attempt } => *attempt,
            _ => RetryCount(0),
        };
        let backoff = QBIT_AUTH_BACKOFF_BASE.exponential(attempt);
        self.qbit = QbitState::Authenticating {
            attempt: attempt.increment(),
        };
        self.mark_changed();
        vec![Action::ScheduleWakeup(WakeupId::QbitAuthRetry, backoff)]
    }

    pub(crate) fn on_qbit_port_sync_success(&mut self) -> Vec<Action> {
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
                self.mark_changed();
                return vec![Action::UpdateMam(ip)];
            }
        }
        vec![]
    }

    pub(crate) fn on_qbit_port_sync_failed(&mut self, err_code: HttpStatusCode) -> Vec<Action> {
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
                self.mark_changed();
                return vec![Action::ScheduleWakeup(
                    WakeupId::QbitSyncRetry,
                    QBIT_SYNC_BACKOFF.into(),
                )];
            }
            warn!(
                port = target.into_inner(),
                err_code = err_code.0,
                retries = attempt.0,
                "qBittorrent port sync failed at retry limit — re-authenticating"
            );
            self.qbit = QbitState::Authenticating {
                attempt: RetryCount(0),
            };
            self.mark_changed();
            return vec![
                Action::SendAlert {
                    priority: AlertPriority::Warning,
                    title: "qBit port sync failed".into(),
                    body: format!(
                        "⚠️ qBittorrent rejecting port updates (HTTP {}) after {} attempts. Forcing re-auth.",
                        err_code.0, QBIT_SYNC_RETRY_LIMIT.0,
                    ),
                },
                Action::AuthenticateQbit,
            ];
        }
        vec![]
    }
}
