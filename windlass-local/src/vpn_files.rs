use notify_debouncer_mini::{DebounceEventResult, new_debouncer, notify::RecursiveMode};
use std::net::Ipv4Addr;
use std::path::Path;
use tokio::sync::mpsc;

use windlass_core::events::Event;
use windlass_types::{VpnIp, VpnPort};

/// Reads and parses both VPN files.
///
/// # Errors
/// Returns `Err` if either file is missing, empty, or unparseable —
/// the Core schedules a retry on error.
pub fn read_port_files(ip_file: &str, port_file: &str) -> Result<(VpnIp, VpnPort), String> {
    let ip_str = std::fs::read_to_string(ip_file).map_err(|e| format!("ip file: {e}"))?;
    let port_str = std::fs::read_to_string(port_file).map_err(|e| format!("port file: {e}"))?;

    let ip: Ipv4Addr = ip_str
        .trim()
        .parse()
        .map_err(|e| format!("ip parse: {e}"))?;

    let port_num: u16 = port_str
        .trim()
        .parse()
        .map_err(|e| format!("port parse: {e}"))?;

    let port = VpnPort::try_new(port_num).map_err(|e| format!("port validate: {e}"))?;

    Ok((VpnIp(ip), port))
}

/// Reads both VPN files once at boot, then spawns the debounced file watcher.
///
/// These two always happen together — boot read gives the Core its initial
/// state, and the watcher keeps it updated as Gluetun rotates the port.
///
/// # Errors
/// Returns the result of the initial file read; errors are expected when
/// Gluetun hasn't written the files yet.
pub async fn read_and_watch(
    vpn_ip_file: &str,
    vpn_port_file: &str,
    tx: mpsc::Sender<Event>,
) -> Result<(VpnIp, VpnPort), String> {
    let result = read_boot_port_files(vpn_ip_file, vpn_port_file).await;
    spawn_file_watcher(vpn_ip_file, vpn_port_file, tx);
    result
}

/// Reads both VPN files once at boot.
///
/// Called before the event loop starts so the Core can fast-forward to
/// connected state immediately.
///
/// # Errors
/// Returns an error string if the files are missing or unparseable.
pub async fn read_boot_port_files(
    vpn_ip_file: &str,
    vpn_port_file: &str,
) -> Result<(VpnIp, VpnPort), String> {
    let ip_file = vpn_ip_file.to_string();
    let port_file = vpn_port_file.to_string();
    tokio::task::spawn_blocking(move || read_port_files(&ip_file, &port_file))
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
}

/// Spawns a debounced inotify watcher on the Gluetun directory.
///
/// Collapses the raw inotify storm from a single write into one event per
/// 100ms window, then reads both VPN files and emits `PortFileReadResult`.
pub fn spawn_file_watcher(vpn_ip_file: &str, vpn_port_file: &str, tx: mpsc::Sender<Event>) {
    let watch_dir = Path::new(vpn_ip_file).parent().map_or_else(
        || "/tmp/gluetun".to_string(),
        |p| p.to_string_lossy().into_owned(),
    );
    spawn_file_watcher_inner(
        &watch_dir,
        vpn_ip_file.to_string(),
        vpn_port_file.to_string(),
        tx,
    );
}

