#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windlass_db_core::{
    ActivityRecord, ActivitySource, AlertRecord, DbCommand, DownloadStateChange, DownloadStatus,
};
use windlass_disk_core::DiskPublish;
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome, Timed};
use windlass_mam_core::{MamCommand, MamPublish};
use windlass_qbit_core::{QbitCommand, QbitPublish};
use windlass_types::AlertPriority;
use windlass_types::VpnPort;
use windlass_vpn_core::{VpnCommand, VpnPublish};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindlassConfig {
    pub snapshot_interval: Duration,
}

// `WindlassEvent` cannot derive `Eq` because `MamPublish::UploadHealthDegraded`
// carries `f64` fields, which only implement `PartialEq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WindlassEvent {
    Init,
    Vpn(VpnPublish),
    Qbit(QbitPublish),
    Mam(MamPublish),
    Disk(DiskPublish),
    DbFailed { operation: String, message: String },
    TimerFired(WindlassTimer),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindlassTimer {
    Snapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindlassAction {
    Vpn(VpnCommand),
    Qbit(QbitCommand),
    Mam(MamCommand),
    Db(DbCommand),
    SaveSystemSnapshot(SystemStateView),
    SendAlert {
        priority: AlertPriority,
        title: String,
        body: String,
    },
    ScheduleTimer {
        timer: WindlassTimer,
        after: Duration,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindlassPublish {
    SystemState(SystemStateView),
    Activity { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindlassTopic {
    SystemState,
    Activity,
}

impl HasTopic<WindlassTopic> for WindlassPublish {
    fn topic(&self) -> WindlassTopic {
        match self {
            Self::SystemState(_) => WindlassTopic::SystemState,
            Self::Activity { .. } => WindlassTopic::Activity,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindlassCommand {
    Refresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindlassResponse {
    Accepted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceStatus {
    Unknown,
    Ready,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemStateView {
    pub vpn: ServiceStatus,
    pub qbit: ServiceStatus,
    pub mam: ServiceStatus,
    pub forwarded_port: Option<VpnPort>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindlassMachine {
    config: WindlassConfig,
    state: SystemStateView,
}

impl WindlassMachine {
    #[must_use]
    pub const fn state(&self) -> &SystemStateView {
        &self.state
    }

    fn snapshot_action(&self) -> WindlassAction {
        WindlassAction::SaveSystemSnapshot(self.state.clone())
    }
}

impl Machine for WindlassMachine {
    type Config = WindlassConfig;
    type Event = WindlassEvent;
    type Action = WindlassAction;
    type Publish = WindlassPublish;
    type Topic = WindlassTopic;
    type Command = WindlassCommand;
    type Response = WindlassResponse;

    fn new(config: Self::Config, _now: Instant) -> Self {
        Self {
            config,
            state: SystemStateView {
                vpn: ServiceStatus::Unknown,
                qbit: ServiceStatus::Unknown,
                mam: ServiceStatus::Unknown,
                forwarded_port: None,
            },
        }
    }

    // Each event arm is a small, self-contained routing decision; the function is
    // long because the cross-system event set is large, not because any arm is complex.
    #[allow(clippy::too_many_lines)]
    fn handle(
        &mut self,
        _now: Instant,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event.inner {
            WindlassEvent::Init => Outcome {
                actions: vec![
                    WindlassAction::Vpn(VpnCommand::StartMonitoring),
                    WindlassAction::Qbit(QbitCommand::EnsureAuthenticated),
                    WindlassAction::Mam(MamCommand::EnsureAuthenticated),
                    WindlassAction::ScheduleTimer {
                        timer: WindlassTimer::Snapshot,
                        after: self.config.snapshot_interval,
                    },
                ],
                publish: Vec::new(),
            },
            WindlassEvent::Vpn(VpnPublish::Connected) => {
                self.state.vpn = ServiceStatus::Ready;
                self.publish_state()
            }
            WindlassEvent::Vpn(VpnPublish::Disconnected) => {
                self.state.vpn = ServiceStatus::Degraded;
                self.state.forwarded_port = None;
                self.publish_state()
            }
            WindlassEvent::Vpn(VpnPublish::PortReady { port }) => {
                self.state.forwarded_port = Some(port);
                Outcome {
                    actions: vec![
                        WindlassAction::Qbit(QbitCommand::EnsureListenPort { port }),
                        WindlassAction::Mam(MamCommand::EnsureSeedboxPort { port }),
                        self.snapshot_action(),
                    ],
                    publish: vec![WindlassPublish::SystemState(self.state.clone())],
                }
            }
            WindlassEvent::Vpn(VpnPublish::PortUnavailable) => {
                self.state.forwarded_port = None;
                self.publish_state()
            }
            WindlassEvent::Qbit(QbitPublish::Ready) => {
                self.state.qbit = ServiceStatus::Ready;
                self.publish_state()
            }
            WindlassEvent::Qbit(QbitPublish::Unavailable { reason }) => {
                self.state.qbit = ServiceStatus::Degraded;
                self.publish_state_with_activity(reason)
            }
            WindlassEvent::Qbit(
                QbitPublish::ListenPortReady { .. } | QbitPublish::TorrentsUpdated { .. },
            )
            | WindlassEvent::Mam(MamPublish::Connectable { .. })
            | WindlassEvent::Disk(DiskPublish::AboveFloor { .. }) => Outcome::none(),
            // DOM-11: QueueOrchestrated → one RecordActivity + one Activity publish.
            WindlassEvent::Qbit(QbitPublish::QueueOrchestrated {
                ref paused,
                ref force_resumed,
            }) => Self::on_queue_orchestrated(paused, force_resumed),
            // DOM-12: UnsatisfiedQuotaCritical → one RecordAlert(Critical) + one Activity publish.
            WindlassEvent::Qbit(QbitPublish::UnsatisfiedQuotaCritical { unsatisfied, limit }) => {
                Self::on_unsatisfied_quota_critical(unsatisfied, limit)
            }
            // DOM-12: UnsatisfiedQuotaApproaching → one RecordAlert(Warning) + one Activity publish.
            WindlassEvent::Qbit(QbitPublish::UnsatisfiedQuotaApproaching {
                unsatisfied,
                limit,
            }) => Self::on_unsatisfied_quota_approaching(unsatisfied, limit),
            WindlassEvent::Qbit(QbitPublish::DeadTorrentRemoved { mam_id, .. }) => {
                Self::on_dead_torrent_removed(mam_id)
            }
            // DOM-10: BannedPrivacySettingsObserved → one Critical RecordAlert + one Activity.
            WindlassEvent::Qbit(QbitPublish::BannedPrivacySettingsObserved { dht, pex, lsd }) => {
                Self::on_banned_privacy_settings_observed(dht, pex, lsd)
            }
            WindlassEvent::Mam(MamPublish::SeedboxPortReady { port }) => Outcome {
                actions: vec![WindlassAction::SendAlert {
                    priority: AlertPriority::Info,
                    title: "MAM seedbox updated".to_string(),
                    body: format!("MAM seedbox registered with port {}.", port.into_inner()),
                }],
                publish: Vec::new(),
            },
            WindlassEvent::Mam(MamPublish::Ready) => {
                self.state.mam = ServiceStatus::Ready;
                self.publish_state()
            }
            // §28: Unavailable keeps the broad "MAM degraded" semantics —
            // Activity log + ServiceStatus::Degraded, no alert.
            WindlassEvent::Mam(MamPublish::Unavailable { reason }) => {
                self.state.mam = ServiceStatus::Degraded;
                self.publish_state_with_activity(reason)
            }
            // DOM-15 (§28): NotConnectable is now a real connectivity problem
            // — MAM responded and told us our client is unreachable from
            // their side.  Emit a Warning alert in addition to the
            // Activity/Degraded path.
            WindlassEvent::Mam(MamPublish::NotConnectable { reason }) => {
                self.state.mam = ServiceStatus::Degraded;
                self.on_mam_not_connectable(&reason)
            }
            // DOM-16 (§28): Unreachable is a transient transport failure
            // (DNS, TCP, TLS, timeout).  Activity + ServiceStatus::Degraded
            // only — no alert here.  Persistent unreachability already
            // surfaces via the §27 KeepAliveDegraded path.
            WindlassEvent::Mam(MamPublish::Unreachable { reason }) => {
                self.state.mam = ServiceStatus::Degraded;
                self.publish_state_with_activity(format!("MAM unreachable: {reason}"))
            }
            // DOM-13: UploadHealthDegraded → one RecordAlert(Warning) + one Activity publish.
            WindlassEvent::Mam(MamPublish::UploadHealthDegraded {
                ratio,
                upload_credit_bytes,
                ratio_ok,
                buffer_ok,
            }) => Self::on_upload_health_degraded(ratio, upload_credit_bytes, ratio_ok, buffer_ok),
            // DOM-14: KeepAliveDegraded → one RecordAlert(Warning) + one Activity publish.
            WindlassEvent::Mam(MamPublish::KeepAliveDegraded {
                consecutive_failures,
                last_reason,
            }) => Self::on_keep_alive_degraded(consecutive_failures, &last_reason),
            WindlassEvent::Mam(MamPublish::RateLimited { retry_after }) => {
                self.state.mam = ServiceStatus::Degraded;
                self.publish_state_with_activity(format!(
                    "MAM rate limited for {}s",
                    retry_after.as_secs()
                ))
            }
            // DOM-9: BelowFloor → one EvictOneForDiskPressure + Activity publish.
            //        AboveFloor → no action, no publish.
            WindlassEvent::Disk(DiskPublish::BelowFloor { free_bytes }) => Outcome {
                actions: vec![WindlassAction::Qbit(QbitCommand::EvictOneForDiskPressure)],
                publish: vec![WindlassPublish::Activity {
                    message: format!("Disk pressure: {free_bytes} bytes free — eviction triggered"),
                }],
            },
            WindlassEvent::DbFailed { operation, message } => Outcome {
                actions: Vec::new(),
                publish: vec![WindlassPublish::Activity {
                    message: format!("DB {operation} failed: {message}"),
                }],
            },
            WindlassEvent::TimerFired(WindlassTimer::Snapshot) => Outcome {
                actions: vec![
                    self.snapshot_action(),
                    WindlassAction::ScheduleTimer {
                        timer: WindlassTimer::Snapshot,
                        after: self.config.snapshot_interval,
                    },
                ],
                publish: Vec::new(),
            },
        }
    }

    fn handle_command(
        &mut self,
        _now: Instant,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        let actions = match cmd {
            WindlassCommand::Refresh => vec![
                WindlassAction::Vpn(VpnCommand::RefreshState),
                WindlassAction::Qbit(QbitCommand::RefreshTorrents),
                WindlassAction::Mam(MamCommand::RefreshStatus),
            ],
        };
        Self::outcome(actions, WindlassResponse::Accepted)
    }
}

impl WindlassMachine {
    /// Handles the `BannedPrivacySettingsObserved` qBit publish (DOM-10 / §23).
    ///
    /// Emits one `RecordAlert(Critical)` action and one `Activity` publish listing
    /// which of DHT, `PeX`, and LSD are enabled.  This is only called when at least
    /// one setting is `true` — the qBit core never publishes this event for all-false.
    fn on_banned_privacy_settings_observed(
        dht: bool,
        pex: bool,
        lsd: bool,
    ) -> Outcome<WindlassAction, WindlassPublish> {
        let mut enabled = Vec::new();
        if dht {
            enabled.push("DHT");
        }
        if pex {
            enabled.push("PeX");
        }
        if lsd {
            enabled.push("LSD");
        }
        let settings = enabled.join(", ");
        let body = format!(
            "The following banned privacy settings are enabled: {settings}. \
             They have been disabled automatically."
        );
        let message = format!("Banned qBit privacy settings auto-reverted: {settings}");
        Outcome {
            actions: vec![
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    at: chrono::Utc::now(),
                    priority: AlertPriority::Critical,
                    title: "Banned qBit privacy setting enabled".to_string(),
                    body,
                })),
                WindlassAction::Db(DbCommand::RecordActivity(ActivityRecord {
                    at: chrono::Utc::now(),
                    source: ActivitySource::Domain,
                    action: "privacy_auto_revert".to_string(),
                    book_id: None,
                    detail: Some(message.clone()),
                    metadata: serde_json::Value::Null,
                })),
            ],
            publish: vec![WindlassPublish::Activity { message }],
        }
    }

    /// Handles the `DeadTorrentRemoved` qBit publish.
    ///
    /// When `mam_id` is `Some`, emits a `MarkDownloadState(Blacklisted)` DB command
    /// and an `Activity` publish.  When `mam_id` is `None`, does nothing (no MAM
    /// record to update).
    fn on_dead_torrent_removed(
        mam_id: Option<windlass_types::MamTorrentId>,
    ) -> Outcome<WindlassAction, WindlassPublish> {
        mam_id.map_or_else(Outcome::none, |id| Outcome {
            actions: vec![WindlassAction::Db(DbCommand::MarkDownloadState(
                DownloadStateChange {
                    mam_id: id,
                    status: DownloadStatus::Blacklisted,
                },
            ))],
            publish: vec![WindlassPublish::Activity {
                message: format!(
                    "Dead torrent blacklisted in DB (MAM ID {})",
                    id.into_inner()
                ),
            }],
        })
    }

    /// Handles the `QueueOrchestrated` qBit publish (DOM-11 / §24).
    ///
    /// Emits one `RecordActivity` DB command and one `Activity` publish describing
    /// the queue-orchestration swap (which satisfied seeder was paused and which
    /// unsatisfied torrent was force-resumed).
    fn on_queue_orchestrated(
        paused: &windlass_types::TorrentHash,
        force_resumed: &windlass_types::TorrentHash,
    ) -> Outcome<WindlassAction, WindlassPublish> {
        let detail = format!("paused={paused:?} resumed={force_resumed:?}");
        let message = format!("Queue orchestrated: {detail}");
        Outcome {
            actions: vec![WindlassAction::Db(DbCommand::RecordActivity(
                ActivityRecord {
                    at: chrono::Utc::now(),
                    source: ActivitySource::Qbit,
                    action: "queue_orchestrated".to_string(),
                    book_id: None,
                    detail: Some(detail),
                    metadata: serde_json::Value::Null,
                },
            ))],
            publish: vec![WindlassPublish::Activity { message }],
        }
    }

    /// Handles the `UnsatisfiedQuotaCritical` qBit publish (DOM-12 / §25).
    ///
    /// The unsatisfied-torrent count has met or exceeded the configured class
    /// limit (MAM Rule 2.8).  Emits one `RecordAlert(Critical)` action and one
    /// `Activity` publish.
    fn on_unsatisfied_quota_critical(
        unsatisfied: u32,
        limit: u32,
    ) -> Outcome<WindlassAction, WindlassPublish> {
        let body = format!("{unsatisfied}/{limit} unsatisfied torrents — download disabled.");
        let message = format!("Quota limit reached: {body}");
        Outcome {
            actions: vec![
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    at: chrono::Utc::now(),
                    priority: AlertPriority::Critical,
                    title: "Quota limit reached".to_string(),
                    body,
                })),
                WindlassAction::Db(DbCommand::RecordActivity(ActivityRecord {
                    at: chrono::Utc::now(),
                    source: ActivitySource::Qbit,
                    action: "unsatisfied_quota_critical".to_string(),
                    book_id: None,
                    detail: Some(message.clone()),
                    metadata: serde_json::Value::Null,
                })),
            ],
            publish: vec![WindlassPublish::Activity { message }],
        }
    }

    /// Handles the `UnsatisfiedQuotaApproaching` qBit publish (DOM-12 / §25).
    ///
    /// The unsatisfied-torrent count is within 5 of the configured class limit
    /// (MAM Rule 2.8).  Emits one `RecordAlert(Warning)` action and one
    /// `Activity` publish.
    fn on_unsatisfied_quota_approaching(
        unsatisfied: u32,
        limit: u32,
    ) -> Outcome<WindlassAction, WindlassPublish> {
        let body = format!("{unsatisfied}/{limit} unsatisfied torrents.");
        let message = format!("Approaching quota limit: {body}");
        Outcome {
            actions: vec![
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    at: chrono::Utc::now(),
                    priority: AlertPriority::Warning,
                    title: "Approaching quota limit".to_string(),
                    body,
                })),
                WindlassAction::Db(DbCommand::RecordActivity(ActivityRecord {
                    at: chrono::Utc::now(),
                    source: ActivitySource::Qbit,
                    action: "unsatisfied_quota_approaching".to_string(),
                    book_id: None,
                    detail: Some(message.clone()),
                    metadata: serde_json::Value::Null,
                })),
            ],
            publish: vec![WindlassPublish::Activity { message }],
        }
    }

    /// Handles the `UploadHealthDegraded` MAM publish (DOM-13 / §26).
    ///
    /// The global ratio or upload-credit buffer is below the configured threshold.
    /// Emits one `RecordAlert(Warning)` action and one `Activity` publish.
    /// Priority is `Warning` (not `Critical`) because the gate is precautionary:
    /// no download is being blocked yet — the alert surfaces the degraded health
    /// so the operator can act before it becomes a compliance issue.
    fn on_upload_health_degraded(
        ratio: f64,
        upload_credit_bytes: u64,
        ratio_ok: bool,
        buffer_ok: bool,
    ) -> Outcome<WindlassAction, WindlassPublish> {
        let buffer_gib = upload_credit_bytes / (1024 * 1024 * 1024);
        let mut reasons = Vec::new();
        if !ratio_ok {
            reasons.push(format!("ratio {ratio:.2} is below minimum"));
        }
        if !buffer_ok {
            reasons.push(format!("upload buffer {buffer_gib} GiB is below minimum"));
        }
        let body = format!(
            "Upload health degraded — {}. Non-freeleech downloads will be blocked.",
            reasons.join("; ")
        );
        let message = format!("Upload health degraded: ratio={ratio:.2}, buffer={buffer_gib} GiB");
        Outcome {
            actions: vec![
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    at: chrono::Utc::now(),
                    priority: AlertPriority::Warning,
                    title: "Upload health degraded".to_string(),
                    body,
                })),
                WindlassAction::Db(DbCommand::RecordActivity(ActivityRecord {
                    at: chrono::Utc::now(),
                    source: ActivitySource::Domain,
                    action: "upload_health_degraded".to_string(),
                    book_id: None,
                    detail: Some(message.clone()),
                    metadata: serde_json::Value::Null,
                })),
            ],
            publish: vec![WindlassPublish::Activity { message }],
        }
    }

    /// Handles the `NotConnectable` MAM publish (DOM-15 / §28).
    ///
    /// MAM responded to a status fetch and told us our client is unreachable
    /// from their side — a real port/seedbox connectivity problem, not a
    /// transient network blip.  Emits one `RecordAlert(Warning)` and one
    /// Activity.  Distinct from `Unreachable` (transport failure, no alert)
    /// and `Unavailable` (broad degradation, no alert).
    fn on_mam_not_connectable(&self, reason: &str) -> Outcome<WindlassAction, WindlassPublish> {
        let body = format!("MAM reports the client as not connectable: {reason}");
        let message = format!("MAM not connectable: {reason}");
        Outcome {
            actions: vec![
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    at: chrono::Utc::now(),
                    priority: AlertPriority::Warning,
                    title: "MAM reports not connectable".to_string(),
                    body,
                })),
                WindlassAction::Db(DbCommand::RecordActivity(ActivityRecord {
                    at: chrono::Utc::now(),
                    source: ActivitySource::Mam,
                    action: "mam_not_connectable".to_string(),
                    book_id: None,
                    detail: Some(message.clone()),
                    metadata: serde_json::Value::Null,
                })),
                self.snapshot_action(),
            ],
            publish: vec![
                WindlassPublish::SystemState(self.state.clone()),
                WindlassPublish::Activity { message },
            ],
        }
    }

    /// Handles the `KeepAliveDegraded` MAM publish (DOM-14 / §27).
    ///
    /// The recurring MAM status fetch has failed at least
    /// `keep_alive_failure_threshold` times in a row.  Emits one
    /// `RecordAlert(Warning)` and one `Activity` publish.  Priority is
    /// `Warning` — repeated heartbeat failures are not yet a definite
    /// account-risk event, but the operator needs to investigate before
    /// MAM Rule 1.6 (inactivity) kicks in.
    fn on_keep_alive_degraded(
        consecutive_failures: u32,
        last_reason: &str,
    ) -> Outcome<WindlassAction, WindlassPublish> {
        let body = format!(
            "MAM keep-alive failing — {consecutive_failures} consecutive failures. \
             Last reason: {last_reason}",
        );
        let message = format!(
            "MAM heartbeat failing: {consecutive_failures} consecutive failures \
             (last: {last_reason})",
        );
        Outcome {
            actions: vec![
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    at: chrono::Utc::now(),
                    priority: AlertPriority::Warning,
                    title: "MAM heartbeat failing".to_string(),
                    body,
                })),
                WindlassAction::Db(DbCommand::RecordActivity(ActivityRecord {
                    at: chrono::Utc::now(),
                    source: ActivitySource::Domain,
                    action: "mam_keep_alive_degraded".to_string(),
                    book_id: None,
                    detail: Some(message.clone()),
                    metadata: serde_json::Value::Null,
                })),
            ],
            publish: vec![WindlassPublish::Activity { message }],
        }
    }

    fn publish_state(&self) -> Outcome<WindlassAction, WindlassPublish> {
        Outcome {
            actions: vec![self.snapshot_action()],
            publish: vec![WindlassPublish::SystemState(self.state.clone())],
        }
    }

    fn publish_state_with_activity(
        &self,
        message: String,
    ) -> Outcome<WindlassAction, WindlassPublish> {
        Outcome {
            actions: vec![self.snapshot_action()],
            publish: vec![
                WindlassPublish::SystemState(self.state.clone()),
                WindlassPublish::Activity { message },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use windlass_machine::{Machine, Outcome, Timed};
    use windlass_mam_core::MamCommand;
    use windlass_mam_core::MamPublish;
    use windlass_qbit_core::QbitCommand;
    use windlass_types::VpnPort;
    use windlass_vpn_core::VpnPublish;

    use crate::{
        ServiceStatus, WindlassAction, WindlassConfig, WindlassEvent, WindlassMachine,
        WindlassPublish,
    };

    fn machine() -> WindlassMachine {
        WindlassMachine::new(
            WindlassConfig {
                snapshot_interval: Duration::from_secs(60),
            },
            Instant::now(),
        )
    }

    fn handle(
        machine: &mut WindlassMachine,
        event: WindlassEvent,
    ) -> Outcome<WindlassAction, WindlassPublish> {
        machine.handle(Instant::now(), Timed::now(event))
    }

    #[test]
    fn vpn_port_ready_converges_qbit_and_mam() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = handle(
            &mut machine,
            WindlassEvent::Vpn(VpnPublish::PortReady { port }),
        );

        assert_eq!(machine.state().forwarded_port, Some(port));
        assert!(
            out.actions
                .contains(&WindlassAction::Qbit(QbitCommand::EnsureListenPort {
                    port
                }))
        );
        assert!(
            out.actions
                .contains(&WindlassAction::Mam(MamCommand::EnsureSeedboxPort { port }))
        );
        assert!(matches!(
            out.publish.as_slice(),
            [WindlassPublish::SystemState(_)]
        ));
    }

    #[test]
    fn vpn_disconnected_degrades_vpn_and_clears_port() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();
        handle(
            &mut machine,
            WindlassEvent::Vpn(VpnPublish::PortReady { port }),
        );

        let out = handle(&mut machine, WindlassEvent::Vpn(VpnPublish::Disconnected));

        assert_eq!(machine.state().vpn, ServiceStatus::Degraded);
        assert_eq!(machine.state().forwarded_port, None);
        assert!(matches!(
            out.publish.as_slice(),
            [WindlassPublish::SystemState(_)]
        ));
    }

    #[test]
    fn mam_seedbox_port_ready_records_boot_alert() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = handle(
            &mut machine,
            WindlassEvent::Mam(MamPublish::SeedboxPortReady { port }),
        );

        assert!(matches!(
            out.actions.as_slice(),
            [WindlassAction::SendAlert { title, .. }] if title == "MAM seedbox updated"
        ));
        assert!(out.publish.is_empty());
    }

    // ── Dead-torrent blacklist tests (DOM-8 / story 20) ───────────────────────

    #[test]
    fn dead_torrent_removed_with_mam_id_emits_blacklist_command() {
        use windlass_db_core::{DbCommand, DownloadStateChange, DownloadStatus};
        use windlass_qbit_core::QbitPublish;
        use windlass_types::{MamTorrentId, TorrentHash};

        let mut machine = machine();
        let mam_id = MamTorrentId::try_new(12_345).unwrap();
        let hash = TorrentHash("a".repeat(40));

        let out = handle(
            &mut machine,
            WindlassEvent::Qbit(QbitPublish::DeadTorrentRemoved {
                hash,
                mam_id: Some(mam_id),
            }),
        );

        let expected_cmd = WindlassAction::Db(DbCommand::MarkDownloadState(DownloadStateChange {
            mam_id,
            status: DownloadStatus::Blacklisted,
        }));
        assert!(
            out.actions.contains(&expected_cmd),
            "MarkDownloadState(Blacklisted) must be emitted for a DeadTorrentRemoved with mam_id"
        );
        assert_eq!(out.actions.len(), 1, "exactly one action expected");
        assert_eq!(
            out.publish.len(),
            1,
            "exactly one publish expected (Activity)"
        );
        assert!(
            matches!(&out.publish[0], WindlassPublish::Activity { .. }),
            "publish must be an Activity"
        );
    }

    #[test]
    fn dead_torrent_removed_without_mam_id_emits_nothing() {
        use windlass_qbit_core::QbitPublish;
        use windlass_types::TorrentHash;

        let mut machine = machine();
        let hash = TorrentHash("b".repeat(40));

        let out = handle(
            &mut machine,
            WindlassEvent::Qbit(QbitPublish::DeadTorrentRemoved { hash, mam_id: None }),
        );

        assert!(
            out.actions.is_empty(),
            "no action must be emitted when mam_id is None"
        );
        assert!(
            out.publish.is_empty(),
            "no publish must be emitted when mam_id is None"
        );
    }

    // ── Disk-pressure routing unit tests (DOM-9 / story 22) ──────────────────

    #[test]
    fn disk_below_floor_emits_evict_command_and_activity() {
        use windlass_disk_core::DiskPublish;
        use windlass_qbit_core::QbitCommand;

        let mut machine = machine();

        let out = handle(
            &mut machine,
            WindlassEvent::Disk(DiskPublish::BelowFloor {
                free_bytes: 500_000,
            }),
        );

        assert_eq!(
            out.actions,
            vec![WindlassAction::Qbit(QbitCommand::EvictOneForDiskPressure)],
            "BelowFloor must emit exactly one EvictOneForDiskPressure command"
        );
        assert_eq!(out.actions.len(), 1, "exactly one action expected");
        assert_eq!(
            out.publish.len(),
            1,
            "exactly one publish expected (Activity)"
        );
        assert!(
            matches!(&out.publish[0], WindlassPublish::Activity { .. }),
            "publish must be an Activity"
        );
    }

    #[test]
    fn disk_above_floor_emits_nothing() {
        use windlass_disk_core::DiskPublish;

        let mut machine = machine();

        let out = handle(
            &mut machine,
            WindlassEvent::Disk(DiskPublish::AboveFloor {
                free_bytes: 2_000_000,
            }),
        );

        assert!(out.actions.is_empty(), "AboveFloor must emit no actions");
        assert!(out.publish.is_empty(), "AboveFloor must emit no publishes");
    }

    // ── Privacy auto-revert routing unit tests (DOM-10 / story 23) ───────────

    #[test]
    fn banned_privacy_settings_observed_dht_only_emits_critical_alert_and_activity() {
        use windlass_db_core::{AlertRecord, DbCommand};
        use windlass_qbit_core::QbitPublish;
        use windlass_types::AlertPriority;

        let mut machine = machine();

        let out = handle(
            &mut machine,
            WindlassEvent::Qbit(QbitPublish::BannedPrivacySettingsObserved {
                dht: true,
                pex: false,
                lsd: false,
            }),
        );

        // Must have exactly 2 actions: RecordAlert(Critical) + RecordActivity
        assert_eq!(out.actions.len(), 2, "exactly two actions expected");
        let has_critical_alert = out.actions.iter().any(|a| {
            matches!(
                a,
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    priority: AlertPriority::Critical,
                    ..
                }))
            )
        });
        assert!(
            has_critical_alert,
            "must emit exactly one RecordAlert(Critical)"
        );
        assert!(
            matches!(
                &out.actions[1],
                WindlassAction::Db(DbCommand::RecordActivity(_))
            ),
            "second action must be RecordActivity"
        );

        // Exactly one Activity publish
        assert_eq!(out.publish.len(), 1, "exactly one publish expected");
        assert!(
            matches!(&out.publish[0], WindlassPublish::Activity { .. }),
            "publish must be an Activity"
        );
    }

    #[test]
    fn banned_privacy_settings_all_true_emits_critical_alert_mentioning_all() {
        use windlass_db_core::{AlertRecord, DbCommand};
        use windlass_qbit_core::QbitPublish;
        use windlass_types::AlertPriority;

        let mut machine = machine();

        let out = handle(
            &mut machine,
            WindlassEvent::Qbit(QbitPublish::BannedPrivacySettingsObserved {
                dht: true,
                pex: true,
                lsd: true,
            }),
        );

        assert_eq!(out.actions.len(), 2);
        let alert_action = &out.actions[0];
        if let WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
            priority: AlertPriority::Critical,
            body,
            ..
        })) = alert_action
        {
            assert!(body.contains("DHT"), "body should mention DHT");
            assert!(body.contains("PeX"), "body should mention PeX");
            assert!(body.contains("LSD"), "body should mention LSD");
        } else {
            panic!("expected RecordAlert(Critical)");
        }
    }

    // ── Queue-orchestration routing unit tests (DOM-11 / story 24) ───────────

    #[test]
    fn queue_orchestrated_emits_record_activity_and_activity_publish() {
        use windlass_db_core::{ActivityRecord, ActivitySource, DbCommand};
        use windlass_qbit_core::QbitPublish;
        use windlass_types::TorrentHash;

        let mut machine = machine();
        let paused = TorrentHash("a".repeat(40));
        let force_resumed = TorrentHash("b".repeat(40));

        let out = handle(
            &mut machine,
            WindlassEvent::Qbit(QbitPublish::QueueOrchestrated {
                paused: paused.clone(),
                force_resumed: force_resumed.clone(),
            }),
        );

        // Exactly one action: RecordActivity
        assert_eq!(
            out.actions.len(),
            1,
            "exactly one action expected (RecordActivity)"
        );
        assert!(
            matches!(
                &out.actions[0],
                WindlassAction::Db(DbCommand::RecordActivity(ActivityRecord {
                    source: ActivitySource::Qbit,
                    action,
                    ..
                })) if action == "queue_orchestrated"
            ),
            "action must be Db(RecordActivity {{ source: Qbit, action: queue_orchestrated }})"
        );

        // Exactly one Activity publish
        assert_eq!(out.publish.len(), 1, "exactly one publish expected");
        assert!(
            matches!(&out.publish[0], WindlassPublish::Activity { .. }),
            "publish must be an Activity"
        );
    }

    // ── Quota alert routing unit tests (DOM-12 / story 25) ───────────────────

    #[test]
    fn quota_critical_emits_critical_alert_and_activity() {
        use windlass_db_core::{AlertRecord, DbCommand};
        use windlass_qbit_core::QbitPublish;
        use windlass_types::AlertPriority;

        let mut machine = machine();
        let out = handle(
            &mut machine,
            WindlassEvent::Qbit(QbitPublish::UnsatisfiedQuotaCritical {
                unsatisfied: 10,
                limit: 10,
            }),
        );

        // Exactly 2 actions: RecordAlert(Critical) + RecordActivity
        assert_eq!(out.actions.len(), 2, "exactly two actions expected");
        let has_critical_alert = out.actions.iter().any(|a| {
            matches!(
                a,
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    priority: AlertPriority::Critical,
                    ..
                }))
            )
        });
        assert!(
            has_critical_alert,
            "must emit exactly one RecordAlert(Critical)"
        );
        assert!(
            matches!(
                &out.actions[1],
                WindlassAction::Db(DbCommand::RecordActivity(_))
            ),
            "second action must be RecordActivity"
        );
        // Exactly one Activity publish
        assert_eq!(out.publish.len(), 1, "exactly one publish expected");
        assert!(
            matches!(&out.publish[0], WindlassPublish::Activity { .. }),
            "publish must be an Activity"
        );
    }

    #[test]
    fn quota_critical_alert_body_mentions_counts() {
        use windlass_db_core::{AlertRecord, DbCommand};
        use windlass_qbit_core::QbitPublish;
        use windlass_types::AlertPriority;

        let mut machine = machine();
        let out = handle(
            &mut machine,
            WindlassEvent::Qbit(QbitPublish::UnsatisfiedQuotaCritical {
                unsatisfied: 100,
                limit: 100,
            }),
        );

        if let WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
            priority: AlertPriority::Critical,
            body,
            title,
            ..
        })) = &out.actions[0]
        {
            assert!(body.contains("100/100"), "body should contain counts");
            assert_eq!(title, "Quota limit reached");
        } else {
            panic!("expected RecordAlert(Critical)");
        }
    }

    #[test]
    fn quota_approaching_emits_warning_alert_and_activity() {
        use windlass_db_core::{AlertRecord, DbCommand};
        use windlass_qbit_core::QbitPublish;
        use windlass_types::AlertPriority;

        let mut machine = machine();
        let out = handle(
            &mut machine,
            WindlassEvent::Qbit(QbitPublish::UnsatisfiedQuotaApproaching {
                unsatisfied: 8,
                limit: 10,
            }),
        );

        // Exactly 2 actions: RecordAlert(Warning) + RecordActivity
        assert_eq!(out.actions.len(), 2, "exactly two actions expected");
        let has_warning_alert = out.actions.iter().any(|a| {
            matches!(
                a,
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    priority: AlertPriority::Warning,
                    ..
                }))
            )
        });
        assert!(
            has_warning_alert,
            "must emit exactly one RecordAlert(Warning)"
        );
        assert!(
            matches!(
                &out.actions[1],
                WindlassAction::Db(DbCommand::RecordActivity(_))
            ),
            "second action must be RecordActivity"
        );
        // Exactly one Activity publish
        assert_eq!(out.publish.len(), 1, "exactly one publish expected");
        assert!(
            matches!(&out.publish[0], WindlassPublish::Activity { .. }),
            "publish must be an Activity"
        );
    }

    #[test]
    fn quota_approaching_alert_body_mentions_counts() {
        use windlass_db_core::{AlertRecord, DbCommand};
        use windlass_qbit_core::QbitPublish;
        use windlass_types::AlertPriority;

        let mut machine = machine();
        let out = handle(
            &mut machine,
            WindlassEvent::Qbit(QbitPublish::UnsatisfiedQuotaApproaching {
                unsatisfied: 95,
                limit: 100,
            }),
        );

        if let WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
            priority: AlertPriority::Warning,
            body,
            title,
            ..
        })) = &out.actions[0]
        {
            assert!(body.contains("95/100"), "body should contain counts");
            assert_eq!(title, "Approaching quota limit");
        } else {
            panic!("expected RecordAlert(Warning)");
        }
    }

    // ── Upload-health alert routing unit tests (DOM-13 / story 26) ───────────

    #[test]
    fn upload_health_degraded_emits_one_warning_record_alert_and_one_activity() {
        use windlass_db_core::{AlertRecord, DbCommand};
        use windlass_mam_core::MamPublish;
        use windlass_types::AlertPriority;

        let mut machine = machine();

        let out = handle(
            &mut machine,
            WindlassEvent::Mam(MamPublish::UploadHealthDegraded {
                ratio: 1.5,
                upload_credit_bytes: 0,
                ratio_ok: false,
                buffer_ok: false,
            }),
        );

        // Exactly 2 actions: RecordAlert(Warning) + RecordActivity
        assert_eq!(out.actions.len(), 2, "exactly two actions expected");
        let has_warning_alert = out.actions.iter().any(|a| {
            matches!(
                a,
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    priority: AlertPriority::Warning,
                    ..
                }))
            )
        });
        assert!(
            has_warning_alert,
            "must emit exactly one RecordAlert(Warning)"
        );
        assert!(
            matches!(
                &out.actions[1],
                WindlassAction::Db(DbCommand::RecordActivity(_))
            ),
            "second action must be RecordActivity"
        );
        // Exactly one Activity publish
        assert_eq!(out.publish.len(), 1, "exactly one publish expected");
        assert!(
            matches!(&out.publish[0], WindlassPublish::Activity { .. }),
            "publish must be an Activity"
        );
    }

    #[test]
    fn upload_health_degraded_alert_title_is_upload_health_degraded() {
        use windlass_db_core::{AlertRecord, DbCommand};
        use windlass_mam_core::MamPublish;
        use windlass_types::AlertPriority;

        let mut machine = machine();

        let out = handle(
            &mut machine,
            WindlassEvent::Mam(MamPublish::UploadHealthDegraded {
                ratio: 1.5,
                upload_credit_bytes: 10 * 1024 * 1024 * 1024,
                ratio_ok: false,
                buffer_ok: true,
            }),
        );

        if let WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
            priority: AlertPriority::Warning,
            title,
            ..
        })) = &out.actions[0]
        {
            assert_eq!(title, "Upload health degraded");
        } else {
            panic!("expected RecordAlert(Warning)");
        }
    }

    // ── DOM-14: KeepAliveDegraded routing (§27) ───────────────────────────────

    #[test]
    fn keep_alive_degraded_emits_one_warning_record_alert_and_one_activity() {
        use windlass_db_core::{AlertRecord, DbCommand};
        use windlass_mam_core::MamPublish;
        use windlass_types::AlertPriority;

        let mut machine = machine();

        let out = handle(
            &mut machine,
            WindlassEvent::Mam(MamPublish::KeepAliveDegraded {
                consecutive_failures: 3,
                last_reason: "timeout".to_string(),
            }),
        );

        assert_eq!(out.actions.len(), 2, "exactly two actions expected");
        let has_warning_alert = out.actions.iter().any(|a| {
            matches!(
                a,
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    priority: AlertPriority::Warning,
                    ..
                }))
            )
        });
        assert!(
            has_warning_alert,
            "must emit exactly one RecordAlert(Warning)"
        );
        assert!(
            matches!(
                &out.actions[1],
                WindlassAction::Db(DbCommand::RecordActivity(_))
            ),
            "second action must be RecordActivity"
        );
        assert_eq!(out.publish.len(), 1, "exactly one publish expected");
        assert!(
            matches!(&out.publish[0], WindlassPublish::Activity { .. }),
            "publish must be an Activity"
        );
    }

    #[test]
    fn keep_alive_degraded_alert_title_and_body_include_failure_context() {
        use windlass_db_core::{AlertRecord, DbCommand};
        use windlass_mam_core::MamPublish;
        use windlass_types::AlertPriority;

        let mut machine = machine();

        let out = handle(
            &mut machine,
            WindlassEvent::Mam(MamPublish::KeepAliveDegraded {
                consecutive_failures: 5,
                last_reason: "dns failure".to_string(),
            }),
        );

        if let WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
            priority: AlertPriority::Warning,
            title,
            body,
            ..
        })) = &out.actions[0]
        {
            assert_eq!(title, "MAM heartbeat failing");
            assert!(body.contains("5"), "body should mention the failure count");
            assert!(
                body.contains("dns failure"),
                "body should include the last failure reason"
            );
        } else {
            panic!("expected RecordAlert(Warning)");
        }
    }

    // ── DOM-15 / DOM-16: NotConnectable vs Unreachable routing (§28) ──────────

    #[test]
    fn not_connectable_emits_one_warning_alert_and_one_activity() {
        use windlass_db_core::{AlertRecord, DbCommand};
        use windlass_mam_core::MamPublish;
        use windlass_types::AlertPriority;

        let mut machine = machine();

        let out = handle(
            &mut machine,
            WindlassEvent::Mam(MamPublish::NotConnectable {
                reason: "port closed".to_string(),
            }),
        );

        let warning_alert_count = out
            .actions
            .iter()
            .filter(|a| {
                matches!(
                    a,
                    WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                        priority: AlertPriority::Warning,
                        ..
                    }))
                )
            })
            .count();
        assert_eq!(
            warning_alert_count, 1,
            "NotConnectable must emit exactly one RecordAlert(Warning)"
        );

        let activity_db_count = out
            .actions
            .iter()
            .filter(|a| matches!(a, WindlassAction::Db(DbCommand::RecordActivity(_))))
            .count();
        assert_eq!(activity_db_count, 1, "must emit one RecordActivity");

        let activity_publish_count = out
            .publish
            .iter()
            .filter(|p| matches!(p, WindlassPublish::Activity { .. }))
            .count();
        assert_eq!(activity_publish_count, 1, "must publish one Activity");
    }

    #[test]
    fn not_connectable_alert_title_and_body_include_reason() {
        use windlass_db_core::DbCommand;
        use windlass_mam_core::MamPublish;
        use windlass_types::AlertPriority;

        let mut machine = machine();

        let out = handle(
            &mut machine,
            WindlassEvent::Mam(MamPublish::NotConnectable {
                reason: "blocked".to_string(),
            }),
        );

        let alert = out.actions.iter().find_map(|a| {
            if let WindlassAction::Db(DbCommand::RecordAlert(record)) = a {
                Some(record)
            } else {
                None
            }
        });
        let alert = alert.expect("must emit a RecordAlert");
        assert_eq!(alert.priority, AlertPriority::Warning);
        assert_eq!(alert.title, "MAM reports not connectable");
        assert!(
            alert.body.contains("blocked"),
            "body should include the reason"
        );
    }

    #[test]
    fn unreachable_emits_activity_only_no_alert() {
        use windlass_db_core::DbCommand;
        use windlass_mam_core::MamPublish;

        let mut machine = machine();

        let out = handle(
            &mut machine,
            WindlassEvent::Mam(MamPublish::Unreachable {
                reason: "dns failed".to_string(),
            }),
        );

        let alert_count = out
            .actions
            .iter()
            .filter(|a| matches!(a, WindlassAction::Db(DbCommand::RecordAlert(_))))
            .count();
        assert_eq!(
            alert_count, 0,
            "Unreachable must NOT emit any RecordAlert (transient signal)"
        );

        let activity_count = out
            .publish
            .iter()
            .filter(|p| matches!(p, WindlassPublish::Activity { .. }))
            .count();
        assert_eq!(
            activity_count, 1,
            "Unreachable must emit exactly one Activity publish"
        );
    }
}

