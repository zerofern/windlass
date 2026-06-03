use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use windlass_clients::mam::{MamClient, MamFetchError, MamSeedboxResult};
use windlass_machine::{ExternalCause, Shell, Timed};
use windlass_mam_core::{MamAction, MamEvent};

pub struct MamShell {
    client: MamClient,
}

impl Shell for MamShell {
    type Config = MamClient;
    type Event = MamEvent;
    type Action = MamAction;

    async fn new(client: Self::Config, _event_tx: UnboundedSender<Timed<MamEvent>>) -> Self {
        Self { client }
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
                    let _ = tx.send(Timed::external(
                        std::time::Instant::now(),
                        ExternalCause::Unknown,
                        event,
                    ));
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
                        MamSeedboxResult::RateLimited => MamEvent::RateLimited {
                            retry_after: Duration::from_secs(1),
                        },
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
                    let _ = tx.send(Timed::external(
                        std::time::Instant::now(),
                        ExternalCause::Unknown,
                        event,
                    ));
                });
            }
            MamAction::ScheduleTimer { timer, after } => {
                let tx = event_tx.clone();
                windlass_machine::causal::spawn(async move {
                    let scheduled_at = std::time::Instant::now() + after;
                    tokio::time::sleep(after).await;
                    let _ = tx.send(Timed::external(
                        scheduled_at,
                        ExternalCause::Timer { name: timer.name() },
                        MamEvent::TimerFired(timer),
                    ));
                });
            }
            // §36 step 5: fetch the `.torrent` bytes for a manual-download
            // admission.  `mam_client.fetch_torrent` returns `None` on any
            // network or HTTP error; we don't have a typed error surface
            // here so the failure reason stays generic.
            MamAction::FetchTorrentBytes { mam_id } => {
                let client = self.client.clone();
                let tx = event_tx.clone();
                windlass_machine::causal::spawn(async move {
                    let event = match client.fetch_torrent(mam_id).await {
                        Some(bytes) => MamEvent::TorrentBytesFetched { mam_id, bytes },
                        None => MamEvent::TorrentBytesFetchFailed {
                            mam_id,
                            reason: "MAM torrent fetch failed (network or HTTP error)".to_string(),
                        },
                    };
                    let _ = tx.send(Timed::external(
                        std::time::Instant::now(),
                        ExternalCause::Unknown,
                        event,
                    ));
                });
            }
        }
    }
}
