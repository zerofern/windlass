use chrono::Utc;
use windlass_clients::qbit::{QbitTorrentDetails, QbitTorrentState};
use windlass_core::events::Event;
use windlass_core::torrent::{TorrentRecord, TorrentState};
use windlass_db::TorrentRow;
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
            if let Some(prefs) = qbit.get_preferences(&cookie).await {
                causal_tx
                    .send(Event::QbitPreferencesReceived {
                        at: Utc::now(),
                        max_active_torrents: prefs.torrents,
                        max_active_downloads: prefs.downloads,
                        max_active_uploads: prefs.uploads,
                    })
                    .await;
            }
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
        let pool = self.db_pool.clone();
        tokio::spawn(async move {
            for record in records {
                let row = TorrentRow {
                    hash: record.hash.0,
                    book_id: None,
                    mam_id: record.mam_id.map(|id| {
                        // MAM IDs are u64 but the column is i64; clamp to i64::MAX on overflow.
                        i64::try_from(id.0).unwrap_or(i64::MAX)
                    }),
                    name: record.name.0,
                    state: record.state.as_db_str().to_string(),
                    seeding_time_secs: i64::try_from(record.seeding_time_secs).unwrap_or(i64::MAX),
                    downloaded_bytes: i64::try_from(record.downloaded_bytes).unwrap_or(i64::MAX),
                    seen_at: record.seen_at.to_rfc3339(),
                    added_at: String::new(),
                };
                if let Err(e) = windlass_db::torrents::upsert(&pool, &row).await {
                    tracing::warn!("Failed to upsert torrent {}: {e}", row.hash);
                }
            }
        });
    }

    pub(super) fn blacklist_mam_id(&self, mam_id: MamTorrentId) {
        let pool = self.db_pool.clone();
        tokio::spawn(async move {
            if let Err(e) = windlass_db::download_queue::blacklist(&pool, mam_id).await {
                tracing::warn!("Failed to blacklist mam_id {}: {e}", mam_id.0);
            }
        });
    }

    pub(super) fn write_event(
        &self,
        source: String,
        action: String,
        book_id: Option<i64>,
        detail: Option<String>,
    ) {
        let pool = self.db_pool.clone();
        tokio::spawn(async move {
            if let Err(e) =
                windlass_db::events::insert(&pool, &source, &action, book_id, detail.as_deref())
                    .await
            {
                tracing::warn!("Failed to write event {action}: {e}");
            }
        });
    }
}

// ── Conversion helpers ────────────────────────────────────────────────────────

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
