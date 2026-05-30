use std::time::Duration;

use windlass_core::events::Event;
use windlass_domain_core::WindlassEvent;
use windlass_mam_core::MamEvent;
use windlass_qbit_core::QbitEvent;
use windlass_types::{MamStatus, TorrentRecord, TorrentState, VpnPort, WakeupId};
use windlass_vpn_core::VpnEvent;

pub(super) enum ServiceEvent {
    Domain(WindlassEvent),
    Vpn(VpnEvent),
    Qbit(QbitEvent),
    Mam(MamEvent),
}

#[allow(clippy::too_many_lines)]
pub(super) fn legacy_to_service_events(
    event: &Event,
    forwarded_port: Option<VpnPort>,
) -> Vec<ServiceEvent> {
    match event {
        Event::Init {
            is_gluetun_healthy,
            port_files,
            ..
        } => {
            let mut events = vec![
                ServiceEvent::Domain(WindlassEvent::Init),
                ServiceEvent::Vpn(VpnEvent::Init),
                ServiceEvent::Qbit(QbitEvent::Init),
                ServiceEvent::Mam(MamEvent::Init),
            ];
            if *is_gluetun_healthy {
                events.push(ServiceEvent::Vpn(VpnEvent::ContainerHealthy));
            }
            if let Ok((_, port)) = port_files {
                events.push(ServiceEvent::Vpn(VpnEvent::PortFileChanged { port: *port }));
            }
            events
        }
        Event::DockerGluetunHealthy { .. } => vec![ServiceEvent::Vpn(VpnEvent::ContainerHealthy)],
        Event::DockerGluetunDied { .. } => vec![ServiceEvent::Vpn(VpnEvent::ContainerUnhealthy)],
        Event::PortFileReadResult { result, .. } => result.as_ref().map_or_else(
            |_| {
                vec![ServiceEvent::Vpn(VpnEvent::StateReadFailed {
                    reason: "port files unavailable".to_string(),
                })]
            },
            |(_, port)| vec![ServiceEvent::Vpn(VpnEvent::PortFileChanged { port: *port })],
        ),
        Event::QbitAuthSuccess { cookie, .. } => {
            vec![ServiceEvent::Qbit(QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            })]
        }
        Event::QbitAuthFailed { .. } => vec![ServiceEvent::Qbit(QbitEvent::AuthFailed {
            reason: "qBittorrent rejected credentials".to_string(),
        })],
        Event::QbitConnectionRefused { .. } => vec![ServiceEvent::Qbit(QbitEvent::AuthFailed {
            reason: "qBittorrent connection refused".to_string(),
        })],
        Event::QbitApiError { code, .. } => vec![ServiceEvent::Qbit(QbitEvent::AuthFailed {
            reason: format!("qBittorrent API error {}", code.0),
        })],
        Event::QbitPortSyncSuccess { .. } => forwarded_port.map_or_else(Vec::new, |port| {
            vec![ServiceEvent::Qbit(QbitEvent::ListenPortSet { port })]
        }),
        Event::QbitPortSyncFailed { code, .. } => forwarded_port.map_or_else(
            || {
                vec![ServiceEvent::Qbit(QbitEvent::PreferencesFailed {
                    reason: format!("qBittorrent port sync failed {}", code.0),
                })]
            },
            |port| {
                vec![ServiceEvent::Qbit(QbitEvent::ListenPortSetFailed {
                    port,
                    reason: format!("qBittorrent port sync failed {}", code.0),
                })]
            },
        ),
        Event::MamUpdateSuccess {
            registered_ip,
            registered_asn,
            registered_as,
            ..
        } => {
            if forwarded_port.is_some() {
                vec![ServiceEvent::Mam(MamEvent::SeedboxUpdated {
                    registered_ip: *registered_ip,
                    registered_asn: *registered_asn,
                    registered_as: registered_as.clone(),
                })]
            } else {
                Vec::new()
            }
        }
        Event::MamAsnMismatch { ip, .. } => vec![ServiceEvent::Mam(MamEvent::StatusFailed {
            reason: format!("MAM ASN mismatch for {}", ip.0),
        })],
        // §28: legacy MamUnreachable maps to the new MAM-core Unreachable
        // event so the new MAM machine publishes the distinct Unreachable
        // signal (MAM-11) instead of a generic StatusFailed.
        Event::MamUnreachable { reason, .. } => vec![ServiceEvent::Mam(MamEvent::Unreachable {
            reason: reason.clone(),
        })],
        Event::MamStatusObserved { status, .. } => match status {
            MamStatus::Connectable => vec![
                ServiceEvent::Mam(MamEvent::AuthSucceeded),
                ServiceEvent::Mam(MamEvent::StatusFetched {
                    connectable: true,
                    seedbox_port: forwarded_port,
                    // Legacy bridge: ratio/upload_credit_bytes are not carried by
                    // the legacy event.  Default to 0.0/0 (fail-closed per §26):
                    // the upload-health gate will fire until the new shell path
                    // (MamShell::FetchStatus → fetch_mam_status) provides real values.
                    ratio: 0.0,
                    upload_credit_bytes: 0,
                }),
            ],
            MamStatus::NotConnectable => vec![ServiceEvent::Mam(MamEvent::StatusFetched {
                connectable: false,
                seedbox_port: forwarded_port,
                ratio: 0.0,
                upload_credit_bytes: 0,
            })],
            MamStatus::Unreachable => vec![ServiceEvent::Mam(MamEvent::StatusFailed {
                reason: "MAM unreachable".to_string(),
            })],
        },
        Event::MamRateLimitViolation { .. } => vec![ServiceEvent::Mam(MamEvent::RateLimited {
            retry_after: Duration::from_secs(1),
        })],
        Event::QbitTorrentDetailsReceived { torrents, .. } => {
            vec![ServiceEvent::Qbit(QbitEvent::TorrentsListed {
                torrents: torrents
                    .iter()
                    .map(|torrent| TorrentRecord {
                        hash: torrent.hash.clone(),
                        downloaded_bytes: torrent.downloaded_bytes,
                        seed_time: Duration::from_secs(torrent.seeding_time_secs),
                        state: legacy_torrent_state_to_torrent_state(&torrent.state),
                        mam_id: torrent.mam_id,
                    })
                    .collect(),
            })]
        }
        Event::Wakeup { id, .. } => match id {
            WakeupId::QbitAuthRetry => vec![ServiceEvent::Qbit(QbitEvent::TimerFired(
                windlass_qbit_core::QbitTimer::AuthRetry,
            ))],
            WakeupId::QbitSyncRetry => vec![ServiceEvent::Qbit(QbitEvent::TimerFired(
                windlass_qbit_core::QbitTimer::SyncRetry,
            ))],
            WakeupId::Heartbeat => vec![ServiceEvent::Mam(MamEvent::TimerFired(
                windlass_mam_core::MamTimer::StatusRetry,
            ))],
            WakeupId::RetryPortRead => vec![ServiceEvent::Vpn(VpnEvent::TimerFired(
                windlass_vpn_core::VpnTimer::PortReadRetry,
            ))],
            // DomainSnapshot is now scheduled internally by the DomainShell; the
            // WakeupId variant is retained for compatibility but produces no service event.
            WakeupId::DomainSnapshot
            | WakeupId::DiskCheck
            | WakeupId::TorrentCheck
            | WakeupId::CompliancePoll => Vec::new(),
        },
        Event::QbitPreferencesReceived { listen_port, .. } => {
            vec![ServiceEvent::Qbit(QbitEvent::PreferencesRead {
                listen_port: *listen_port,
                // Legacy bridge: the old event does not carry privacy settings.
                // Default to false (safe: no spurious disable action emitted).
                dht: false,
                pex: false,
                lsd: false,
                // Legacy bridge: default to u32::MAX ("no limit") so shadow-mode
                // events from the old code path never trigger queue orchestration.
                // The new path populates this from `QbitPreferences.max_active_torrents`.
                max_active_torrents: u32::MAX,
            })]
        }
        Event::QbitPreferencesFailed { reason, .. } => {
            vec![ServiceEvent::Qbit(QbitEvent::PreferencesFailed {
                reason: reason.clone(),
            })]
        }
        Event::DiskSpaceObserved { .. }
        | Event::NewTorrentsObserved { .. }
        | Event::LogsDumped { .. }
        | Event::DeleteTorrentRequested { .. }
        | Event::ManualDownloadRequested { .. }
        | Event::TorrentAddedToQbit { .. }
        | Event::TorrentAddFailed { .. } => Vec::new(),
    }
}

fn legacy_torrent_state_to_torrent_state(s: &windlass_core::torrent::TorrentState) -> TorrentState {
    use windlass_core::torrent::TorrentState as L;
    match s {
        L::Downloading => TorrentState::Downloading,
        L::StalledDownloading => TorrentState::StalledDownloading,
        L::Uploading => TorrentState::Uploading,
        L::StalledUploading => TorrentState::StalledUploading,
        L::ForcedUpload => TorrentState::ForcedUpload,
        L::PausedDownloading => TorrentState::PausedDownloading,
        L::PausedUploading => TorrentState::PausedUploading,
        L::Error => TorrentState::Error,
        L::Other => TorrentState::Other("other".to_string()),
    }
}
