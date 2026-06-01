mod compliance;
// §36 step 1: legacy `vpn` handler retired.
// §36 step 2: legacy `mam` handler retired (2026-05-31).
// §36 step 3: legacy `qbit` handler retired (2026-06-01).  `QbitMachine`
// owns auth/port-sync/preferences/torrents; domain DOM-29/30/31/32 cover
// the activity entries and Critical/Warning alerts.
// §36 step 4: legacy `monitoring` handler retired (2026-06-01).
// `DiskMachine` (via the bridge) drives BelowFloor/AboveFloor; domain
// DOM-9 emits the Warning alert + eviction; QbitMachine publishes
// `NewTorrentsAdded` (DOM-33 Info alert); MamMachine `RateLimited`
// drives DOM-34 (Critical alert).
// §36 step 5: legacy `download` handler retired (2026-06-01).  Web
// route sends `WindlassCommand::ManualDownload` directly to the domain;
// MAM/qBit cores own the fetch + add; domain DOM-35-39 cover the
// alerts and activity entries.
