use std::time::Duration;
use windlass_types::{Backoff, Interval, RetryCount};

mod compliance;
mod mam;
mod monitoring;
mod qbit;
mod vpn;

pub use monitoring::{on_disk_space_observed, on_mam_rate_limit_violation};
pub use vpn::on_port_file_read_err;

const QBIT_SYNC_RETRY_LIMIT: RetryCount = RetryCount(3);
const HEARTBEAT_INTERVAL: Interval = Interval(Duration::from_secs(45 * 60));
const DISK_CHECK_INTERVAL: Interval = Interval(Duration::from_secs(6 * 60 * 60));
const TORRENT_CHECK_INTERVAL: Interval = Interval(Duration::from_secs(5 * 60));
const PORT_READ_RETRY_DELAY: Backoff = Backoff(Duration::from_millis(500));
const QBIT_AUTH_BACKOFF_BASE: Backoff = Backoff(Duration::from_secs(2));
const QBIT_SYNC_BACKOFF: Backoff = Backoff(Duration::from_secs(2));
/// Short fixed delay for connection-refused retries during container startup.
const QBIT_CONNECTION_RETRY_DELAY: Backoff = Backoff(Duration::from_secs(5));
