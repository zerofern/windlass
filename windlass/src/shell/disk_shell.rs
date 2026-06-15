//! Disk shell — periodic filesystem observation for `DiskMachine`.
//!
//! The shell reads available bytes immediately and at a fixed interval, then
//! emits typed observations. Threshold decisions remain in the pure core.
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedSender;
use tracing::warn;

use windlass_disk_core::{DiskAction, DiskEvent};
use windlass_machine::{Shell, Timed};

pub struct DiskShellConfig {
    pub data_path: PathBuf,
    pub poll_interval: Duration,
}

pub struct DiskShell;

fn spawn_poll_loop(config: DiskShellConfig, event_tx: UnboundedSender<Timed<DiskEvent>>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(config.poll_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match windlass_local::monitors::available_bytes(&config.data_path) {
                Ok(free_bytes) => {
                    if event_tx
                        .send(Timed::external(
                            Instant::now(),
                            windlass_machine::ExternalCause::Unknown,
                            DiskEvent::DiskSpaceObserved { free_bytes },
                        ))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => warn!(
                    path = %config.data_path.display(),
                    %error,
                    "failed to read available disk space"
                ),
            }
        }
    });
}

impl Shell for DiskShell {
    type Config = DiskShellConfig;
    type Event = DiskEvent;
    type Action = DiskAction;

    async fn new(config: Self::Config, event_tx: UnboundedSender<Timed<DiskEvent>>) -> Self {
        spawn_poll_loop(config, event_tx);
        Self
    }

    fn dispatch(&mut self, action: DiskAction, _event_tx: &UnboundedSender<Timed<DiskEvent>>) {
        // `DiskAction` is uninhabited — this match is exhaustive and
        // unreachable.  Pattern match defensively so future variants
        // surface as compile errors.
        match action {}
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::mpsc;
    use windlass_disk_core::DiskEvent;

    use super::{DiskShellConfig, spawn_poll_loop};

    #[tokio::test]
    async fn poll_loop_emits_initial_observation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_poll_loop(
            DiskShellConfig {
                data_path: temp.path().to_path_buf(),
                poll_interval: Duration::from_secs(60),
            },
            tx,
        );

        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("initial poll completes")
            .expect("event channel remains open");
        assert!(matches!(
            event.inner,
            DiskEvent::DiskSpaceObserved { free_bytes } if free_bytes > 0
        ));
    }
}
