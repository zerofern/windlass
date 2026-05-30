# Legacy `windlass-core` Retirement Plan (§36)

This document is the cutover plan for operator-readiness §36 — moving every
live decision off `windlass-core::SystemState` and onto the per-system
`Machine` cores (`VpnMachine`, `QbitMachine`, `MamMachine`, `DbMachine`,
`WindlassMachine`), then deleting the legacy crate.

§36 itself ships in two phases:

1. **This audit** (the file you're reading): per-handler inventory, gap
   analysis, and proposed cutover order. No code deletion.
2. **Per-handler cutover commits** (follow-up sessions): each handler
   removed independently, with `just integration` green at every step.

Treat the audit as a working document — update it as gaps close and as the
real cutover sequence diverges from the proposed one.

## Today's shadow

`windlass/src/shell/mod.rs` runs both paths per event:

```rust
service_cores.observe(&event);                                  // (1) feed new cores
let outcome = process_legacy_event(event, &mut state, ...);     // (2) run legacy
dispatch_event(outcome.actions, ...);                           // (3) execute LEGACY actions
```

Step (1) feeds the new per-system cores via the generic service runtime,
which routes their actions to their own shells (`qbit_shell.rs`,
`mam_shell.rs`, `vpn_shell.rs`, etc.).  Step (2) runs the legacy
`SystemState::process_event` and produces legacy `Action`s.  Step (3)
executes those legacy actions through `dispatch_event`.

**Both paths run in production.**  The new cores aren't pure observers —
their actions do reach qBit/MAM/Docker via the service runtime.  But the
legacy path is also live and is the originator for several decisions the
new cores do not yet make (compliance/torrent persistence in particular).

The cutover deletes the legacy path entirely; the new cores become the
sole decision-makers.

## Per-handler inventory

| File | Lines | Coverage | Risk | Notes |
|---|---:|---|---|---|
| `vpn.rs`        |  161 | Partial    | Medium | Decision logic covered by VpnMachine + §5/§31/§33/§35, **but** crash-recovery side-effects (log dump, stop/start dependents, Gluetun restart, "Gluetun died" alert) are not emitted by the new path. **Blocked on §38** (Docker core). |
| `mam.rs`        |   92 | Full       | Low    | MamMachine + §7/§27/§28/§30/§32. |
| `qbit.rs`       |  178 | Full*      | Low    | QbitMachine + §6 et al.  Excludes the compliance pass (handled separately). |
| `monitoring.rs` |  103 | Partial    | Medium | Most decisions in new cores; the `on_wakeup` dispatcher needs an audit. |
| `download.rs`   |  247 | Partial    | Medium | §29's `TryAddTorrent` covers the autonomous path; manual-UI add path may still rely on legacy. |
| `compliance.rs` |  242 | Partial    | **High** | DB-write path for `/api/v1/torrents` is the coupling. See below. |

### `vpn.rs` (Medium risk — blocked on §38)

Handlers: `on_init`, `on_docker_gluetun_died`, `on_logs_dumped`,
`on_docker_gluetun_healthy`, `on_port_file_read_ok`,
`on_port_file_read_err`.

New-core equivalent: `VpnMachine`'s `Init`, `ContainerHealthy`,
`ContainerUnhealthy`, `PortFileChanged`, `StateReadFailed` arms, plus the
§31/§33/§35 extensions.

**Gap identified after initial audit:**  the legacy handler emits five
side-effect actions on Gluetun crash recovery that the new path does not
produce:

- `Action::FetchAndDumpAllLogs` (logs on every crash) — new
  `VpnAction::WriteCrashDump` is a stub in `vpn_shell.rs`.
- `Action::StopDependentContainers` / `Action::StartDependentContainers`
  — no `VpnAction` covers fleet stop/start at all; §35's per-dependent
  `RestartContainer` is also a stub.
- `Action::RestartGluetun` — no equivalent on `VpnAction`.
- `Action::SendAlert(Critical, "Gluetun died")` — domain core emits
  alerts on IP-mismatch / dependent-untrusted / restart-storm but not on
  plain crash.

These belong in a new Docker core rather than in VPN core (operator
preference: "dependents are not a VPN concern").  This is now §38 in
operator-readiness.md.

**Cutover step (post-§38):** in `service_events.rs`, the legacy →
ServiceEvent translation already maps each Docker/port event to the
right `VpnEvent`.  After §38 lands, removing the legacy handler is a
matter of dropping the match arms in `windlass-core/src/lib.rs` and
verifying the shell-side action emission (`VpnAction::InspectContainer`
plus Docker-core fleet commands routed via domain) covers what
`process_legacy_event` used to emit.

### `mam.rs` (Low risk)

Handlers: `on_mam_update_success`, `on_mam_asn_mismatch`,
`on_mam_connectable`, `on_mam_not_connectable`.

New-core equivalent: `MamMachine` arms.  §30 already routes
`MamAsnMismatch` distinctly (DOM-20), §28 distinguishes
`Unreachable`/`NotConnectable` (DOM-15/16), §32 carries
registered IP/ASN/AS in `SeedboxUpdated` (MAM-18/19).

**Cutover step:** same shape as VPN — drop legacy match arms; the
service-events bridge already routes the legacy events to their MAM-core
equivalents.

### `qbit.rs` (Low risk)

Handlers: `on_qbit_auth_success`, `on_qbit_connection_refused`,
`on_qbit_auth_failed`, `on_qbit_api_error`, `on_qbit_port_sync_success`,
`on_qbit_port_sync_failed`, `on_qbit_preferences_received`.

New-core equivalent: `QbitMachine` arms.  QBIT-1 (cookie gate), QBIT-2/3
(refresh chain), QBIT-4/6/7 (port convergence), QBIT-5 (failure backoff),
QBIT-12/20 (privacy), QBIT-17/18/19 (quota), QBIT-21 (AddTorrent
cookie gate).

**Cutover step:** drop legacy arms.  *Caveat:* `on_qbit_preferences_received`
takes `max_active_torrents` and feeds the legacy active-limit logic in
`compliance.rs` — that flow gets retired together with compliance (below).

### `monitoring.rs` (Medium risk)

Handlers: `on_new_torrents_observed`, `on_wakeup`,
`on_disk_space_observed`, `on_mam_rate_limit_violation`.

New-core equivalent: mostly `DiskMachine` (§22) for the free-space
signal, `QbitMachine`'s `TorrentRefresh` chain (§6) for the periodic
torrent fetch, MAM core (§28) for rate-limit handling.

**Gap to verify:**
- `on_wakeup(WakeupId)` dispatches against a `WakeupId` enum.  Each
  variant needs a confirmed new-core equivalent (or removal).  Specifically
  audit: heartbeat wakeup, disk-check wakeup, compliance-poll wakeup.
- `on_new_torrents_observed` writes per-torrent metadata; verify the
  new path covers this (likely via `QbitPublish::TorrentsUpdated` +
  DB-core persistence).

### `download.rs` (Medium risk)

Handlers: `on_manual_download_requested`, `on_torrent_added_to_qbit`,
`on_torrent_add_failed`.

New-core equivalent: §29 introduced `WindlassCommand::TryAddTorrent`
which goes through the composite admission predicate and routes to
`QbitCommand::AddTorrent` on success.

**Gap to verify:**
- The manual-download UI endpoint (web route) currently emits the
  legacy `Event::ManualDownloadRequested`.  It should be migrated to
  emit `WindlassCommand::TryAddTorrent { candidate }` instead, with the
  web layer constructing the `DownloadCandidate` from the MAM-id.
- The `Event::TorrentAddedToQbit` / `TorrentAddFailed` post-conditions
  (which the legacy path turns into activity + alerts) need new-core
  equivalents in the qBit-shell `AddTorrent` action handler (currently
  stubbed for librarian A1).

### `compliance.rs` (HIGH risk)

The most coupled handler.  `on_qbit_torrent_details_received` runs five
checks (new-torrent priority, dead torrents, HnR-at-risk alerts, quota,
active-limit) **plus** writes the `torrents` DB table that
`/api/v1/torrents` and the Torrent Monitor UI read.

| Legacy check                       | New-core equivalent | Status |
|------------------------------------|---------------------|---|
| `check_new_torrents` (no-partials) | §21 / QBIT-10       | Covered |
| `check_dead_torrents`              | §20 / QBIT-9 + DOM-8 | Covered |
| `check_hnr_at_risk` (alerts)       | §19 / QBIT-8 (mostly — alert path differs) | Mostly covered; verify alert shape parity |
| `check_quota`                      | §25 / QBIT-17/18/19 + DOM-12 | Covered |
| `check_active_limit`               | §24 / QBIT-14/15/16 + DOM-11 | Covered |
| `Action::UpsertTorrentRecords`     | **Gap** — no new-core path yet | **Blocker** |

**Persistence gap.**  Today the legacy `Action::UpsertTorrentRecords`
writes the `torrents` table on every `QbitTorrentDetailsReceived`
event.  The new qBit core observes torrents (`TorrentsListed` → state
update + publishes) but does not yet persist them through the DB core
to the `torrents` table.

The Torrent Monitor UI reads from that table, so without a new-core
persistence path the UI goes blank after legacy removal.

This is the **single hardest piece of work** in §36.  Concretely it
needs:

1. A `QbitPublish::TorrentRecords { rows }` or equivalent that carries
   the snapshot.
2. Domain (or a new librarian/persistence layer) routes that publish to
   a `Db(UpsertTorrentRecords)` action.
3. DB core / shell already handles upsert — verify that exists.
4. UI continues reading from the same table; no schema change.

Once that path is wired, `compliance.rs` can be removed in one piece
because every other check is already in the new cores.

## Cutover order

0. **§38: introduce Docker core** — prerequisite for step 1.  Until
   Docker core owns container lifecycle (start/stop/restart/dump) and
   domain emits the crash-recovery alert, removing `vpn.rs` would lose
   real behaviour.  See operator-readiness.md §38.
1. **vpn.rs** — after §38 lands.  Remove the legacy `Event::*` arms
   that map to VPN events; let the service-events bridge keep doing
   the translation until step 7.
2. **mam.rs** — same shape, slightly larger event set.
3. **qbit.rs** (excluding compliance-related preferences flow) —
   careful with `on_qbit_preferences_received`; the `max_active_torrents`
   feeding flag has to keep working until compliance is retired.
4. **monitoring.rs** — audit `on_wakeup` first; if every wakeup id has
   a new-core consumer, remove.
5. **download.rs** — migrate the web manual-download route to emit
   `WindlassCommand::TryAddTorrent`; then remove.
6. **Wire torrent-records persistence** through the new cores (the
   compliance.rs persistence gap above).  This is its own sub-story —
   probably warrants a §36a marker before §36b proceeds.
7. **compliance.rs** — remove last, after the persistence path is
   green.
8. **Drop the shadow** — remove `process_legacy_event` and the
   `service_cores.observe()` call from the shell event loop.  At this
   point new cores are the sole live decision-makers.
9. **Workspace cleanup** — drop `windlass-core` dep from every crate
   that still has it, delete the crate, update `Cargo.toml` /
   `Cargo.lock`.
10. **Verify** — `just check`, frontend build, `just integration`.

Each step lands as its own commit so the cutover is bisectable.

## Out of scope for §36

- Debug-mode replay against the new cores (queued as §37).
- Any *new* operator behavior — §36 is pure migration.

## Open questions for the cutover

- Does `WakeupId` need to survive the cutover in some form, or are all
  its uses replaced by the per-machine self-perpetuating timer chains
  (QbitTimer::TorrentRefresh, MamTimer::KeepAlive, etc.)?
- What's the right home for the torrent-records persistence path —
  inside the qBit core (publishing a `TorrentRecords` snapshot), inside
  the domain (which already routes other things to DB), or a new
  persistence-layer crate?  Lean: qBit core publishes, domain routes to
  `Db(Upsert…)`.  Decide as part of step 6.
- Does the web `/api/v1/torrents` endpoint need a new-core-aware
  refresh trigger, or is the existing DB-table read sufficient once the
  qBit core keeps it populated?
