// §36 retirement notes — handler files retired by step:
//   step 1 (2026-05-31): `vpn`        → `VpnMachine` + §38 Docker core
//   step 2 (2026-05-31): `mam`        → `MamMachine` + DOM-15/16/17/20
//   step 3 (2026-06-01): `qbit`       → `QbitMachine` + DOM-29/30/31/32
//   step 4 (2026-06-01): `monitoring` → `DiskMachine` + DOM-9; DOM-33
//                                       (new torrents); DOM-34 (rate limit)
//   step 5 (2026-06-01): `download`   → `WindlassCommand::ManualDownload`
//                                       + DOM-35/36/37/38/39
//   step 7 (2026-06-01): `compliance` → §20/§21/§24/§25 + DOM-8/11/12;
//                                       DOM-40 (persistence); DOM-41/42
//                                       (HnR-at-risk + HnR-lock alerts)
//
// The legacy `process_legacy_event` still exists in
// `windlass/src/shell/mod.rs` but every event-arm produces an empty
// action list; step 8 drops the shadow entirely.
