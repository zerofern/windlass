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
                    let event = match client.authenticate().await {
                        windlass_core::events::Event::QbitAuthSuccess { cookie, .. } => {
                            QbitEvent::AuthSucceeded { cookie }
                        }
                        windlass_core::events::Event::QbitAuthFailed { .. } => {
                            QbitEvent::AuthFailed {
                                reason: "credentials rejected".to_string(),
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
                        },
                    );
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
                    client.force_resume_torrent(&cookie, &hash).await;
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
