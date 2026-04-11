use chrono::Utc;
use windlass_core::events::Event;
use windlass_debug::CausalTx;
use windlass_types::{AuthCookie, MamTorrentId};

use super::ShellContext;

impl ShellContext<'_> {
    /// Fetches a `.torrent` file from MAM and adds it to qBittorrent.
    ///
    /// Emits `TorrentAddedToQbit` on success or `TorrentAddFailed` if either
    /// the MAM fetch or the qBit add step fails.
    pub(super) fn fetch_and_add_torrent(
        &self,
        mam_id: MamTorrentId,
        cookie: AuthCookie,
        causal_tx: CausalTx,
    ) {
        let mam = self.mam.clone();
        let qbit = self.qbit.clone();
        tokio::spawn(causal_tx.run(move |causal_tx| async move {
            let Some(bytes) = mam.fetch_torrent(mam_id).await else {
                causal_tx
                    .send(Event::TorrentAddFailed {
                        at: Utc::now(),
                        mam_id,
                        reason: "Failed to fetch torrent from MAM".into(),
                    })
                    .await;
                return;
            };
            match qbit.add_torrent(&cookie, bytes).await {
                Some(hash) => {
                    causal_tx
                        .send(Event::TorrentAddedToQbit {
                            at: Utc::now(),
                            mam_id,
                            hash,
                        })
                        .await;
                }
                None => {
                    causal_tx
                        .send(Event::TorrentAddFailed {
                            at: Utc::now(),
                            mam_id,
                            reason: "Failed to add torrent to qBittorrent".into(),
                        })
                        .await;
                }
            }
        }));
    }
}