#[cfg(test)]
mod prop_tests {
    use std::time::{Duration, Instant};

    use proptest::prelude::*;
    use windlass_machine::{Machine, Timed};
    use windlass_mam_core::{MamCommand, MamPublish};
    use windlass_qbit_core::{QbitCommand, QbitPublish};
    use windlass_types::{TorrentHash, VpnPort};
    use windlass_vpn_core::VpnPublish;

    use crate::{
        ServiceStatus, SystemStateView, WindlassAction, WindlassConfig, WindlassEvent,
        WindlassMachine, WindlassPublish, WindlassTimer,
    };

    fn any_vpn_port() -> impl Strategy<Value = VpnPort> {
        (1u16..=u16::MAX).prop_map(|p| VpnPort::try_new(p).unwrap())
    }

    fn any_torrent_hash() -> impl Strategy<Value = TorrentHash> {
        "[a-f0-9]{40}".prop_map(TorrentHash)
    }

    fn any_service_status() -> impl Strategy<Value = ServiceStatus> {
        prop_oneof![
            Just(ServiceStatus::Unknown),
            Just(ServiceStatus::Ready),
            Just(ServiceStatus::Degraded),
        ]
    }

    fn any_windlass_machine() -> impl Strategy<Value = WindlassMachine> {
        (
            any_service_status(),
            any_service_status(),
            any_service_status(),
            proptest::option::of(any_vpn_port()),
        )
            .prop_map(|(vpn, qbit, mam, forwarded_port)| {
                let mut machine = WindlassMachine::new(
                    WindlassConfig {
                        snapshot_interval: Duration::from_secs(60),
                    },
                    Instant::now(),
                );
                machine.state = SystemStateView {
                    vpn,
                    qbit,
                    mam,
                    forwarded_port,
                };
                machine
            })
    }

