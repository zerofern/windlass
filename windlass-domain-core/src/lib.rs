#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windlass_db_core::{
    ActivityRecord, ActivitySource, AlertRecord, DbCommand, DownloadStateChange, DownloadStatus,
};
use windlass_disk_core::DiskPublish;
use windlass_docker_core::DockerPublish;
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome, Timed};
use windlass_mam_core::{MamCommand, MamPublish};
use windlass_qbit_core::{QbitCommand, QbitPublish};
use windlass_types::{AlertPriority, MamTorrentId, VpnIp, VpnPort};
use windlass_vpn_core::{VerificationSource, VpnCommand, VpnPublish};

// `WindlassConfig` is no longer `Copy` (§38 adds `gluetun_anchor: String`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindlassConfig {
    pub snapshot_interval: Duration,
    /// §38: name of the Gluetun anchor container.  Used to address
    /// `Docker(RestartContainer { name })` during crash recovery.
    pub gluetun_anchor: String,
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
    /// §38: Docker-core lifecycle publishes — container crashed/healthy,
    /// stop/start results, log dumps, and the migrated §35 stale-namespace
    /// signals (`DependentNetworkUntrusted` / `DependentNetworkTrusted` /
    /// `RestartStorm`).
    Docker(DockerPublish),
    DbFailed {
        operation: String,
        message: String,
    },
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
    /// §38: command the Docker core (e.g. `StopDependents`,
    /// `RestartContainer`, `DumpAllLogs`).  Used by the crash-recovery
    /// orchestration on `Vpn(Crashed)` / `Vpn(Recovered)`.
    Docker(windlass_docker_core::DockerCommand),
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

// §29: `WindlassCommand` carries a `DownloadCandidate` (String fields) in
// `TryAddTorrent`, so it can no longer be `Copy`.  Only `Clone`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindlassCommand {
    Refresh,
    /// §29: ask the domain core to admit and start downloading a candidate
    /// torrent.  Runs the composite fail-closed admission predicate; on
    /// success emits `Qbit(QbitCommand::AddTorrent)`, on failure emits an
    /// `Activity` publish listing the failing gates.
    TryAddTorrent {
        candidate: DownloadCandidate,
    },
}

/// §29: the librarian-side admission-candidate fields.
///
/// Carries the candidate identity (`mam_id`, `dl_url`) plus every field
/// referenced by an admission gate.  See `WindlassMachine::admit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadCandidate {
    pub mam_id: MamTorrentId,
    pub dl_url: String,
    pub size_bytes: u64,
    pub numfiles: u32,
    pub freeleech: bool,
    /// Estimated download duration, used by the freeleech-window gate.
    pub est_download_duration: Duration,
    /// `true` iff the user has already snatched this torrent (librarian A2
    /// gate).  Admission blocks when true.
    pub my_snatched: bool,
    /// Absolute end of the freeleech window for this candidate, if any.
    /// `None` for non-freeleech candidates.  Admission requires
    /// `now + est_download_duration + safety_buffer <= window_end`.
    pub freeleech_window_end: Option<chrono::DateTime<chrono::Utc>>,
}

/// §29: per-gate failure reason returned by `WindlassMachine::admit`.
/// Each variant maps to a single sentence in the blocked-admission activity
/// log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateFailure {
    /// Upload-health gate (§26): ratio or upload-credit buffer below
    /// configured minimums.  Bypassed for freeleech candidates only on the
    /// ratio side — the buffer is always required.
    UploadHealth,
    /// Unsatisfied-quota gate (§25): MAM Rule 2.8 class-cap limit reached.
    UnsatisfiedQuotaFull,
    /// qBit privacy gate (§23): `DHT`, `PeX`, or `LSD` is enabled.
    QbitPrivacyUnclean,
    /// qBit listen port does not match the VPN's forwarded port.
    QbitPortDesynced,
    /// MAM health gate: MAM is degraded (unavailable, not connectable,
    /// unreachable, or never reached `Ready`).
    MamUnhealthy,
    /// VPN IP compliance gate (§30 — not yet implemented; placeholder).
    VpnIpNonCompliant,
    /// Librarian gate (A2): `candidate.my_snatched == true`.
    AlreadySnatched,
    /// Librarian gate (A2): candidate.numfiles > 20 (collection).
    Collection,
    /// Librarian gate (A2): freeleech window cannot accommodate the estimated
    /// download duration plus safety buffer.
    FreeleechWindowTooNarrow,
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

/// §29: per-gate booleans for the composite download-admission predicate.
///
/// Every field defaults to the fail-closed value (`false` for "ok"-style
/// gates, `true` for "full"-style gates) so a candidate admitted before
/// any positive signal arrives is blocked.  Each publish handler in
/// `WindlassMachine::handle` flips the corresponding flag.
///
/// `vpn_ip_compliant` is currently a stub — operator-readiness §30 is not
/// yet implemented.  We default it to `Some(true)` (with a TODO) so §29
/// can be exercised end-to-end; §30 will replace this default with the
/// real VPN-IP comparison and update the relevant publish wiring.
// Several gate flags are inherently boolean; grouping them is the structural
// shape (counterpart bool per gate), not accidental.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionState {
    pub upload_health_ok: bool,
    pub unsatisfied_quota_full: bool,
    pub qbit_privacy_clean: bool,
    pub qbit_listen_port: Option<VpnPort>,
    pub mam_healthy: bool,
    /// §30 placeholder — see struct docs.
    pub vpn_ip_compliant: Option<bool>,
}

impl AdmissionState {
    /// Fail-closed defaults: every "ok" gate is false, every "full" gate is
    /// true, and `vpn_ip_compliant` is `None` (unknown).  §30 flips it to
    /// `Some(true)` on a successful seedbox update and `Some(false)` on a
    /// fresh ASN mismatch.
    const fn fail_closed() -> Self {
        Self {
            upload_health_ok: false,
            unsatisfied_quota_full: true,
            qbit_privacy_clean: false,
            qbit_listen_port: None,
            mam_healthy: false,
            vpn_ip_compliant: None,
        }
    }

