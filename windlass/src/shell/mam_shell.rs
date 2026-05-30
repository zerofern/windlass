use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use windlass_clients::mam::{MamClient, MamFetchError};
use windlass_machine::{Shell, Timed};
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
                tokio::spawn(async move {
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
                    let _ = tx.send(Timed::now(event));
                });
            }
            MamAction::UpdateSeedbox => {
                let client = self.client.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let event = match client.update_seedbox().await {
                        // §32: forward the registered IP/ASN/AS the legacy
                        // event carries so the MAM core can dedup further
                        // updates against MAM's view of "what's registered".
                        windlass_core::events::Event::MamUpdateSuccess {
                            registered_ip,
                            registered_asn,
                            registered_as,
                            ..
                        } => MamEvent::SeedboxUpdated {
                            registered_ip,
                            registered_asn,
                            registered_as,
                        },
                        windlass_core::events::Event::MamRateLimitViolation { .. } => {
                            MamEvent::RateLimited {
                                retry_after: Duration::from_secs(1),
                            }
                        }
                        // §30: ASN mismatch is a distinct compliance signal,
                        // not a generic SeedboxUpdateFailed.  The MAM core
                        // tracks it via AsnState and the domain blocks
                        // admission + raises a Critical alert.
                        windlass_core::events::Event::MamAsnMismatch { ip, .. } => {
                            MamEvent::AsnMismatch { ip }
                        }
                        // §28: a transport-level failure now arrives as a
                        // distinct event instead of being silently mapped to
                        // SeedboxUpdateFailed.
                        windlass_core::events::Event::MamUnreachable { reason, .. } => {
                            MamEvent::Unreachable { reason }
                        }
                        other => MamEvent::SeedboxUpdateFailed {
                            reason: format!("unexpected response: {other:?}"),
                        },
                    };
                    let _ = tx.send(Timed::now(event));
                });
            }
            MamAction::ScheduleTimer { timer, after } => {
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let scheduled_at = std::time::Instant::now() + after;
                    tokio::time::sleep(after).await;
                    let _ = tx.send(Timed::new(scheduled_at, MamEvent::TimerFired(timer)));
                });
            }
        }
    }
}