    fn any_vpn_publish() -> impl Strategy<Value = VpnPublish> {
        prop_oneof![
            Just(VpnPublish::Connected),
            Just(VpnPublish::Disconnected),
            any_vpn_port().prop_map(|port| VpnPublish::PortReady { port }),
            Just(VpnPublish::PortUnavailable),
        ]
    }

    fn any_mam_id() -> impl Strategy<Value = windlass_types::MamTorrentId> {
        (1u64..=1_000_000u64).prop_map(|n| windlass_types::MamTorrentId::try_new(n).unwrap())
    }

    fn any_qbit_publish() -> impl Strategy<Value = QbitPublish> {
        prop_oneof![
            Just(QbitPublish::Ready),
            any::<String>().prop_map(|reason| QbitPublish::Unavailable { reason }),
            any_vpn_port().prop_map(|port| QbitPublish::ListenPortReady { port }),
            prop::collection::vec(any_torrent_hash(), 0..4)
                .prop_map(|hashes| QbitPublish::TorrentsUpdated { hashes }),
            (any_torrent_hash(), proptest::option::of(any_mam_id()))
                .prop_map(|(hash, mam_id)| QbitPublish::DeadTorrentRemoved { hash, mam_id }),
            (any::<bool>(), any::<bool>(), any::<bool>()).prop_map(|(dht, pex, lsd)| {
                QbitPublish::BannedPrivacySettingsObserved { dht, pex, lsd }
            }),
            (any_torrent_hash(), any_torrent_hash()).prop_map(|(paused, force_resumed)| {
                QbitPublish::QueueOrchestrated {
                    paused,
                    force_resumed,
                }
            }),
            (any::<u32>(), any::<u32>()).prop_map(|(unsatisfied, limit)| {
                QbitPublish::UnsatisfiedQuotaCritical { unsatisfied, limit }
            }),
            (any::<u32>(), any::<u32>()).prop_map(|(unsatisfied, limit)| {
                QbitPublish::UnsatisfiedQuotaApproaching { unsatisfied, limit }
            }),
        ]
    }

