use std::time::Duration;

use chrono::Utc;
use uom::si::information::byte;
use windlass_core::events::Event;
use windlass_debug::CausalTx;
use windlass_local::{monitors, vpn_files};
use windlass_types::{AlertPriority, AuthCookie, VpnIp, VpnPort, WakeupId};

use super::ShellContext;

impl ShellContext<'_> {
    // ── Timers ────────────────────────────────────────────────────────────────

    pub(super) fn schedule_wakeup(&mut self, id: WakeupId, duration: Duration) {
        // Cancel any existing timer for this id to prevent duplicate wakeup loops.
        if let Some(handle) = self.wakeups.remove(&id) {
            handle.abort();
        }
        let tx = self.tx.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            let _ = tx.send(Event::Wakeup { at: Utc::now(), id }).await;
        });
        self.wakeups.insert(id, handle);
    }

    // ── Port files ────────────────────────────────────────────────────────────

    /// Retry path only — the debounced file watcher handles normal reads.
    pub(super) fn read_port_files(&self, causal_tx: CausalTx) {
        let ip_file = self.vpn_ip_file.to_owned();
        let port_file = self.vpn_port_file.to_owned();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                vpn_files::read_port_files(&ip_file, &port_file)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            causal_tx
                .send(Event::PortFileReadResult {
                    at: Utc::now(),
                    result,
                })
                .await;
        });
    }

    // ── Docker ────────────────────────────────────────────────────────────────

    pub(super) fn fetch_and_dump_all_logs(&self, causal_tx: CausalTx) {
        let docker = self.docker.clone();
        let deps = self.dependents.to_vec();
        tokio::spawn(async move {
            docker.fetch_and_dump_logs(&deps).await;
            causal_tx.send(Event::LogsDumped { at: Utc::now() }).await;
        });
    }

    pub(super) fn stop_dependent_containers(&self) {
        let docker = self.docker.clone();
        let deps = self.dependents.to_vec();
        tokio::spawn(async move {
            docker.stop_dependents(&deps).await;
        });
    }

    pub(super) fn start_dependent_containers(&self) {
        let docker = self.docker.clone();
        let deps = self.dependents.to_vec();
        tokio::spawn(async move {
            docker.start_dependents(&deps).await;
        });
    }

    pub(super) fn restart_gluetun(&self) {
        let docker = self.docker.clone();
        tokio::spawn(async move {
            docker.restart_gluetun().await;
        });
    }

    // ── qBittorrent ───────────────────────────────────────────────────────────

    pub(super) fn authenticate_qbit(&self, causal_tx: CausalTx) {
        let qbit = self.qbit.clone();
        tokio::spawn(async move {
            let event = qbit.authenticate().await;
            causal_tx.send(event).await;
        });
    }

    pub(super) fn sync_qbit_port(&self, cookie: AuthCookie, port: VpnPort, causal_tx: CausalTx) {
        let qbit = self.qbit.clone();
        tokio::spawn(async move {
            let event = qbit.sync_port(&cookie, port).await;
            causal_tx.send(event).await;
        });
    }

    // ── MAM ───────────────────────────────────────────────────────────────────

    pub(super) fn update_mam(&self, _ip: VpnIp, causal_tx: CausalTx) {
        let mam = self.mam.clone();
        tokio::spawn(async move {
            let event = mam.update_seedbox().await;
            causal_tx.send(event).await;
        });
    }

    pub(super) fn check_mam_connectability(&self, causal_tx: CausalTx) {
        let mam = self.mam.clone();
        tokio::spawn(async move {
            let event = mam.check_connectability().await;
            causal_tx.send(event).await;
        });
    }

    // ── Monitoring ────────────────────────────────────────────────────────────

    pub(super) fn check_disk_space(&self, causal_tx: CausalTx) {
        let path = self.data_path.to_owned();
        tokio::spawn(async move {
            let space = tokio::task::spawn_blocking(move || monitors::check_disk_space(&path))
                .await
                .unwrap_or_else(|_| uom::si::f64::Information::new::<byte>(f64::MAX));
            causal_tx
                .send(Event::DiskSpaceObserved {
                    at: Utc::now(),
                    space,
                })
                .await;
        });
    }

    pub(super) fn check_new_torrents(&self, cookie: AuthCookie, causal_tx: CausalTx) {
        let qbit = self.qbit.clone();
        tokio::spawn(async move {
            // Shell sends the raw full list — Core owns the deduplication logic.
            let current = qbit.list_torrents(&cookie).await;
            causal_tx
                .send(Event::NewTorrentsObserved {
                    at: Utc::now(),
                    torrents: current,
                })
                .await;
        });
    }

    // ── Alerts ────────────────────────────────────────────────────────────────

    pub(super) fn send_gotify_alert(&self, priority: AlertPriority, message: String) {
        let gotify = self.gotify.clone();
        tokio::spawn(async move {
            gotify.send_alert(priority, &message).await;
        });
    }
}