    fn qbit_port_synced(&self, forwarded_port: Option<VpnPort>) -> bool {
        match (self.qbit_listen_port, forwarded_port) {
            (Some(a), Some(b)) => a.into_inner() == b.into_inner(),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindlassMachine {
    config: WindlassConfig,
    state: SystemStateView,
    admission: AdmissionState,
}

impl WindlassMachine {
    #[must_use]
    pub const fn state(&self) -> &SystemStateView {
        &self.state
    }

    #[must_use]
    pub const fn admission(&self) -> &AdmissionState {
        &self.admission
    }

    /// §29: composite fail-closed admission predicate.
    ///
    /// Returns `Ok(())` iff every gate holds for `candidate`.  Otherwise
    /// returns the list of failing gates in canonical order so the activity
    /// log is deterministic.  Total — holds for any `(state, candidate)`
    /// pair, including ones where the underlying publishes are missing
    /// (those default to the fail-closed value).
    ///
    /// Each gate maps to one `GateFailure` variant; see that enum for the
    /// per-gate definitions.
    ///
    /// # Errors
    /// Returns `Err(Vec<GateFailure>)` listing every failing gate in
    /// canonical order when at least one gate fails.
    pub fn admit(
        &self,
        candidate: &DownloadCandidate,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), Vec<GateFailure>> {
        let mut failures = Vec::new();
        // Upload health: freeleech bypasses ratio but still needs the buffer
        // (semantics owned by MAM core's `upload_health_ok(freeleech)`).
        // We only have the non-freeleech result wired in admission state;
        // for freeleech, treat upload-health-ok as true (buffer is rolled in
        // via the §26 publish for now).  This mirrors the MAM-7 semantics
        // and the librarian-readiness A1 cost-rule docs.
        if !candidate.freeleech && !self.admission.upload_health_ok {
            failures.push(GateFailure::UploadHealth);
        }
        if self.admission.unsatisfied_quota_full {
            failures.push(GateFailure::UnsatisfiedQuotaFull);
        }
        if !self.admission.qbit_privacy_clean {
            failures.push(GateFailure::QbitPrivacyUnclean);
        }
        if !self.admission.qbit_port_synced(self.state.forwarded_port) {
            failures.push(GateFailure::QbitPortDesynced);
        }
        if !self.admission.mam_healthy {
            failures.push(GateFailure::MamUnhealthy);
        }
        if self.admission.vpn_ip_compliant != Some(true) {
            failures.push(GateFailure::VpnIpNonCompliant);
        }
        if candidate.my_snatched {
            failures.push(GateFailure::AlreadySnatched);
        }
        if candidate.numfiles > 20 {
            failures.push(GateFailure::Collection);
        }
        if candidate.freeleech && !freeleech_window_fits(now, candidate) {
            failures.push(GateFailure::FreeleechWindowTooNarrow);
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures)
        }
    }

    fn snapshot_action(&self) -> WindlassAction {
        WindlassAction::SaveSystemSnapshot(self.state.clone())
    }
}

/// Safety buffer added to the candidate's estimated download duration when
/// checking the freeleech window.  Configurable later; matches the
/// librarian-readiness §A2 default.
const FREELEECH_SAFETY_BUFFER: Duration = Duration::from_mins(30);

fn freeleech_window_fits(
    now: chrono::DateTime<chrono::Utc>,
    candidate: &DownloadCandidate,
) -> bool {
    let Some(window_end) = candidate.freeleech_window_end else {
        // No window provided for a freeleech candidate → fail-closed.
        return false;
    };
    let total = candidate.est_download_duration + FREELEECH_SAFETY_BUFFER;
    let Ok(total_chrono) = chrono::Duration::from_std(total) else {
        return false;
    };
    now + total_chrono <= window_end
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
            admission: AdmissionState::fail_closed(),
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
            // §38 / DOM-27: rising-edge VPN crash drives the full Docker
            // crash-recovery sequence — dump logs, stop dependents,
            // restart Gluetun — plus a Critical alert so the operator
            // knows.  Fires exactly once per VPN healthy → unhealthy
            // transition (VPN-17).
            WindlassEvent::Vpn(VpnPublish::Crashed) => {
                use windlass_docker_core::DockerCommand;
                Outcome {
                    actions: vec![
                        WindlassAction::Docker(DockerCommand::DumpAllLogs),
                        WindlassAction::Docker(DockerCommand::StopDependents),
                        WindlassAction::Docker(DockerCommand::RestartContainer {
                            name: self.config.gluetun_anchor.clone(),
                        }),
                        WindlassAction::SendAlert {
                            priority: AlertPriority::Critical,
                            title: "Gluetun died".to_string(),
                            body: "💀 Gluetun crashed.  Dumping logs, stopping dependents, and restarting.".to_string(),
                        },
                    ],
                    publish: Vec::new(),
                }
            }
            // §38 / DOM-28: rising-edge VPN recovery starts dependents
            // back up.  Fires exactly once per unhealthy → healthy
            // transition (VPN-18).
            WindlassEvent::Vpn(VpnPublish::Recovered) => {
                use windlass_docker_core::DockerCommand;
                Outcome {
                    actions: vec![WindlassAction::Docker(DockerCommand::StartDependents)],
                    publish: Vec::new(),
                }
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
            // §31: VPN observed a fresh public IP from the Gluetun file.
            // Forward to MAM so it can issue UpdateSeedbox (deduped against
            // the last observed IP) and arm the stale-registration timer.
            WindlassEvent::Vpn(VpnPublish::PublicIpObserved { ip }) => Outcome {
                actions: vec![WindlassAction::Mam(MamCommand::ObservedIpChanged { ip })],
                publish: Vec::new(),
            },
            // §31: VPN disconnected or Gluetun deleted the IP file.  Clear
            // the §29 admission gate to its unknown default — autograb is
            // blocked until the next PublicIpObserved.
            WindlassEvent::Vpn(VpnPublish::PublicIpUnavailable) => {
                self.admission.vpn_ip_compliant = None;
                self.publish_state_with_activity(
                    "VPN public IP unavailable — admission blocked".to_string(),
                )
            }
            // §31 / DOM-21 + §33 / DOM-23: file-vs-verified mismatch on
            // either source — strong leak signal.  Flip the §29 gate and
            // fire a Critical alert that names the disagreeing source.
            WindlassEvent::Vpn(VpnPublish::PublicIpMismatch {
                file_ip,
                verified_ip,
                source,
            }) => {
                self.admission.vpn_ip_compliant = Some(false);
                Self::on_public_ip_mismatch(file_ip, verified_ip, source)
            }
            // §31 / DOM-22: persistent ifconfig.co failure.  Warning alert
            // + Activity, does NOT block admission (Gluetun file is still
            // trusted as the IP source).
            WindlassEvent::Vpn(VpnPublish::PublicIpVerificationDegraded {
                consecutive_failures,
                last_reason,
            }) => Self::on_public_ip_verification_degraded(consecutive_failures, &last_reason),
            // §33 / DOM-24: same Warning shape for the MAM-jsonIp source.
            // Independent counter, distinct alert title.
            WindlassEvent::Vpn(VpnPublish::MamIpVerificationDegraded {
                consecutive_failures,
                last_reason,
            }) => Self::on_mam_ip_verification_degraded(consecutive_failures, &last_reason),
            // §35 / DOM-25 (§38 migrated to Docker core): stale-namespace
            // dependent — Critical alert + admission block.  Docker core
            // has already requested the restart; the Critical signal makes
            // the operator aware.
            WindlassEvent::Docker(DockerPublish::DependentNetworkUntrusted {
                name,
                dependent_started_at,
                gluetun_healthy_since,
            }) => {
                self.admission.vpn_ip_compliant = Some(false);
                Self::on_dependent_network_untrusted(
                    &name,
                    dependent_started_at,
                    gluetun_healthy_since,
                )
            }
            // §35 (§38 migrated to Docker core): dependent's namespace is
            // fresh again.  Activity only; admission stays blocked until
            // §31/§32/§33 re-confirm the IP/ASN gates.
            WindlassEvent::Docker(DockerPublish::DependentNetworkTrusted { name }) => {
                Self::on_dependent_network_trusted(&name)
            }
            // §35 / DOM-26 (§38 migrated to Docker core): restart circuit
            // breaker tripped.  Critical alert + Activity; admission stays
            // blocked.
            WindlassEvent::Docker(DockerPublish::RestartStorm { window_count, max }) => {
                self.admission.vpn_ip_compliant = Some(false);
                Self::on_restart_storm(window_count, max)
            }
            // §38 PR 6: Docker-core anchor lifecycle drives VPN core.
            // Translate per-name publishes for the anchor into
            // VpnCommand variants so VPN core no longer polls Docker
            // directly.  Non-anchor lifecycle is informational here;
            // PR 4's crash-recovery path already covers the anchor
            // alert via VpnPublish::Crashed.
            WindlassEvent::Docker(DockerPublish::ContainerHealthy { name }) => {
                if name == self.config.gluetun_anchor {
                    Outcome {
                        actions: vec![WindlassAction::Vpn(VpnCommand::ContainerHealthy)],
                        publish: Vec::new(),
                    }
                } else {
                    Outcome::none()
                }
            }
            WindlassEvent::Docker(DockerPublish::ContainerCrashed { name }) => {
                if name == self.config.gluetun_anchor {
                    Outcome {
                        actions: vec![WindlassAction::Vpn(VpnCommand::ContainerUnhealthy)],
                        publish: Vec::new(),
                    }
                } else {
                    Outcome::none()
                }
            }
            // Other DockerPublish variants are informational only.
            WindlassEvent::Docker(
                DockerPublish::Stopped { .. }
                | DockerPublish::Started { .. }
                | DockerPublish::LogsDumped { .. },
            ) => Outcome::none(),
            WindlassEvent::Qbit(QbitPublish::Ready) => {
                self.state.qbit = ServiceStatus::Ready;
                self.publish_state()
            }
            WindlassEvent::Qbit(QbitPublish::Unavailable { reason }) => {
                self.state.qbit = ServiceStatus::Degraded;
                self.publish_state_with_activity(reason)
            }
            // §29: ListenPortReady drives the qBit-port-synced admission gate.
            WindlassEvent::Qbit(QbitPublish::ListenPortReady { port }) => {
                self.admission.qbit_listen_port = Some(port);
                Outcome::none()
            }
            // §29: Connectable indicates MAM is healthy for admission.
            WindlassEvent::Mam(MamPublish::Connectable { .. }) => {
                self.admission.mam_healthy = true;
                Outcome::none()
            }
            WindlassEvent::Qbit(QbitPublish::TorrentsUpdated { .. })
            | WindlassEvent::Disk(DiskPublish::AboveFloor { .. }) => Outcome::none(),
            // DOM-11: QueueOrchestrated → one RecordActivity + one Activity publish.
            WindlassEvent::Qbit(QbitPublish::QueueOrchestrated {
                ref paused,
                ref force_resumed,
            }) => Self::on_queue_orchestrated(paused, force_resumed),
            // DOM-12: UnsatisfiedQuotaCritical → one RecordAlert(Critical) + one Activity publish.
            // §29: also marks the unsatisfied-quota gate as full.
            WindlassEvent::Qbit(QbitPublish::UnsatisfiedQuotaCritical { unsatisfied, limit }) => {
                self.admission.unsatisfied_quota_full = true;
                Self::on_unsatisfied_quota_critical(unsatisfied, limit)
            }
            // DOM-12: UnsatisfiedQuotaApproaching → one RecordAlert(Warning) + one Activity publish.
            // §29: approaching is below the limit (within 5), so the gate is NOT full.
            WindlassEvent::Qbit(QbitPublish::UnsatisfiedQuotaApproaching {
                unsatisfied,
                limit,
            }) => {
                self.admission.unsatisfied_quota_full = false;
                Self::on_unsatisfied_quota_approaching(unsatisfied, limit)
            }
            // §29: positive-side counterpart — quota is safely under threshold.
            WindlassEvent::Qbit(QbitPublish::UnsatisfiedQuotaOk { .. }) => {
                self.admission.unsatisfied_quota_full = false;
                Outcome::none()
            }
            WindlassEvent::Qbit(QbitPublish::DeadTorrentRemoved { mam_id, .. }) => {
                Self::on_dead_torrent_removed(mam_id)
            }
            // DOM-10: BannedPrivacySettingsObserved → one Critical RecordAlert + one Activity.
            // §29: also marks the qBit privacy gate as unclean.
            WindlassEvent::Qbit(QbitPublish::BannedPrivacySettingsObserved { dht, pex, lsd }) => {
                self.admission.qbit_privacy_clean = false;
                Self::on_banned_privacy_settings_observed(dht, pex, lsd)
            }
            // §29: positive-side counterpart — DHT/PeX/LSD all off.
            WindlassEvent::Qbit(QbitPublish::PrivacyClean) => {
                self.admission.qbit_privacy_clean = true;
                Outcome::none()
            }
            WindlassEvent::Mam(MamPublish::SeedboxPortReady { port }) => Outcome {
                actions: vec![WindlassAction::SendAlert {
                    priority: AlertPriority::Info,
                    title: "MAM seedbox updated".to_string(),
                    body: format!("MAM seedbox registered with port {}.", port.into_inner()),
                }],
                publish: Vec::new(),
            },
            // §29: MAM auth-success marks the MAM-healthy admission gate true.
            WindlassEvent::Mam(MamPublish::Ready) => {
                self.state.mam = ServiceStatus::Ready;
                self.admission.mam_healthy = true;
                self.publish_state()
            }
            // §28: Unavailable keeps the broad "MAM degraded" semantics —
            // Activity log + ServiceStatus::Degraded, no alert.
            // §29: clears the MAM-healthy admission gate.
            WindlassEvent::Mam(MamPublish::Unavailable { reason }) => {
                self.state.mam = ServiceStatus::Degraded;
                self.admission.mam_healthy = false;
                self.publish_state_with_activity(reason)
            }
            // DOM-15 (§28): NotConnectable is now a real connectivity problem
            // — MAM responded and told us our client is unreachable from
            // their side.  Emit a Warning alert in addition to the
            // Activity/Degraded path.  §29: clears MAM-healthy.
            WindlassEvent::Mam(MamPublish::NotConnectable { reason }) => {
                self.state.mam = ServiceStatus::Degraded;
                self.admission.mam_healthy = false;
                self.on_mam_not_connectable(&reason)
            }
            // DOM-16 (§28): Unreachable is a transient transport failure
            // (DNS, TCP, TLS, timeout).  Activity + ServiceStatus::Degraded
            // only — no alert here.  Persistent unreachability already
            // surfaces via the §27 KeepAliveDegraded path.  §29: clears
            // MAM-healthy.
            WindlassEvent::Mam(MamPublish::Unreachable { reason }) => {
                self.state.mam = ServiceStatus::Degraded;
                self.admission.mam_healthy = false;
                self.publish_state_with_activity(format!("MAM unreachable: {reason}"))
            }
            // DOM-13: UploadHealthDegraded → one RecordAlert(Warning) + one Activity publish.
            // §29: clears the upload-health admission gate.
            WindlassEvent::Mam(MamPublish::UploadHealthDegraded {
                ratio,
                upload_credit_bytes,
                ratio_ok,
                buffer_ok,
            }) => {
                self.admission.upload_health_ok = false;
                Self::on_upload_health_degraded(ratio, upload_credit_bytes, ratio_ok, buffer_ok)
            }
            // §29: positive-side counterpart — both metrics meet the minimums.
            WindlassEvent::Mam(MamPublish::UploadHealthOk { .. }) => {
                self.admission.upload_health_ok = true;
                Outcome::none()
            }
            // DOM-20 (§30): MAM rejected our IP with an ASN mismatch.
            // Blocks admission and fires a Critical alert.
            WindlassEvent::Mam(MamPublish::AsnMismatch { ip }) => {
                self.admission.vpn_ip_compliant = Some(false);
                Self::on_mam_asn_mismatch(ip)
            }
            // §30: MAM accepted our IP — clear the admission gate.
            WindlassEvent::Mam(MamPublish::AsnAccepted) => {
                self.admission.vpn_ip_compliant = Some(true);
                Outcome::none()
            }
            // DOM-14: KeepAliveDegraded → one RecordAlert(Warning) + one Activity publish.
            WindlassEvent::Mam(MamPublish::KeepAliveDegraded {
                consecutive_failures,
                last_reason,
            }) => Self::on_keep_alive_degraded(consecutive_failures, &last_reason),
            WindlassEvent::Mam(MamPublish::RateLimited { retry_after }) => {
                self.state.mam = ServiceStatus::Degraded;
                self.admission.mam_healthy = false;
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
        match cmd {
            WindlassCommand::Refresh => {
                let actions = vec![
                    WindlassAction::Vpn(VpnCommand::RefreshState),
                    WindlassAction::Qbit(QbitCommand::RefreshTorrents),
                    WindlassAction::Mam(MamCommand::RefreshStatus),
                ];
                Self::outcome(actions, WindlassResponse::Accepted)
            }
            // DOM-17/18/19 (§29): composite fail-closed admission.
            WindlassCommand::TryAddTorrent { candidate } => {
                let now = chrono::Utc::now();
                match self.admit(&candidate, now) {
                    Ok(()) => Self::outcome(
                        vec![WindlassAction::Qbit(QbitCommand::AddTorrent {
                            mam_id: candidate.mam_id,
                            dl_url: candidate.dl_url,
                        })],
                        WindlassResponse::Accepted,
                    ),
                    Err(failures) => Self::outcome_with_publish(
                        Vec::new(),
                        vec![WindlassPublish::Activity {
                            message: blocked_admission_message(candidate.mam_id, &failures),
                        }],
                        WindlassResponse::Accepted,
                    ),
                }
            }
        }
    }
}

/// §29: human-readable activity-log line for a blocked admission.  Lists
/// every failed gate in canonical order — output is deterministic.
fn blocked_admission_message(mam_id: MamTorrentId, failures: &[GateFailure]) -> String {
    let gates: Vec<&'static str> = failures.iter().copied().map(gate_label).collect();
    format!(
        "Admission blocked for MAM #{}: {}",
        mam_id.into_inner(),
        gates.join(", "),
    )
}

const fn gate_label(gate: GateFailure) -> &'static str {
    match gate {
        GateFailure::UploadHealth => "upload health",
        GateFailure::UnsatisfiedQuotaFull => "unsatisfied-quota limit reached",
        GateFailure::QbitPrivacyUnclean => "qBit privacy settings (DHT/PeX/LSD)",
        GateFailure::QbitPortDesynced => "qBit listen port out of sync with VPN",
        GateFailure::MamUnhealthy => "MAM unhealthy",
        GateFailure::VpnIpNonCompliant => "VPN IP non-compliant",
        GateFailure::AlreadySnatched => "already snatched",
        GateFailure::Collection => "torrent is a collection (numfiles > 20)",
        GateFailure::FreeleechWindowTooNarrow => {
            "freeleech window too narrow for estimated download"
        }
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

    /// Handles `Vpn(PublicIpMismatch)` (DOM-21 / §31 + DOM-23 / §33).
    ///
    /// Gluetun's file IP and an external verification source disagree —
    /// strong indicator that traffic is leaking around the VPN.  Emits one
    /// `RecordAlert(Critical)`, one `RecordActivity`, and one `Activity`
    /// publish whose title and body name the source so the operator can
    /// distinguish an ifconfig.co edge case from a MAM compliance issue.
    /// The caller has already flipped `admission.vpn_ip_compliant` to
    /// `Some(false)`.
    fn on_public_ip_mismatch(
        file_ip: VpnIp,
        verified_ip: VpnIp,
        source: VerificationSource,
    ) -> Outcome<WindlassAction, WindlassPublish> {
        let (source_label, source_human, action_label) = match source {
            VerificationSource::IfConfigCo => (
                "ifconfig.co",
                "ifconfig.co (public-internet view)",
                "vpn_public_ip_mismatch_ifconfig",
            ),
            VerificationSource::MamJsonIp => (
                "MAM /json/jsonIp.php",
                "MAM /json/jsonIp.php (what MAM sees)",
                "vpn_public_ip_mismatch_mam",
            ),
        };
        let body = format!(
            "Gluetun reports the VPN exit as {} but {} reports we appear \
             as {}. Treat as a potential leak; autograb is blocked until \
             verification recovers.",
            file_ip.0, source_human, verified_ip.0,
        );
        let message = format!(
            "VPN IP mismatch ({source_label}): file={} verified={}",
            file_ip.0, verified_ip.0,
        );
        Outcome {
            actions: vec![
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    at: chrono::Utc::now(),
                    priority: AlertPriority::Critical,
                    title: format!("VPN public IP mismatch ({source_label})"),
                    body,
                })),
                WindlassAction::Db(DbCommand::RecordActivity(ActivityRecord {
                    at: chrono::Utc::now(),
                    source: ActivitySource::Vpn,
                    action: action_label.to_string(),
                    book_id: None,
                    detail: Some(message.clone()),
                    metadata: serde_json::Value::Null,
                })),
            ],
            publish: vec![WindlassPublish::Activity { message }],
        }
    }