    /// Ratio constrained to `0.0..=10.0` to avoid NaN/Infinity.
    fn any_ratio() -> impl Strategy<Value = f64> {
        (0u32..=1000u32).prop_map(|n| f64::from(n) / 100.0)
    }

    /// Upload-credit buffer constrained to `0..=(100 GiB)`.
    fn any_buffer() -> impl Strategy<Value = u64> {
        0u64..=(100 * 1024 * 1024 * 1024u64)
    }

    fn any_mam_publish() -> impl Strategy<Value = MamPublish> {
        prop_oneof![
            Just(MamPublish::Ready),
            any::<String>().prop_map(|reason| MamPublish::Unavailable { reason }),
            (0u64..=3600).prop_map(|s| MamPublish::RateLimited {
                retry_after: Duration::from_secs(s)
            }),
            proptest::option::of(any_vpn_port())
                .prop_map(|seedbox_port| MamPublish::Connectable { seedbox_port }),
            any::<String>().prop_map(|reason| MamPublish::NotConnectable { reason }),
            any::<String>().prop_map(|reason| MamPublish::Unreachable { reason }),
            any_vpn_port().prop_map(|port| MamPublish::SeedboxPortReady { port }),
            (any_ratio(), any_buffer(), any::<bool>(), any::<bool>()).prop_map(
                |(ratio, upload_credit_bytes, ratio_ok, buffer_ok)| {
                    MamPublish::UploadHealthDegraded {
                        ratio,
                        upload_credit_bytes,
                        ratio_ok,
                        buffer_ok,
                    }
                }
            ),
            (any::<u32>(), any::<String>()).prop_map(|(consecutive_failures, last_reason)| {
                MamPublish::KeepAliveDegraded {
                    consecutive_failures,
                    last_reason,
                }
            }),
        ]
    }

