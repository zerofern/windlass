use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use windlass_clients::mam::{MamClient, MamFetchError, MamSeedboxResult};
use windlass_machine::{KeyedTimers, Shell, Timed};
use windlass_mam_core::{MamAction, MamEvent, MamTimer};

pub struct MamShell {
    client: MamClient,
    /// Replace-semantics timers: at most one pending sleep per
    /// [`MamTimer`] id, so retry paths and the keep-alive chain can
    /// never stack duplicate self-perpetuating chains.
    timers: KeyedTimers<MamTimer>,
}

impl Shell for MamShell {
    type Config = MamClient;
    type Event = MamEvent;
    type Action = MamAction;

    async fn new(client: Self::Config, _event_tx: UnboundedSender<Timed<MamEvent>>) -> Self {
        Self {
            client,
            timers: KeyedTimers::new(),
        }
    }

    fn dispatch(&mut self, action: MamAction, event_tx: &UnboundedSender<Timed<MamEvent>>) {
        match action {
            MamAction::FetchStatus => {
                let client = self.client.clone();
                let tx = event_tx.clone();
                windlass_machine::causal::spawn(async move {
                    // §28: map the typed MamFetchError surface to the right
                    // MAM-core event so the machine can publish Unreachable
                    // vs StatusFailed vs RateLimited distinctly.
                    let event = match client.fetch_mam_status().await {
                        Ok(status) => MamEvent::StatusFetched {
                            connectable: status.connectable,
                            seedbox_port: None,
                            ratio: status.ratio,
                            upload_credit_bytes: status.upload_credit_bytes,
                        },
                        Err(MamFetchError::Unreachable(reason)) => MamEvent::Unreachable { reason },
                        Err(MamFetchError::LocalRateLimit) => MamEvent::RateLimited {
                            retry_after: Duration::from_secs(1),
                        },
                        Err(MamFetchError::StatusFailed(reason)) => {
                            MamEvent::StatusFailed { reason }
                        }
                    };
                    let _ = tx.send(Timed::from_dispatch(std::time::Instant::now(), event));
                });
            }
            MamAction::UpdateSeedbox => {
                let client = self.client.clone();
                let tx = event_tx.clone();
                windlass_machine::causal::spawn(async move {
                    // §36 step 9a: typed result direct from the client —
                    // shell maps each variant to the matching MamEvent.
                    let event = match client.update_seedbox().await {
                        MamSeedboxResult::Success {
                            registered_ip,
                            registered_asn,
                            registered_as,
                        } => MamEvent::SeedboxUpdated {
                            registered_ip,
                            registered_asn,
                            registered_as,
                        },
                        // The client reports the honest remaining
                        // window (up to 1h for the dynamic-seedbox
                        // guard).  Mapping this to a short constant
                        // (the old `1s`) made the machine retry
                        // against a closed guard once per second —
                        // a critical-alert storm.
                        MamSeedboxResult::RateLimited { retry_after } => {
                            MamEvent::RateLimited { retry_after }
                        }
                        // §30: ASN mismatch is a distinct compliance signal.
                        MamSeedboxResult::AsnMismatch { ip } => MamEvent::AsnMismatch { ip },
                        // §28: transport-level failure routes as Unreachable.
                        MamSeedboxResult::Unreachable { reason } => {
                            MamEvent::Unreachable { reason }
                        }
                        MamSeedboxResult::Failed { reason } => {
                            MamEvent::SeedboxUpdateFailed { reason }
                        }
                    };
                    let _ = tx.send(Timed::from_dispatch(std::time::Instant::now(), event));
                });
            }
            MamAction::ScheduleTimer { timer, after } => {
                self.timers.schedule(
                    timer,
                    timer.name(),
                    after,
                    event_tx,
                    MamEvent::TimerFired(timer),
                );
            }
            // §36 step 5: fetch the `.torrent` bytes for a manual-download
            // admission.  `mam_client.fetch_torrent` returns `None` on any
            // network or HTTP error; we don't have a typed error surface
            // here so the failure reason stays generic.
            MamAction::FetchTorrentBytes { mam_id } => {
                let client = self.client.clone();
                let tx = event_tx.clone();
                windlass_machine::causal::spawn(async move {
                    let event = client.fetch_torrent(mam_id).await.map_or_else(
                        || MamEvent::TorrentBytesFetchFailed {
                            mam_id,
                            reason: "MAM torrent fetch failed (network or HTTP error)".to_string(),
                        },
                        |bytes| MamEvent::TorrentBytesFetched { mam_id, bytes },
                    );
                    let _ = tx.send(Timed::from_dispatch(std::time::Instant::now(), event));
                });
            }
        }
    }
}