    /// Handles `Vpn(MamIpVerificationDegraded)` (DOM-24 / §33).
    ///
    /// MAM `/json/jsonIp.php` has been unreachable past the configured
    /// failure threshold.  Same Warning shape as the ifconfig.co counterpart
    /// (DOM-22); does **not** block admission.
    fn on_mam_ip_verification_degraded(
        consecutive_failures: u32,
        last_reason: &str,
    ) -> Outcome<WindlassAction, WindlassPublish> {
        let body = format!(
            "MAM /json/jsonIp.php verification failed {consecutive_failures} \
             times in a row. Last reason: {last_reason}. The Gluetun-reported \
             IP is still the source of truth; admission is unaffected.",
        );
        let message = format!(
            "VPN MAM-IP verification degraded: {consecutive_failures} \
             consecutive failures (last: {last_reason})",
        );
        Outcome {
            actions: vec![
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    at: chrono::Utc::now(),
                    priority: AlertPriority::Warning,
                    title: "MAM IP verification failing".to_string(),
                    body,
                })),
                WindlassAction::Db(DbCommand::RecordActivity(ActivityRecord {
                    at: chrono::Utc::now(),
                    source: ActivitySource::Vpn,
                    action: "vpn_mam_ip_verification_degraded".to_string(),
                    book_id: None,
                    detail: Some(message.clone()),
                    metadata: serde_json::Value::Null,
                })),
            ],
            publish: vec![WindlassPublish::Activity { message }],
        }
    }

    /// Handles `Vpn(DependentNetworkUntrusted)` (DOM-25 / §35).
    ///
    /// A dependent container's network namespace is stale (it started
    /// before the current Gluetun health window).  Emits one
    /// `RecordAlert(Critical)`, one `RecordActivity`, and one `Activity`
    /// publish.  Caller has already flipped
    /// `admission.vpn_ip_compliant` to `Some(false)`.
    fn on_dependent_network_untrusted(
        name: &str,
        dependent_started_at: chrono::DateTime<chrono::Utc>,
        gluetun_healthy_since: chrono::DateTime<chrono::Utc>,
    ) -> Outcome<WindlassAction, WindlassPublish> {
        let body = format!(
            "Dependent container `{name}` started at {dependent_started_at} \
             before Gluetun became healthy at {gluetun_healthy_since}. Its \
             network namespace may be stale; the VPN core has requested a \
             restart. Autograb is blocked until trust recovers.",
        );
        let message = format!(
            "dependent `{name}` on stale namespace (started {dependent_started_at}, \
             gluetun healthy_since {gluetun_healthy_since})",
        );
        Outcome {
            actions: vec![
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    at: chrono::Utc::now(),
                    priority: AlertPriority::Critical,
                    title: format!("Dependent `{name}` network untrusted"),
                    body,
                })),
                WindlassAction::Db(DbCommand::RecordActivity(ActivityRecord {
                    at: chrono::Utc::now(),
                    source: ActivitySource::Vpn,
                    action: "vpn_dependent_network_untrusted".to_string(),
                    book_id: None,
                    detail: Some(message.clone()),
                    metadata: serde_json::Value::Null,
                })),
            ],
            publish: vec![WindlassPublish::Activity { message }],
        }
    }

    /// Handles `Vpn(DependentNetworkTrusted)` (DOM-25 counterpart / §35).
    ///
    /// The dependent's namespace is fresh again.  No alert; just an
    /// activity note so the operator can see recovery.  Admission stays
    /// where it was (§31/§32/§33 own the IP-side gate).
    fn on_dependent_network_trusted(name: &str) -> Outcome<WindlassAction, WindlassPublish> {
        let message = format!("dependent `{name}` network trusted (fresh namespace)");
        Outcome {
            actions: vec![WindlassAction::Db(DbCommand::RecordActivity(
                ActivityRecord {
                    at: chrono::Utc::now(),
                    source: ActivitySource::Vpn,
                    action: "vpn_dependent_network_trusted".to_string(),
                    book_id: None,
                    detail: Some(message.clone()),
                    metadata: serde_json::Value::Null,
                },
            ))],
            publish: vec![WindlassPublish::Activity { message }],
        }
    }

    /// Handles `Vpn(RestartStorm)` (DOM-26 / §35).
    ///
    /// The restart circuit breaker tripped — the VPN core is refusing
    /// to emit further `RestartContainer` actions until the window
    /// slides.  Operator must intervene.  Emits one
    /// `RecordAlert(Critical)`, one `RecordActivity`, and one
    /// `Activity` publish.  Admission stays blocked.
    fn on_restart_storm(window_count: u32, max: u32) -> Outcome<WindlassAction, WindlassPublish> {
        let body = format!(
            "Restart circuit breaker tripped: {window_count} dependent \
             restarts in the current window (limit {max}). Further \
             RestartContainer actions are suppressed and a crash dump \
             has been requested (deduped per incident).  Investigate \
             before clearing.",
        );
        let message = format!("restart storm: {window_count} restarts >= cap {max}");
        Outcome {
            actions: vec![
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    at: chrono::Utc::now(),
                    priority: AlertPriority::Critical,
                    title: "Dependent restart storm".to_string(),
                    body,
                })),
                WindlassAction::Db(DbCommand::RecordActivity(ActivityRecord {
                    at: chrono::Utc::now(),
                    source: ActivitySource::Vpn,
                    action: "vpn_restart_storm".to_string(),
                    book_id: None,
                    detail: Some(message.clone()),
                    metadata: serde_json::Value::Null,
                })),
            ],
            publish: vec![WindlassPublish::Activity { message }],
        }
    }

    /// Handles `Vpn(PublicIpVerificationDegraded)` (DOM-22 / §31).
    ///
    /// ifconfig.co has been unreachable for at least the configured
    /// failure threshold.  Surface as a `Warning` — Gluetun's file is
    /// still the source of truth, so admission is not blocked.
    fn on_public_ip_verification_degraded(
        consecutive_failures: u32,
        last_reason: &str,
    ) -> Outcome<WindlassAction, WindlassPublish> {
        let body = format!(
            "Public-IP verification (ifconfig.co) failed {consecutive_failures} \
             times in a row. Last reason: {last_reason}. The Gluetun-reported \
             IP is still the source of truth; admission is unaffected.",
        );
        let message = format!(
            "VPN public-IP verification degraded: {consecutive_failures} \
             consecutive failures (last: {last_reason})",
        );
        Outcome {
            actions: vec![
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    at: chrono::Utc::now(),
                    priority: AlertPriority::Warning,
                    title: "VPN IP verification failing".to_string(),
                    body,
                })),
                WindlassAction::Db(DbCommand::RecordActivity(ActivityRecord {
                    at: chrono::Utc::now(),
                    source: ActivitySource::Vpn,
                    action: "vpn_public_ip_verification_degraded".to_string(),
                    book_id: None,
                    detail: Some(message.clone()),
                    metadata: serde_json::Value::Null,
                })),
            ],
            publish: vec![WindlassPublish::Activity { message }],
        }
    }

    /// Handles the `AsnMismatch` MAM publish (DOM-20 / §30).
    ///
    /// MAM rejected our dynamic-seedbox update because our current IP
    /// belongs to an unregistered ASN.  Emits one `RecordAlert(Critical)`,
    /// one `RecordActivity`, and one `Activity` publish.  The §29 admission
    /// gate is already blocked by the caller, so the alert is the operator's
    /// signal to register the new ASN with MAM (or wait for Windlass
    /// automation to do so in a future story).
    fn on_mam_asn_mismatch(ip: VpnIp) -> Outcome<WindlassAction, WindlassPublish> {
        let body = format!(
            "MAM rejected the dynamic-seedbox update from IP {}: ASN not \
             registered for this account. Autograb is blocked until the \
             next successful seedbox update.",
            ip.0,
        );
        let message = format!("MAM ASN mismatch for {}", ip.0);
        Outcome {
            actions: vec![
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    at: chrono::Utc::now(),
                    priority: AlertPriority::Critical,
                    title: "MAM ASN mismatch".to_string(),
                    body,
                })),
                WindlassAction::Db(DbCommand::RecordActivity(ActivityRecord {
                    at: chrono::Utc::now(),
                    source: ActivitySource::Mam,
                    action: "mam_asn_mismatch".to_string(),
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
                gluetun_anchor: "gluetun".to_string(),
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

    // ── DOM-20: AsnMismatch routing (§30) ─────────────────────────────────────

    #[test]
    fn asn_mismatch_emits_one_critical_alert_and_blocks_admission() {
        use windlass_db_core::{AlertRecord, DbCommand};
        use windlass_mam_core::MamPublish;
        use windlass_types::{AlertPriority, VpnIp};

        let mut machine = machine();
        let ip = VpnIp(std::net::Ipv4Addr::new(10, 20, 30, 40));

        let out = handle(
            &mut machine,
            WindlassEvent::Mam(MamPublish::AsnMismatch { ip }),
        );

        let critical_alert_count = out
            .actions
            .iter()
            .filter(|a| {
                matches!(
                    a,
                    WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                        priority: AlertPriority::Critical,
                        ..
                    }))
                )
            })
            .count();
        assert_eq!(
            critical_alert_count, 1,
            "AsnMismatch must emit exactly one RecordAlert(Critical)"
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

        // Admission gate flipped to non-compliant.
        assert_eq!(machine.admission().vpn_ip_compliant, Some(false));
    }

    #[test]
    fn asn_accepted_flips_admission_to_compliant() {
        use windlass_mam_core::MamPublish;

        let mut machine = machine();
        // Pre-condition: gate is unknown by default.
        assert_eq!(machine.admission().vpn_ip_compliant, None);

        let out = handle(&mut machine, WindlassEvent::Mam(MamPublish::AsnAccepted));

        assert!(out.actions.is_empty(), "AsnAccepted emits no actions");
        assert!(out.publish.is_empty(), "AsnAccepted emits no publishes");
        assert_eq!(machine.admission().vpn_ip_compliant, Some(true));
    }

    // ── §38 / DOM-27 + DOM-28: VPN crash-recovery orchestration ──────────

    #[test]
    fn vpn_crashed_drives_dump_stop_restart_and_critical_alert() {
        // DOM-27: rising-edge Vpn(Crashed) emits the full crash-recovery
        // fan-out in order: DumpAllLogs, StopDependents, RestartContainer
        // (anchor), SendAlert(Critical).
        use windlass_docker_core::DockerCommand;
        use windlass_types::AlertPriority;

        let mut machine = machine();
        let out = handle(&mut machine, WindlassEvent::Vpn(VpnPublish::Crashed));

        assert_eq!(out.actions.len(), 4, "expected 4 crash-recovery actions");
        assert!(matches!(
            out.actions[0],
            WindlassAction::Docker(DockerCommand::DumpAllLogs)
        ));
        assert!(matches!(
            out.actions[1],
            WindlassAction::Docker(DockerCommand::StopDependents)
        ));
        assert!(matches!(
            &out.actions[2],
            WindlassAction::Docker(DockerCommand::RestartContainer { name }) if name == "gluetun"
        ));
        assert!(matches!(
            &out.actions[3],
            WindlassAction::SendAlert { priority, .. } if *priority == AlertPriority::Critical
        ));
    }

    #[test]
    fn vpn_recovered_drives_start_dependents() {
        // DOM-28: rising-edge Vpn(Recovered) emits StartDependents only.
        use windlass_docker_core::DockerCommand;

        let mut machine = machine();
        let out = handle(&mut machine, WindlassEvent::Vpn(VpnPublish::Recovered));

        assert_eq!(
            out.actions,
            vec![WindlassAction::Docker(DockerCommand::StartDependents)]
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
        AdmissionState, DownloadCandidate, ServiceStatus, SystemStateView, WindlassAction,
        WindlassCommand, WindlassConfig, WindlassEvent, WindlassMachine, WindlassPublish,
        WindlassTimer,
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
                        gluetun_anchor: "gluetun".to_string(),
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

    fn any_admission_state() -> impl Strategy<Value = AdmissionState> {
        (
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            proptest::option::of(any_vpn_port()),
            any::<bool>(),
            proptest::option::of(any::<bool>()),
        )
            .prop_map(
                |(
                    upload_health_ok,
                    unsatisfied_quota_full,
                    qbit_privacy_clean,
                    qbit_listen_port,
                    mam_healthy,
                    vpn_ip_compliant,
                )| AdmissionState {
                    upload_health_ok,
                    unsatisfied_quota_full,
                    qbit_privacy_clean,
                    qbit_listen_port,
                    mam_healthy,
                    vpn_ip_compliant,
                },
            )
    }

    fn any_admission_machine() -> impl Strategy<Value = WindlassMachine> {
        (any_windlass_machine(), any_admission_state()).prop_map(|(mut m, a)| {
            m.admission = a;
            m
        })
    }

    fn any_candidate() -> impl Strategy<Value = DownloadCandidate> {
        (
            any_mam_id(),
            any::<u64>(),
            0u32..50,
            any::<bool>(),
            0u64..=86_400,
            any::<bool>(),
            proptest::option::of(0i64..=86_400i64),
        )
            .prop_map(
                |(
                    mam_id,
                    size_bytes,
                    numfiles,
                    freeleech,
                    est_secs,
                    my_snatched,
                    window_offset,
                )| DownloadCandidate {
                    mam_id,
                    dl_url: format!("https://example.invalid/t/{}", mam_id.into_inner()),
                    size_bytes,
                    numfiles,
                    freeleech,
                    est_download_duration: Duration::from_secs(est_secs),
                    my_snatched,
                    freeleech_window_end: window_offset
                        .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs)),
                },
            )
    }

    fn any_vpn_publish() -> impl Strategy<Value = VpnPublish> {
        prop_oneof![
            Just(VpnPublish::Connected),
            Just(VpnPublish::Disconnected),
            Just(VpnPublish::Crashed),
            Just(VpnPublish::Recovered),
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
            any::<[u8; 4]>().prop_map(|b| MamPublish::AsnMismatch {
                ip: windlass_types::VpnIp(std::net::Ipv4Addr::from(b)),
            }),
            Just(MamPublish::AsnAccepted),
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

    fn any_docker_publish() -> impl Strategy<Value = windlass_docker_core::DockerPublish> {
        use windlass_docker_core::DockerPublish;
        prop_oneof![
            any::<String>().prop_map(|name| DockerPublish::ContainerCrashed { name }),
            any::<String>().prop_map(|name| DockerPublish::ContainerHealthy { name }),
            any::<String>().prop_map(|name| DockerPublish::Stopped { name }),
            any::<String>().prop_map(|name| DockerPublish::Started {
                name,
                started_at: chrono::Utc::now(),
            }),
            (any::<String>(), any::<String>())
                .prop_map(|(name, path)| DockerPublish::LogsDumped { name, path }),
            (any::<String>(), 0i64..=3_600i64, 0i64..=3_600i64).prop_map(|(name, dep, healthy)| {
                DockerPublish::DependentNetworkUntrusted {
                    name,
                    dependent_started_at: chrono::Utc::now() - chrono::Duration::seconds(dep),
                    gluetun_healthy_since: chrono::Utc::now() - chrono::Duration::seconds(healthy),
                }
            }),
            any::<String>().prop_map(|name| DockerPublish::DependentNetworkTrusted { name }),
            (any::<u32>(), any::<u32>())
                .prop_map(|(window_count, max)| DockerPublish::RestartStorm { window_count, max }),
        ]
    }

    fn any_windlass_event() -> impl Strategy<Value = WindlassEvent> {
        prop_oneof![
            Just(WindlassEvent::Init),
            any_vpn_publish().prop_map(WindlassEvent::Vpn),
            any_qbit_publish().prop_map(WindlassEvent::Qbit),
            any_mam_publish().prop_map(WindlassEvent::Mam),
            any_disk_publish().prop_map(WindlassEvent::Disk),
            any_docker_publish().prop_map(WindlassEvent::Docker),
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

        // DOM-17 [safety] (§29): `WindlassCommand::TryAddTorrent` emits
        // exactly one `Qbit(QbitCommand::AddTorrent)` action iff every
        // admission gate holds.  Total invariant: tested over fully-arbitrary
        // admission state and candidate.
        #[test]
        fn try_add_torrent_emits_add_iff_admit_ok(
            mut machine in any_admission_machine(),
            candidate in any_candidate(),
        ) {
            let now = chrono::Utc::now();
            let pre_admit = machine.admit(&candidate, now);
            let out = machine.handle_command(
                Instant::now(),
                WindlassCommand::TryAddTorrent { candidate: candidate.clone() },
            );
            let add_count = out.actions.iter().filter(|a| matches!(
                a,
                WindlassAction::Qbit(QbitCommand::AddTorrent { .. })
            )).count();
            // admit() is pure (`&self`), so the pre-call result agrees with
            // what the handler sees.  Use a tiny tolerance for the
            // freeleech-window check, which depends on `now`.
            let _ = pre_admit;
            let post_admit = machine.admit(&candidate, chrono::Utc::now());
            if post_admit.is_ok() {
                prop_assert_eq!(add_count, 1,
                    "DOM-17: every-gate-pass must emit exactly one AddTorrent");
            } else {
                prop_assert_eq!(add_count, 0,
                    "DOM-18: any-gate-fail must emit zero AddTorrent");
            }
        }

        // DOM-19 [safety] (§29): when admission is blocked, the handler emits
        // exactly one `Activity` publish (the human-readable failure list)
        // and zero AddTorrent actions.  Total invariant.
        #[test]
        fn try_add_torrent_blocked_emits_one_activity(
            mut machine in any_admission_machine(),
            candidate in any_candidate(),
        ) {
            let now = chrono::Utc::now();
            let will_block = machine.admit(&candidate, now).is_err();
            let out = machine.handle_command(
                Instant::now(),
                WindlassCommand::TryAddTorrent { candidate },
            );
            if will_block {
                let activity_count = out.publish.iter()
                    .filter(|p| matches!(p, WindlassPublish::Activity { .. }))
                    .count();
                prop_assert_eq!(activity_count, 1,
                    "DOM-19: blocked admission must publish exactly one Activity");
                let add_count = out.actions.iter().filter(|a| matches!(
                    a,
                    WindlassAction::Qbit(QbitCommand::AddTorrent { .. })
                )).count();
                prop_assert_eq!(add_count, 0,
                    "DOM-19: blocked admission emits zero AddTorrent");
            }
        }

        // DOM-20 [safety] (§30): `Mam(AsnMismatch { ip })` emits exactly one
        // `Db(RecordAlert{Critical})`, one `Db(RecordActivity)`, and one
        // `Activity` publish; admission flips to `Some(false)`.  Total
        // invariant.
        #[test]
        fn asn_mismatch_emits_one_critical_alert_and_one_activity(
            mut machine in any_windlass_machine(),
            ip_bytes in any::<[u8; 4]>(),
        ) {
            use windlass_db_core::{AlertRecord, DbCommand};
            use windlass_types::{AlertPriority, VpnIp};

            let ip = VpnIp(std::net::Ipv4Addr::from(ip_bytes));
            let out = machine.handle(
                Instant::now(),
                Timed::now(WindlassEvent::Mam(MamPublish::AsnMismatch { ip })),
            );

            let critical_count = out.actions.iter().filter(|a| matches!(
                a,
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    priority: AlertPriority::Critical,
                    ..
                }))
            )).count();
            prop_assert_eq!(critical_count, 1,
                "DOM-20: AsnMismatch must emit one RecordAlert(Critical)");

            let activity_db_count = out.actions.iter().filter(|a| matches!(
                a, WindlassAction::Db(DbCommand::RecordActivity(_))
            )).count();
            prop_assert_eq!(activity_db_count, 1,
                "DOM-20: must emit one RecordActivity");

            let activity_count = out.publish.iter().filter(|p| matches!(
                p, WindlassPublish::Activity { .. }
            )).count();
            prop_assert_eq!(activity_count, 1,
                "DOM-20: must publish exactly one Activity");

            prop_assert_eq!(machine.admission().vpn_ip_compliant, Some(false));
        }

        // DOM-21 [safety] (§31): Vpn(PublicIpMismatch) emits exactly one
        // RecordAlert(Critical) + RecordActivity + Activity publish, and
        // flips admission.vpn_ip_compliant to Some(false).  Total.
        #[test]
        fn public_ip_mismatch_emits_critical_alert_and_blocks_admission(
            mut machine in any_windlass_machine(),
            file_ip_bytes in any::<[u8; 4]>(),
            verified_ip_bytes in any::<[u8; 4]>(),
        ) {
            use windlass_db_core::{AlertRecord, DbCommand};
            use windlass_types::{AlertPriority, VpnIp};

            let file_ip = VpnIp(std::net::Ipv4Addr::from(file_ip_bytes));
            let verified_ip = VpnIp(std::net::Ipv4Addr::from(verified_ip_bytes));
            let out = machine.handle(
                Instant::now(),
                Timed::now(WindlassEvent::Vpn(VpnPublish::PublicIpMismatch {
                    file_ip,
                    verified_ip,
                    source: windlass_vpn_core::VerificationSource::IfConfigCo,
                })),
            );

            let critical_count = out.actions.iter().filter(|a| matches!(
                a,
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    priority: AlertPriority::Critical,
                    ..
                }))
            )).count();
            prop_assert_eq!(critical_count, 1);

            let activity_count = out.publish.iter().filter(|p| matches!(
                p, WindlassPublish::Activity { .. }
            )).count();
            prop_assert_eq!(activity_count, 1);

            prop_assert_eq!(machine.admission().vpn_ip_compliant, Some(false));
        }

        // DOM-22 [safety] (§31): Vpn(PublicIpVerificationDegraded) emits
        // exactly one RecordAlert(Warning) + RecordActivity + Activity, but
        // does NOT block admission (Gluetun's file is still the source of
        // truth).  Total.
        #[test]
        fn public_ip_verification_degraded_emits_warning_only(
            mut machine in any_windlass_machine(),
            consecutive_failures in any::<u32>(),
            last_reason in any::<String>(),
        ) {
            use windlass_db_core::{AlertRecord, DbCommand};
            use windlass_types::AlertPriority;

            let pre_compliant = machine.admission().vpn_ip_compliant;
            let out = machine.handle(
                Instant::now(),
                Timed::now(WindlassEvent::Vpn(VpnPublish::PublicIpVerificationDegraded {
                    consecutive_failures,
                    last_reason,
                })),
            );

            let warning_count = out.actions.iter().filter(|a| matches!(
                a,
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    priority: AlertPriority::Warning,
                    ..
                }))
            )).count();
            prop_assert_eq!(warning_count, 1);
            // Verification failure must not flip the admission gate.
            prop_assert_eq!(machine.admission().vpn_ip_compliant, pre_compliant);
        }

        // DOM-23 [safety] (§33): Vpn(PublicIpMismatch{source:MamJsonIp})
        // emits the same Critical+Activity shape as DOM-21, just with a
        // source-named title.  Admission still flips to Some(false).
        #[test]
        fn mam_source_public_ip_mismatch_emits_critical_alert(
            mut machine in any_windlass_machine(),
            file_ip_bytes in any::<[u8; 4]>(),
            verified_ip_bytes in any::<[u8; 4]>(),
        ) {
            use windlass_db_core::{AlertRecord, DbCommand};
            use windlass_types::{AlertPriority, VpnIp};

            let file_ip = VpnIp(std::net::Ipv4Addr::from(file_ip_bytes));
            let verified_ip = VpnIp(std::net::Ipv4Addr::from(verified_ip_bytes));
            let out = machine.handle(
                Instant::now(),
                Timed::now(WindlassEvent::Vpn(VpnPublish::PublicIpMismatch {
                    file_ip,
                    verified_ip,
                    source: windlass_vpn_core::VerificationSource::MamJsonIp,
                })),
            );

            let critical_count = out.actions.iter().filter(|a| matches!(
                a,
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    priority: AlertPriority::Critical,
                    ..
                }))
            )).count();
            prop_assert_eq!(critical_count, 1);

            let activity_count = out.publish.iter().filter(|p| matches!(
                p, WindlassPublish::Activity { .. }
            )).count();
            prop_assert_eq!(activity_count, 1);

            prop_assert_eq!(machine.admission().vpn_ip_compliant, Some(false));
        }

        // DOM-24 [safety] (§33): Vpn(MamIpVerificationDegraded) emits one
        // Warning alert + Activity, does NOT block admission.  Mirrors
        // DOM-22 for the MAM source.
        #[test]
        fn mam_ip_verification_degraded_emits_warning_only(
            mut machine in any_windlass_machine(),
            consecutive_failures in any::<u32>(),
            last_reason in any::<String>(),
        ) {
            use windlass_db_core::{AlertRecord, DbCommand};
            use windlass_types::AlertPriority;

            let pre_compliant = machine.admission().vpn_ip_compliant;
            let out = machine.handle(
                Instant::now(),
                Timed::now(WindlassEvent::Vpn(VpnPublish::MamIpVerificationDegraded {
                    consecutive_failures,
                    last_reason,
                })),
            );

            let warning_count = out.actions.iter().filter(|a| matches!(
                a,
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    priority: AlertPriority::Warning,
                    ..
                }))
            )).count();
            prop_assert_eq!(warning_count, 1);
            prop_assert_eq!(machine.admission().vpn_ip_compliant, pre_compliant);
        }

        // DOM-25 [safety] (§35): DependentNetworkUntrusted emits one
        // Critical alert + RecordActivity + Activity, and flips
        // admission to Some(false).  Total.
        #[test]
        fn dependent_network_untrusted_emits_critical_and_blocks(
            mut machine in any_windlass_machine(),
            name in any::<String>(),
            healthy_offset in 0i64..=3_600i64,
            dep_offset in 0i64..=3_600i64,
        ) {
            use windlass_db_core::{AlertRecord, DbCommand};
            use windlass_types::AlertPriority;

            let healthy = chrono::Utc::now() - chrono::Duration::seconds(healthy_offset);
            let started = chrono::Utc::now() - chrono::Duration::seconds(dep_offset + healthy_offset);
            let out = machine.handle(Instant::now(), Timed::now(
                WindlassEvent::Docker(windlass_docker_core::DockerPublish::DependentNetworkUntrusted {
                    name,
                    dependent_started_at: started,
                    gluetun_healthy_since: healthy,
                }),
            ));
            let critical = out.actions.iter().filter(|a| matches!(
                a,
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    priority: AlertPriority::Critical,
                    ..
                }))
            )).count();
            prop_assert_eq!(critical, 1);
            prop_assert_eq!(machine.admission().vpn_ip_compliant, Some(false));
        }

        // DOM-26 [safety] (§35): RestartStorm → Critical + admission
        // block.
        #[test]
        fn restart_storm_emits_critical_and_blocks(
            mut machine in any_windlass_machine(),
            window_count in any::<u32>(),
            max in any::<u32>(),
        ) {
            use windlass_db_core::{AlertRecord, DbCommand};
            use windlass_types::AlertPriority;

            let out = machine.handle(Instant::now(), Timed::now(
                WindlassEvent::Docker(windlass_docker_core::DockerPublish::RestartStorm { window_count, max }),
            ));
            let critical = out.actions.iter().filter(|a| matches!(
                a,
                WindlassAction::Db(DbCommand::RecordAlert(AlertRecord {
                    priority: AlertPriority::Critical,
                    ..
                }))
            )).count();
            prop_assert_eq!(critical, 1);
            prop_assert_eq!(machine.admission().vpn_ip_compliant, Some(false));
        }
    }
}