    fn any_disk_publish() -> impl Strategy<Value = windlass_disk_core::DiskPublish> {
        use windlass_disk_core::DiskPublish;
        prop_oneof![
            any::<u64>().prop_map(|free_bytes| DiskPublish::BelowFloor { free_bytes }),
            any::<u64>().prop_map(|free_bytes| DiskPublish::AboveFloor { free_bytes }),
        ]
    }

    fn any_windlass_event() -> impl Strategy<Value = WindlassEvent> {
        prop_oneof![
            Just(WindlassEvent::Init),
            any_vpn_publish().prop_map(WindlassEvent::Vpn),
            any_qbit_publish().prop_map(WindlassEvent::Qbit),
            any_mam_publish().prop_map(WindlassEvent::Mam),
            any_disk_publish().prop_map(WindlassEvent::Disk),
            (any::<String>(), any::<String>())
                .prop_map(|(operation, message)| WindlassEvent::DbFailed { operation, message }),
            Just(WindlassEvent::TimerFired(WindlassTimer::Snapshot)),
        ]
    }

    proptest! {
        // GLOBAL-1 (no panic).
        #[test]
        fn handle_never_panics(mut machine in any_windlass_machine(), event in any_windlass_event()) {
            let _ = machine.handle(Instant::now(), Timed::now(event));
        }

        // DOM-1 (Guarantee C, marquee): the domain never commands qBit or MAM to
        // converge on a port unless it currently holds that forwarded port.
        #[test]
        fn converge_commands_imply_forwarded_port(
            mut machine in any_windlass_machine(),
            event in any_windlass_event(),
        ) {
            let out = machine.handle(Instant::now(), Timed::now(event));
            for action in &out.actions {
                if let WindlassAction::Qbit(QbitCommand::EnsureListenPort { port })
                    | WindlassAction::Mam(MamCommand::EnsureSeedboxPort { port }) = action
                {
                    prop_assert_eq!(machine.state().forwarded_port, Some(*port));
                }
            }
        }

        // DOM-2 (Guarantees B/C): losing VPN connectivity always clears the
        // forwarded port, regardless of prior state.
        #[test]
        fn vpn_loss_clears_forwarded_port(
            mut machine in any_windlass_machine(),
            lost in prop_oneof![
                Just(VpnPublish::Disconnected),
                Just(VpnPublish::PortUnavailable),
            ],
        ) {
            machine.handle(Instant::now(), Timed::now(WindlassEvent::Vpn(lost)));
            prop_assert!(machine.state().forwarded_port.is_none());
        }

        // DOM-9 [safety] (disk-pressure routing — §22):
        // BelowFloor → exactly one EvictOneForDiskPressure + one Activity publish.
        // AboveFloor → no actions, no publishes.
        // Total invariant.
        #[test]
        fn disk_below_floor_produces_exactly_one_evict_and_one_activity(
            mut machine in any_windlass_machine(),
            free_bytes in any::<u64>(),
        ) {
            use windlass_disk_core::DiskPublish;
            let out = machine.handle(
                Instant::now(),
                Timed::now(WindlassEvent::Disk(DiskPublish::BelowFloor { free_bytes })),
            );
            prop_assert_eq!(out.actions.len(), 1);
            prop_assert!(
                matches!(out.actions[0], WindlassAction::Qbit(QbitCommand::EvictOneForDiskPressure)),
                "BelowFloor must emit exactly one EvictOneForDiskPressure"
            );
            prop_assert_eq!(out.publish.len(), 1);
            prop_assert!(
                matches!(&out.publish[0], WindlassPublish::Activity { .. }),
                "BelowFloor must emit exactly one Activity publish"
            );
        }

        #[test]
        fn disk_above_floor_produces_nothing(
            mut machine in any_windlass_machine(),
            free_bytes in any::<u64>(),
        ) {
            use windlass_disk_core::DiskPublish;
            let out = machine.handle(
                Instant::now(),
                Timed::now(WindlassEvent::Disk(DiskPublish::AboveFloor { free_bytes })),
            );
            prop_assert!(out.actions.is_empty(), "AboveFloor must emit no actions");
            prop_assert!(out.publish.is_empty(), "AboveFloor must emit no publishes");
        }

        // DOM-10 [safety] (privacy alert routing — §23):
        // `Qbit(BannedPrivacySettingsObserved { any true })` emits exactly one
        // `Db(RecordAlert{ priority: Critical })` and one `Activity` publish.
        // Total invariant.
        #[test]
        fn banned_privacy_settings_with_any_true_emits_critical_alert_and_activity(
            mut machine in any_windlass_machine(),
            // Generate at least one `true` by OR-ing with a guaranteed-true flag.
            dht in any::<bool>(),
            pex in any::<bool>(),
            lsd in any::<bool>(),
        ) {
            use windlass_db_core::{AlertRecord, DbCommand};
            use windlass_types::AlertPriority;

            // Only test the invariant when at least one setting is true.
            // (The qBit core never publishes this event with all-false, but we
            // need to skip the all-false case here to stay within the qBit-core
            // invariant scope.)
            prop_assume!(dht || pex || lsd);

            let out = machine.handle(
                Instant::now(),
                Timed::now(WindlassEvent::Qbit(
                    QbitPublish::BannedPrivacySettingsObserved { dht, pex, lsd },
                )),
            );

            let critical_alert_count = out.actions.iter().filter(|a| {
                matches!(
                    a,
                    WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                        priority: AlertPriority::Critical,
                        ..
                    }))
                )
            }).count();
            prop_assert_eq!(
                critical_alert_count,
                1,
                "BannedPrivacySettingsObserved must emit exactly one RecordAlert(Critical)"
            );

            let activity_publish_count = out
                .publish
                .iter()
                .filter(|p| matches!(p, WindlassPublish::Activity { .. }))
                .count();
            prop_assert_eq!(
                activity_publish_count,
                1,
                "BannedPrivacySettingsObserved must emit exactly one Activity publish"
            );
        }

        // DOM-12 [safety] (quota alert routing — §25):
        // `Qbit(UnsatisfiedQuotaCritical)` emits exactly one
        // `Db(RecordAlert { Critical })` and one `Activity` publish.
        // Total invariant.
        #[test]
        fn quota_critical_emits_one_critical_alert_and_one_activity(
            mut machine in any_windlass_machine(),
            unsatisfied in any::<u32>(),
            limit in any::<u32>(),
        ) {
            use windlass_db_core::{AlertRecord, DbCommand};
            use windlass_types::AlertPriority;

            let out = machine.handle(
                Instant::now(),
                Timed::now(WindlassEvent::Qbit(
                    QbitPublish::UnsatisfiedQuotaCritical { unsatisfied, limit },
                )),
            );

            let critical_alert_count = out.actions.iter().filter(|a| {
                matches!(
                    a,
                    WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                        priority: AlertPriority::Critical,
                        ..
                    }))
                )
            }).count();
            prop_assert_eq!(
                critical_alert_count,
                1,
                "UnsatisfiedQuotaCritical must emit exactly one RecordAlert(Critical)"
            );

            let activity_count = out.publish.iter()
                .filter(|p| matches!(p, WindlassPublish::Activity { .. }))
                .count();
            prop_assert_eq!(
                activity_count,
                1,
                "UnsatisfiedQuotaCritical must emit exactly one Activity publish"
            );
        }

        // DOM-12 [safety] (quota alert routing — §25):
        // `Qbit(UnsatisfiedQuotaApproaching)` emits exactly one
        // `Db(RecordAlert { Warning })` and one `Activity` publish.
        // Total invariant.
        #[test]
        fn quota_approaching_emits_one_warning_alert_and_one_activity(
            mut machine in any_windlass_machine(),
            unsatisfied in any::<u32>(),
            limit in any::<u32>(),
        ) {
            use windlass_db_core::{AlertRecord, DbCommand};
            use windlass_types::AlertPriority;

            let out = machine.handle(
                Instant::now(),
                Timed::now(WindlassEvent::Qbit(
                    QbitPublish::UnsatisfiedQuotaApproaching { unsatisfied, limit },
                )),
            );

            let warning_alert_count = out.actions.iter().filter(|a| {
                matches!(
                    a,
                    WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                        priority: AlertPriority::Warning,
                        ..
                    }))
                )
            }).count();
            prop_assert_eq!(
                warning_alert_count,
                1,
                "UnsatisfiedQuotaApproaching must emit exactly one RecordAlert(Warning)"
            );

            let activity_count = out.publish.iter()
                .filter(|p| matches!(p, WindlassPublish::Activity { .. }))
                .count();
            prop_assert_eq!(
                activity_count,
                1,
                "UnsatisfiedQuotaApproaching must emit exactly one Activity publish"
            );
        }

        // DOM-13 [safety] (upload-health alert routing — §26):
        // `Mam(UploadHealthDegraded)` emits exactly one `Db(RecordAlert{Warning})`
        // and exactly one `Activity` publish.  Total invariant.
        #[test]
        fn upload_health_degraded_emits_one_warning_alert_and_one_activity(
            mut machine in any_windlass_machine(),
            ratio in any_ratio(),
            upload_credit_bytes in any_buffer(),
            ratio_ok in any::<bool>(),
            buffer_ok in any::<bool>(),
        ) {
            use windlass_db_core::{AlertRecord, DbCommand};
            use windlass_types::AlertPriority;

            let out = machine.handle(
                Instant::now(),
                Timed::now(WindlassEvent::Mam(MamPublish::UploadHealthDegraded {
                    ratio,
                    upload_credit_bytes,
                    ratio_ok,
                    buffer_ok,
                })),
            );

            let warning_alert_count = out.actions.iter().filter(|a| {
                matches!(
                    a,
                    WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                        priority: AlertPriority::Warning,
                        ..
                    }))
                )
            }).count();
            prop_assert_eq!(
                warning_alert_count,
                1,
                "UploadHealthDegraded must emit exactly one RecordAlert(Warning)"
            );

            let activity_count = out.publish.iter()
                .filter(|p| matches!(p, WindlassPublish::Activity { .. }))
                .count();
            prop_assert_eq!(
                activity_count,
                1,
                "UploadHealthDegraded must emit exactly one Activity publish"
            );
        }

        // DOM-14 [safety] (keep-alive alert routing — §27):
        // `Mam(KeepAliveDegraded)` emits exactly one `Db(RecordAlert{Warning})`
        // and exactly one `Activity` publish.  Total invariant.
        #[test]
        fn keep_alive_degraded_emits_one_warning_alert_and_one_activity(
            mut machine in any_windlass_machine(),
            consecutive_failures in any::<u32>(),
            last_reason in any::<String>(),
        ) {
            use windlass_db_core::{AlertRecord, DbCommand};
            use windlass_types::AlertPriority;

            let out = machine.handle(
                Instant::now(),
                Timed::now(WindlassEvent::Mam(MamPublish::KeepAliveDegraded {
                    consecutive_failures,
                    last_reason,
                })),
            );

            let warning_alert_count = out.actions.iter().filter(|a| {
                matches!(
                    a,
                    WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                        priority: AlertPriority::Warning,
                        ..
                    }))
                )
            }).count();
            prop_assert_eq!(
                warning_alert_count,
                1,
                "KeepAliveDegraded must emit exactly one RecordAlert(Warning)"
            );

            let activity_count = out.publish.iter()
                .filter(|p| matches!(p, WindlassPublish::Activity { .. }))
                .count();
            prop_assert_eq!(
                activity_count,
                1,
                "KeepAliveDegraded must emit exactly one Activity publish"
            );
        }

        // DOM-15 [safety] (§28): `Mam(NotConnectable)` emits exactly one
        // `Db(RecordAlert{Warning})` + one Activity publish.  Total invariant.
        #[test]
        fn not_connectable_emits_one_warning_alert_and_one_activity(
            mut machine in any_windlass_machine(),
            reason in any::<String>(),
        ) {
            use windlass_db_core::{AlertRecord, DbCommand};
            use windlass_types::AlertPriority;

            let out = machine.handle(
                Instant::now(),
                Timed::now(WindlassEvent::Mam(MamPublish::NotConnectable { reason })),
            );

            let warning_alert_count = out.actions.iter().filter(|a| {
                matches!(
                    a,
                    WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                        priority: AlertPriority::Warning,
                        ..
                    }))
                )
            }).count();
            prop_assert_eq!(
                warning_alert_count,
                1,
                "NotConnectable must emit exactly one RecordAlert(Warning)"
            );

            let activity_count = out.publish.iter()
                .filter(|p| matches!(p, WindlassPublish::Activity { .. }))
                .count();
            prop_assert_eq!(
                activity_count,
                1,
                "NotConnectable must emit exactly one Activity publish"
            );
        }

        // DOM-16 [safety] (§28): `Mam(Unreachable)` emits zero RecordAlert
        // + exactly one Activity publish.  Transient transport signal —
        // alert escalation lives in the §27 KeepAliveDegraded path.
        // Total invariant.
        #[test]
        fn unreachable_emits_activity_only_no_alert(
            mut machine in any_windlass_machine(),
            reason in any::<String>(),
        ) {
            use windlass_db_core::DbCommand;

            let out = machine.handle(
                Instant::now(),
                Timed::now(WindlassEvent::Mam(MamPublish::Unreachable { reason })),
            );

            let alert_count = out.actions.iter().filter(|a| {
                matches!(a, WindlassAction::Db(DbCommand::RecordAlert(_)))
            }).count();
            prop_assert_eq!(
                alert_count,
                0,
                "Unreachable must emit zero RecordAlert"
            );

            let activity_count = out.publish.iter()
                .filter(|p| matches!(p, WindlassPublish::Activity { .. }))
                .count();
            prop_assert_eq!(
                activity_count,
                1,
                "Unreachable must emit exactly one Activity publish"
            );
        }
    }
}
