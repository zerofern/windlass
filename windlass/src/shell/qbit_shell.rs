use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use windlass_clients::qbit::{QbitClient, QbitTorrentState};
use windlass_machine::{Shell, Timed};
use windlass_qbit_core::{QbitAction, QbitEvent};
use windlass_types::{TorrentRecord, TorrentState};

pub struct QbitShell {
    client: QbitClient,
}

impl Shell for QbitShell {
    type Config = QbitClient;
    type Event = QbitEvent;
    type Action = QbitAction;

    async fn new(client: Self::Config, _event_tx: UnboundedSender<Timed<QbitEvent>>) -> Self {
        Self { client }
    }

    // Each action arm is a small `tokio::spawn` block; the function is long because
    // the action set is large, not because any single arm is complex.
    #[allow(clippy::too_many_lines)]
    fn dispatch(&mut self, action: QbitAction, event_tx: &UnboundedSender<Timed<QbitEvent>>) {
        match action {
            QbitAction::Login => {
                let client = self.client.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    // §36 step 3: credentials rejection (`QbitAuthFailed`)
                    // is a configuration error and routes to `AuthRejected`
                    // so domain can fire a Critical alert.  Transient
                    // failures (connection refused, API errors, unexpected
                    // responses) route to `AuthFailed` for silent retry.
                    let event = match client.authenticate().await {
                        windlass_core::events::Event::QbitAuthSuccess { cookie, .. } => {
                            QbitEvent::AuthSucceeded { cookie }
                        }
                        windlass_core::events::Event::QbitAuthFailed { .. } => {
                            QbitEvent::AuthRejected {
                                reason: "qBittorrent rejected credentials".to_string(),
                            }
                        }
                        windlass_core::events::Event::QbitConnectionRefused { .. } => {
                            QbitEvent::AuthFailed {
                                reason: "connection refused".to_string(),
                            }
                        }
                        other => QbitEvent::AuthFailed {
                            reason: format!("unexpected response: {other:?}"),
                        },
                    };
                    let _ = tx.send(Timed::now(event));
                });
            }
            QbitAction::ReadPreferences { cookie } => {
                let client = self.client.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let event = client.get_preferences(&cookie).await.map_or_else(
                        || QbitEvent::PreferencesFailed {
                            reason: "failed to fetch preferences".to_string(),
                        },
                        |prefs| QbitEvent::PreferencesRead {
                            listen_port: prefs.listen_port,
                            dht: prefs.dht,
                            pex: prefs.pex,
                            lsd: prefs.lsd,
                            max_active_torrents: prefs.max_active_torrents,
                        },
                    );
                    let _ = tx.send(Timed::now(event));
                });
            }
            QbitAction::DisableBannedPrivacySettings { cookie } => {
                let client = self.client.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let event = if client.set_private_tracker_safe_prefs(&cookie).await {
                        QbitEvent::PrivacySettingsDisabled
                    } else {
                        QbitEvent::PrivacySettingsDisableFailed {
                            reason: "failed to set private tracker safe prefs".to_string(),
                        }
                    };
                    let _ = tx.send(Timed::now(event));
                });
            }
            QbitAction::SetListenPort { cookie, port } => {
                let client = self.client.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let event = match client.sync_port(&cookie, port).await {
                        windlass_core::events::Event::QbitPortSyncSuccess { .. } => {
                            QbitEvent::ListenPortSet { port }
                        }
                        windlass_core::events::Event::QbitPortSyncFailed { code, .. } => {
                            QbitEvent::ListenPortSetFailed {
                                port,
                                reason: format!("port sync failed (status {})", code.0),
                            }
                        }
                        other => QbitEvent::ListenPortSetFailed {
                            port,
                            reason: format!("unexpected response: {other:?}"),
                        },
                    };
                    let _ = tx.send(Timed::now(event));
                });
            }
            QbitAction::ListTorrents { cookie } => {
                let client = self.client.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let details = client.list_torrent_details(&cookie).await;
                    let torrents: Vec<TorrentRecord> = details
                        .into_iter()
                        .map(|d| TorrentRecord {
                            hash: d.hash,
                            downloaded_bytes: d.downloaded_bytes,
                            seed_time: Duration::from_secs(d.seeding_time_secs),
                            state: qbit_state_to_torrent_state(d.state),
                            mam_id: d.mam_id,
                        })
                        .collect();
                    let _ = tx.send(Timed::now(QbitEvent::TorrentsListed { torrents }));
                });
            }
            QbitAction::DeleteTorrent { cookie, hash } => {
                let client = self.client.clone();
                tokio::spawn(async move {
                    client.delete_torrent(&cookie, &hash).await;
                });
            }
            QbitAction::SetAllFilesPriority { cookie, hash } => {
                let client = self.client.clone();
                tokio::spawn(async move {
                    client.set_all_files_priority(&cookie, &hash).await;
                });
            }
            QbitAction::PauseTorrent { cookie, hash } => {
                let client = self.client.clone();
                tokio::spawn(async move {
                    client.pause_torrent(&cookie, &hash).await;
                });
            }
            QbitAction::ResumeTorrent { cookie, hash } => {
                let client = self.client.clone();
                tokio::spawn(async move {
                    client.resume_torrent(&cookie, &hash).await;
                });
            }
            QbitAction::ForceResumeTorrent { cookie, hash } => {
                let client = self.client.clone();
                tokio::spawn(async move {
                    client.force_resume_torrent(&cookie, &hash).await;
                });
            }
            // §29 / DOM-17 / §36 step 5: the domain has authorised this
            // add (composite admission predicate); MAM core has already
            // fetched the `.torrent` bytes.  Call qBittorrent's add-API
            // and report the result back via TorrentAdded /
            // TorrentAddFailed events.
            QbitAction::AddTorrent {
                cookie,
                mam_id,
                bytes,
            } => {
                let client = self.client.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let event = match client.add_torrent(&cookie, bytes).await {
                        Some(hash) => QbitEvent::TorrentAdded { mam_id, hash },
                        None => QbitEvent::TorrentAddFailed {
                            mam_id,
                            reason: "qBittorrent rejected the torrent add request".to_string(),
                        },
                    };
                    let _ = tx.send(Timed::now(event));
                });
            }
            QbitAction::ScheduleTimer { timer, after } => {
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let scheduled_at = std::time::Instant::now() + after;
                    tokio::time::sleep(after).await;
                    let _ = tx.send(Timed::new(scheduled_at, QbitEvent::TimerFired(timer)));
                });
            }
        }
    }
}

fn qbit_state_to_torrent_state(s: QbitTorrentState) -> TorrentState {
    match s {
        QbitTorrentState::Downloading => TorrentState::Downloading,
        QbitTorrentState::StalledDownloading => TorrentState::StalledDownloading,
        QbitTorrentState::Uploading => TorrentState::Uploading,
        QbitTorrentState::StalledUploading => TorrentState::StalledUploading,
        QbitTorrentState::ForcedUpload => TorrentState::ForcedUpload,
        QbitTorrentState::PausedDownloading => TorrentState::PausedDownloading,
        QbitTorrentState::PausedUploading => TorrentState::PausedUploading,
        QbitTorrentState::Error => TorrentState::Error,
        QbitTorrentState::Other(s) => TorrentState::Other(s),
    }
}
