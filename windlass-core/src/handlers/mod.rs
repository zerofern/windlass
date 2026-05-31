use std::time::Duration;
use windlass_types::{Backoff, Interval, RetryCount};

mod compliance;
mod download;
mod mam;
mod monitoring;
mod qbit;
// §36 step 1: legacy `vpn` handler retired.  Dispatch arms in lib.rs now
// no-op; `VpnMachine` (via the `service_events.rs` bridge) owns the real
// behaviour.

pub use download::{on_torrent_add_failed, on_torrent_added_to_qbit};
pub use monitoring::{on_disk_space_observed, on_mam_rate_limit_violation};

const QBIT_SYNC_RETRY_LIMIT: RetryCount = RetryCount(3);
const HEARTBEAT_INTERVAL: Interval = Interval(Duration::from_mins(45));
const DISK_CHECK_INTERVAL: Interval = Interval(Duration::from_hours(6));
const TORRENT_CHECK_INTERVAL: Interval = Interval(Duration::from_mins(5));
const QBIT_AUTH_BACKOFF_BASE: Backoff = Backoff(Duration::from_secs(2));
const QBIT_SYNC_BACKOFF: Backoff = Backoff(Duration::from_secs(2));
/// Short fixed delay for connection-refused retries during container startup.
const QBIT_CONNECTION_RETRY_DELAY: Backoff = Backoff(Duration::from_secs(5));
