use chrono::Utc;
use windlass_clients::qbit::{QbitTorrentDetails, QbitTorrentState};
use windlass_core::events::Event;
use windlass_core::torrent::{TorrentRecord, TorrentState};
use windlass_db::actor::PostgresDbActor;
use windlass_db_core::{
    ActivityRecord, ActivitySource, BookId, DbCommand, DbEvent, DownloadStateChange,
    DownloadStatus, TorrentStateRecord,
};
use windlass_debug::CausalTx;
use windlass_types::{AuthCookie, MamTorrentId, TorrentHash};

use super::ShellContext;

impl ShellContext<'_> {
    pub(super) fn fetch_torrent_details(&self, cookie: AuthCookie, causal_tx: CausalTx) {
        let qbit = self.qbit.clone();
        tokio::spawn(causal_tx.run(move |causal_tx| async move {
            let raw = qbit.list_torrent_details(&cookie).await;
            let torrents = raw.into_iter().map(qbit_details_to_record).collect();
            causal_tx
                .send(Event::QbitTorrentDetailsReceived {
                    at: Utc::now(),
                    torrents,
                })
                .await;
        }));
    }

    pub(super) fn fetch_qbit_preferences(&self, cookie: AuthCookie, causal_tx: CausalTx) {
        let qbit = self.qbit.clone();
        tokio::spawn(causal_tx.run(move |causal_tx| async move {
            let event = qbit.get_preferences(&cookie).await.map_or_else(
                || Event::QbitPreferencesFailed {
                    at: Utc::now(),
                    reason: "qBittorrent preferences unavailable".to_string(),
                },
                |prefs| Event::QbitPreferencesReceived {
                    at: Utc::now(),
                    max_active_torrents: prefs.torrents,
                    max_active_downloads: prefs.downloads,
                    max_active_uploads: prefs.uploads,
                    listen_port: prefs.listen_port,
                },
            );
            causal_tx.send(event).await;
        }));
    }

    pub(super) fn pause_torrent(&self, hash: TorrentHash, cookie: AuthCookie) {
        let qbit = self.qbit.clone();
        tokio::spawn(async move {
            qbit.pause_torrent(&cookie, &hash).await;
        });
    }

    pub(super) fn force_resume_torrent(&self, hash: TorrentHash, cookie: AuthCookie) {
        let qbit = self.qbit.clone();
        tokio::spawn(async move {
            qbit.force_resume_torrent(&cookie, &hash).await;
        });
    }

    pub(super) fn delete_torrent(&self, hash: TorrentHash, cookie: AuthCookie) {
        let qbit = self.qbit.clone();
        tokio::spawn(async move {
            qbit.delete_torrent(&cookie, &hash).await;
        });
    }

    pub(super) fn set_all_files_priority(&self, hash: TorrentHash, cookie: AuthCookie) {
        let qbit = self.qbit.clone();
        tokio::spawn(async move {
            qbit.set_all_files_priority(&cookie, &hash).await;
        });
    }

    pub(super) fn upsert_torrent_records(&self, records: Vec<TorrentRecord>) {
        let actor = PostgresDbActor::new(self.db_pool.clone());
        tokio::spawn(async move {
            for record in records {
                let hash = record.hash.clone();
                let state = torrent_state_record(&record.state);
                let event = actor
                    .handle(DbCommand::UpsertTorrent(windlass_db_core::TorrentRecord {
                        hash: record.hash,
                        book_id: None,
                        mam_id: record.mam_id,
                        name: record.name.0,
                        state,
                        seeding_time_secs: i64::try_from(record.seeding_time_secs)
                            .unwrap_or(i64::MAX),
                        downloaded_bytes: i64::try_from(record.downloaded_bytes)
                            .unwrap_or(i64::MAX),
                        seen_at: record.seen_at,
                    }))
                    .await;
                if let DbEvent::Failed(error) = event {
                    tracing::warn!("Failed to upsert torrent {}: {}", hash.0, error.message);
                }
            }
        });
    }

    pub(super) fn blacklist_mam_id(&self, mam_id: MamTorrentId) {
        let actor = PostgresDbActor::new(self.db_pool.clone());
        tokio::spawn(async move {
            let event = actor
                .handle(DbCommand::MarkDownloadState(DownloadStateChange {
                    mam_id,
                    status: DownloadStatus::Blacklisted,
                }))
                .await;
            if let DbEvent::Failed(error) = event {
                tracing::warn!("Failed to blacklist mam_id {}: {}", mam_id.0, error.message);
            }
        });
    }

    pub(super) fn write_activity(
        &self,
        source: String,
        action: String,
        book_id: Option<i64>,
        detail: Option<String>,
    ) {
        let actor = PostgresDbActor::new(self.db_pool.clone());
        tokio::spawn(async move {
            let event = actor
                .handle(DbCommand::RecordActivity(ActivityRecord {
                    at: Utc::now(),
                    source: ActivitySource::Shell,
                    action,
                    book_id: book_id.map(BookId),
                    detail,
                    metadata: serde_json::json!({ "legacy_source": source }),
                }))
                .await;
            if let DbEvent::Failed(error) = event {
                tracing::warn!("Failed to write activity to DB: {}", error.message);
            }
        });
    }
}

// ── Conversion helpers ────────────────────────────────────────────────────────

fn torrent_state_record(state: &TorrentState) -> TorrentStateRecord {
    match state {
        TorrentState::Downloading => TorrentStateRecord::Downloading,
        TorrentState::StalledDownloading => TorrentStateRecord::StalledDownloading,
        TorrentState::Uploading => TorrentStateRecord::Uploading,
        TorrentState::StalledUploading => TorrentStateRecord::StalledUploading,
        TorrentState::ForcedUpload => TorrentStateRecord::ForcedUpload,
        TorrentState::PausedDownloading => TorrentStateRecord::PausedDownloading,
        TorrentState::PausedUploading => TorrentStateRecord::PausedUploading,
        TorrentState::Error => TorrentStateRecord::Error,
        TorrentState::Other => TorrentStateRecord::Unknown("other".to_string()),
    }
}

fn qbit_details_to_record(d: QbitTorrentDetails) -> TorrentRecord {
    TorrentRecord {
        hash: d.hash,
        name: d.name,
        state: qbit_state_to_core(&d.state),
        seeding_time_secs: d.seeding_time_secs,
        downloaded_bytes: d.downloaded_bytes,
        mam_id: d.mam_id,
        seen_at: Utc::now(),
    }
}

const fn qbit_state_to_core(s: &QbitTorrentState) -> TorrentState {
    match s {
        QbitTorrentState::Downloading => TorrentState::Downloading,
        QbitTorrentState::StalledDownloading => TorrentState::StalledDownloading,
        QbitTorrentState::Uploading => TorrentState::Uploading,
        QbitTorrentState::StalledUploading => TorrentState::StalledUploading,
        QbitTorrentState::ForcedUpload => TorrentState::ForcedUpload,
        QbitTorrentState::PausedDownloading => TorrentState::PausedDownloading,
        QbitTorrentState::PausedUploading => TorrentState::PausedUploading,
        QbitTorrentState::Error => TorrentState::Error,
        QbitTorrentState::Other(_) => TorrentState::Other,
    }
}
