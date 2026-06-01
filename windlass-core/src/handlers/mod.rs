use std::time::Duration;
use windlass_types::Interval;

mod compliance;
mod download;
mod monitoring;
// §36 step 1: legacy `vpn` handler retired.
// §36 step 2: legacy `mam` handler retired (2026-05-31).
// §36 step 3: legacy `qbit` handler retired (2026-06-01).  `QbitMachine`
// owns auth/port-sync/preferences/torrents; domain DOM-29/30/31/32 cover
// the activity entries and Critical/Warning alerts.

pub use download::{on_torrent_add_failed, on_torrent_added_to_qbit};
pub use monitoring::{on_disk_space_observed, on_mam_rate_limit_violation};

const DISK_CHECK_INTERVAL: Interval = Interval(Duration::from_hours(6));
const TORRENT_CHECK_INTERVAL: Interval = Interval(Duration::from_mins(5));
