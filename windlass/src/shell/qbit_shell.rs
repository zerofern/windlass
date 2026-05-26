use tokio::sync::mpsc::UnboundedSender;

use windlass_clients::qbit::QbitClient;
use windlass_machine::{Shell, Timed};
use windlass_qbit_core::{QbitAction, QbitEvent};
use windlass_types::TorrentHash;

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
                        windlass_core::events::Event::QbitAuthFailed { .. } => QbitEvent::AuthFailed {
                            reason: "credentials rejected".to_string(),
                        },
                        windlass_core::events::Event::QbitConnectionRefused { .. } => {
                            QbitEvent::AuthFailed {
                                reason: "connection refused".to_string(),
                            }
                        }
                        other => QbitEvent::AuthFailed {
                            reason: format!("unexpected response: {:?}", other),
                        },
                    };
                    let _ = tx.send(Timed::now(event));
                });
            }
            QbitAction::ReadPreferences { cookie } => {
                let client = self.client.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let event = match client.get_preferences(&cookie).await {
                        Some(prefs) => QbitEvent::PreferencesRead {
                            listen_port: prefs.listen_port,
                        },
                        None => QbitEvent::PreferencesFailed {
                            reason: "failed to fetch preferences".to_string(),
                        },
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
                            reason: format!("unexpected response: {:?}", other),
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
                    let hashes: Vec<TorrentHash> =
                        details.into_iter().map(|d| d.hash).collect();
                    let _ = tx.send(Timed::now(QbitEvent::TorrentsListed { hashes }));
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
                    tokio::time::sleep(after).await;
                    let _ = tx.send(Timed::now(QbitEvent::TimerFired(timer)));
                });
            }
        }
    }
}
