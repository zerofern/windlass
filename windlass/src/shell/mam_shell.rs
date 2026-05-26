use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use windlass_clients::mam::MamClient;
use windlass_machine::{Shell, Timed};
use windlass_mam_core::{MamAction, MamEvent};
use windlass_types::MamStatus;

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
                    let event = match client.check_connectability().await {
                        windlass_core::events::Event::MamStatusObserved { status, .. } => {
                            match status {
                                MamStatus::Connectable => MamEvent::StatusFetched {
                                    connectable: true,
                                    seedbox_port: None,
                                },
                                MamStatus::NotConnectable => MamEvent::StatusFetched {
                                    connectable: false,
                                    seedbox_port: None,
                                },
                                MamStatus::Unreachable => MamEvent::StatusFailed {
                                    reason: "MAM unreachable".to_string(),
                                },
                            }
                        }
                        windlass_core::events::Event::MamRateLimitViolation { .. } => {
                            MamEvent::RateLimited {
                                retry_after: Duration::from_secs(1),
                            }
                        }
                        other => MamEvent::StatusFailed {
                            reason: format!("unexpected response: {:?}", other),
                        },
                    };
                    let _ = tx.send(Timed::now(event));
                });
            }
            MamAction::UpdateSeedboxPort { port } => {
                let client = self.client.clone();
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let event = match client.update_seedbox().await {
                        windlass_core::events::Event::MamUpdateSuccess { .. } => {
                            MamEvent::SeedboxUpdated { port }
                        }
                        windlass_core::events::Event::MamRateLimitViolation { .. } => {
                            MamEvent::RateLimited {
                                retry_after: Duration::from_secs(1),
                            }
                        }
                        windlass_core::events::Event::MamAsnMismatch { ip, .. } => {
                            MamEvent::SeedboxUpdateFailed {
                                port,
                                reason: format!("ASN mismatch for {}", ip.0),
                            }
                        }
                        other => MamEvent::SeedboxUpdateFailed {
                            port,
                            reason: format!("unexpected response: {:?}", other),
                        },
                    };
                    let _ = tx.send(Timed::now(event));
                });
            }
            MamAction::ScheduleTimer { timer, after } => {
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(after).await;
                    let _ = tx.send(Timed::now(MamEvent::TimerFired(timer)));
                });
            }
        }
    }
}