/// Inner file-watcher spawn used by both `spawn_file_watcher` and the Tier 3
/// tests (which construct paths manually to control the watch directory).
///
/// # Panics
/// Panics if the debouncer or file watcher cannot be created (OS error).
pub fn spawn_file_watcher_inner(
    watch_dir: &str,
    ip_file: String,
    port_file: String,
    tx: mpsc::Sender<Event>,
) {
    // Capacity 1: if a read is already queued, drop extra signals.
    let (notify_tx, mut notify_rx) = mpsc::channel::<()>(1);

    let mut debouncer = new_debouncer(
        std::time::Duration::from_millis(100),
        move |_: DebounceEventResult| {
            // try_send: drop the signal if one is already pending so we never
            // queue more work than the processing loop can handle.
            let _ = notify_tx.try_send(());
        },
    )
    .expect("Failed to create file watcher debouncer");

    debouncer
        .watcher()
        .watch(Path::new(watch_dir), RecursiveMode::NonRecursive)
        .expect("Failed to watch gluetun dir");

    tokio::spawn(async move {
        let _debouncer = debouncer; // keep alive for the duration of the task
        let mut last_sent: Option<(VpnIp, VpnPort)> = None;
        while notify_rx.recv().await.is_some() {
            let ip_f = ip_file.clone();
            let port_f = port_file.clone();
            let result = tokio::task::spawn_blocking(move || read_port_files(&ip_f, &port_f))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));

            // Deduplicate: skip sending if content is identical to the last
            // successful send — prevents feedback loops where read-triggered
            // inotify events re-fire the debouncer.
            if let Ok(ref val) = result {
                if last_sent.as_ref() == Some(val) {
                    continue;
                }
                last_sent = Some(*val);
            }

            if tx.send(Event::PortFileReadResult(result)).await.is_err() {
                break;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_temp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "{content}").unwrap();
        f
    }

    #[test]
    fn read_port_files_parses_valid_input() {
        let ip_f = write_temp("10.8.0.1");
        let port_f = write_temp("51820");
        let (ip, port) = read_port_files(
            ip_f.path().to_str().unwrap(),
            port_f.path().to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(ip.0.to_string(), "10.8.0.1");
        assert_eq!(port.into_inner(), 51820);
    }

    #[test]
    fn read_port_files_trims_trailing_whitespace() {
        let ip_f = write_temp("  10.8.0.1  ");
        let port_f = write_temp("  51820  ");
        assert!(
            read_port_files(
                ip_f.path().to_str().unwrap(),
                port_f.path().to_str().unwrap(),
            )
            .is_ok()
        );
    }

    #[test]
    fn read_port_files_missing_ip_file_returns_err() {
        let port_f = write_temp("51820");
        let err =
            read_port_files("/nonexistent/ip_xyz", port_f.path().to_str().unwrap()).unwrap_err();
        assert!(err.contains("ip file"), "unexpected error: {err}");
    }

    #[test]
    fn read_port_files_missing_port_file_returns_err() {
        let ip_f = write_temp("10.8.0.1");
        let err =
            read_port_files(ip_f.path().to_str().unwrap(), "/nonexistent/port_xyz").unwrap_err();
        assert!(err.contains("port file"), "unexpected error: {err}");
    }

    #[test]
    fn read_port_files_malformed_ip_returns_err() {
        let ip_f = write_temp("not-an-ip");
        let port_f = write_temp("51820");
        let err = read_port_files(
            ip_f.path().to_str().unwrap(),
            port_f.path().to_str().unwrap(),
        )
        .unwrap_err();
        assert!(err.contains("ip parse"), "unexpected error: {err}");
    }

    #[test]
    fn read_port_files_malformed_port_returns_err() {
        let ip_f = write_temp("10.8.0.1");
        let port_f = write_temp("notaport");
        let err = read_port_files(
            ip_f.path().to_str().unwrap(),
            port_f.path().to_str().unwrap(),
        )
        .unwrap_err();
        assert!(err.contains("port parse"), "unexpected error: {err}");
    }

    // ── Tier 3: File-system integration ──────────────────────────────────────

    #[tokio::test]
    async fn file_watcher_fires_port_file_result_on_write() {
        use std::time::Duration;
        let dir = tempfile::TempDir::new().unwrap();
        let ip_path = dir.path().join("ip");
        let port_path = dir.path().join("forwarded_port");
        std::fs::write(&ip_path, "10.8.0.1").unwrap();
        std::fs::write(&port_path, "51820").unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        spawn_file_watcher_inner(
            dir.path().to_str().unwrap(),
            ip_path.to_str().unwrap().to_string(),
            port_path.to_str().unwrap().to_string(),
            tx,
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        std::fs::write(&port_path, "51821").unwrap();
        let event = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("timed out waiting for PortFileReadResult")
            .expect("channel closed unexpectedly");
        let expected_port = VpnPort::try_new(51821).unwrap();
        assert!(
            matches!(event, Event::PortFileReadResult(Ok((_, p))) if p == expected_port),
            "expected PortFileReadResult(Ok(_, 51821)), got {event:?}"
        );
    }

    #[tokio::test]
    async fn file_watcher_fires_exactly_once_per_write() {
        use std::time::Duration;
        let dir = tempfile::TempDir::new().unwrap();
        let ip_path = dir.path().join("ip");
        let port_path = dir.path().join("forwarded_port");
        std::fs::write(&ip_path, "10.8.0.1").unwrap();
        std::fs::write(&port_path, "51820").unwrap();
        let (tx, mut rx) = mpsc::channel(32);
        spawn_file_watcher_inner(
            dir.path().to_str().unwrap(),
            ip_path.to_str().unwrap().to_string(),
            port_path.to_str().unwrap().to_string(),
            tx,
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        std::fs::write(&port_path, "51821").unwrap();
        tokio::time::sleep(Duration::from_millis(350)).await;
        let mut count = 0;
        while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(10), rx.recv()).await {
            count += 1;
        }
        assert_eq!(
            count, 1,
            "debouncer must emit exactly 1 event per write burst, got {count}"
        );
    }

    #[tokio::test]
    async fn file_watcher_fires_on_subsequent_writes() {
        use std::time::Duration;
        let dir = tempfile::TempDir::new().unwrap();
        let ip_path = dir.path().join("ip");
        let port_path = dir.path().join("forwarded_port");
        std::fs::write(&ip_path, "10.8.0.1").unwrap();
        std::fs::write(&port_path, "51820").unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        spawn_file_watcher_inner(
            dir.path().to_str().unwrap(),
            ip_path.to_str().unwrap().to_string(),
            port_path.to_str().unwrap().to_string(),
            tx,
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        std::fs::write(&port_path, "51821").unwrap();
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("timed out on first write")
            .unwrap();
        // Wait for the debounce window to fully settle before the second write.
        tokio::time::sleep(Duration::from_millis(400)).await;
        std::fs::write(&port_path, "51822").unwrap();
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("timed out on second write — watcher stopped after first event")
            .unwrap();
    }
}
