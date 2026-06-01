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
| `vpn.rs`        |  161 | Full       | Low    | §38 (Docker core) lands the crash-recovery side-effects (Docker(DumpAllLogs/StopDependents/RestartContainer{anchor}) + SendAlert(Critical "Gluetun died")) via the domain's DOM-27/DOM-28 handlers on VpnPublish::Crashed/Recovered. **Unblocked.** |
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

### `mam.rs` (Low risk — DONE 2026-05-31)

Handlers: `on_mam_update_success`, `on_mam_asn_mismatch`,
`on_mam_connectable`, `on_mam_not_connectable`.

New-core equivalent: `MamMachine` arms.  §30 already routes
`MamAsnMismatch` distinctly (DOM-20), §28 distinguishes
`Unreachable`/`NotConnectable` (DOM-15/16), §32 carries
registered IP/ASN/AS in `SeedboxUpdated` (MAM-18/19).

**Cutover step (DONE):** legacy `handlers/mam.rs` deleted; the four
event dispatches (`MamUpdateSuccess` / `MamAsnMismatch` /
`MamStatusObserved` / `MamUnreachable`) now no-op.  The legacy
"NAT frozen" hard-recovery path that set `state.vpn =
VpnState::DumpingLogs` and emitted `FetchAndDumpAllLogs` is
intentionally retired — §38's DOM-27 owns Gluetun restart on real
crashes; MAM `NotConnectable` no longer drives a stack restart.

### `qbit.rs` (Low risk)

Handlers: `on_qbit_auth_success`, `on_qbit_connection_refused`,
`on_qbit_auth_failed`, `on_qbit_api_error`, `on_qbit_port_sync_success`,
`on_qbit_port_sync_failed`, `on_qbit_preferences_received`.

New-core equivalent: `QbitMachine` arms.  QBIT-1 (cookie gate), QBIT-2/3
(refresh chain), QBIT-4/6/7 (port convergence), QBIT-5 (failure backoff),
QBIT-12/20 (privacy), QBIT-17/18/19 (quota), QBIT-21 (AddTorrent
cookie gate).  `max_active_torrents` is fully in the new core via the
QbitMachine's own `ReadPreferences` action (not the legacy event).

**Gaps to port before retirement** (2026-06-01 audit):

| Legacy behaviour                                | New-path status |
|-------------------------------------------------|---|
| `SendAlert(Critical, "qBit auth failed")` on `Event::QbitAuthFailed` | **Gap** — `AuthFailed` collapses all 3 failure modes; only an Activity entry fires today. |
| `SendAlert(Warning, "qBit port sync failed")` after 3 retries  | **Gap** — `ListenPortSetFailed` retries via SyncRetry timer without attempt counter. |
| `WriteActivity("qbit_authenticated")` on auth success           | **Gap** — domain on `QbitPublish::Ready` sets `ServiceStatus::Ready` but no Activity entry. |
| `WriteActivity("port_synced")` on port sync success             | **Gap** — domain on `QbitPublish::ListenPortReady` sets admission gate but no Activity entry. |
| `ScheduleWakeup(CompliancePoll)` after port sync                | Covered — QbitMachine's `TorrentRefresh` timer self-drives torrent monitoring (§6/QBIT-2/3). |
| `max_active_torrents` storage for compliance                    | Covered — already in new path via `QbitAction::ReadPreferences`. |

**Cutover step:** port the 4 gaps (auth-rejected Critical, port-sync
persistent failure Warning, two rising-edge Activity entries), then
drop the legacy arms.

### `monitoring.rs` (Medium risk)

Handlers: `on_new_torrents_observed`, `on_wakeup`,
`on_disk_space_observed`, `on_mam_rate_limit_violation`.

New-core equivalent: mostly `DiskMachine` (§22) for the free-space
signal, `QbitMachine`'s `TorrentRefresh` chain (§6) for the periodic
torrent fetch, MAM core (§28) for rate-limit handling.

**Gaps to port before retirement** (2026-06-01 audit):

| Legacy behaviour | New-path status |
|---|---|
| `SendAlert(Info, "New torrents")` on `on_new_torrents_observed` (when fresh names appear) | **Gap** — QbitMachine publishes `TorrentsUpdated { hashes }` but no Info alert; domain does not emit one. |
| `SendAlert(Warning, "Low disk space")` at <50 GB | **Likely covered** — DiskMachine publishes `BelowFloor`; verify domain emits a Warning alert. |
| `SendAlert(Critical, "MAM rate limit")` on rate-limit violation | **Gap** — bridge routes to `MamEvent::RateLimited`; MamMachine handles backoff but no Critical alert in domain on rate-limit. |
| `ScheduleWakeup(Heartbeat/DiskCheck/TorrentCheck/CompliancePoll/...)` | Mostly filtered or covered by new self-driving timers; verify per-WakeupId during step 4. |

### `compliance.rs` shared state mutation (gap noted at audit time)

`on_qbit_torrent_details_received` writes
`self.torrents = torrents.into_iter().map(...).collect();` — populates
the legacy in-memory torrent index that several legacy handlers
(`on_delete_torrent_requested`, `on_manual_download_requested` quota
check, `check_active_limit`) read.  Equivalent state lives in the new
`QbitMachine`; legacy reads of `self.torrents` will become stale until
those handlers are retired.

### `download.rs` (Medium risk)

Handlers: `on_manual_download_requested`, `on_torrent_added_to_qbit`,
`on_torrent_add_failed`.

New-core equivalent: §29 introduced `WindlassCommand::TryAddTorrent`
which goes through the composite admission predicate and routes to
`QbitCommand::AddTorrent` on success.

**Gaps to port before retirement** (2026-06-01 audit):

| Legacy behaviour | New-path status |
|---|---|
| Web route emits `Event::ManualDownloadRequested` | **Gap** — route must emit `WindlassCommand::TryAddTorrent { candidate }`; web layer builds `DownloadCandidate` from MAM-id. |
| `WriteActivity("download_blocked")` on blacklisted | **Gap** — §29 admission predicate publishes an `Activity` reason on rejection but does not emit a structured `download_blocked` entry with mam_id detail. |
| `SendAlert(Warning, "Download blocked — quota full")` | **Gap** — §29 publishes the rejection reason as Activity; no Warning alert today. |
| `SendAlert(Warning, "Download blocked — qBit not ready")` | **Gap** — same as above. |
| `SendAlert(Info, "Download started") + WriteActivity("torrent_added") + UpsertTorrentRecords(stub)` on add success | **Gap** — `QbitAction::AddTorrent` is stubbed (librarian A1); no post-conditions wired. |
| `SendAlert(Warning, "Download failed") + WriteActivity("torrent_add_failed")` | **Gap** — same as above. |

### `compliance.rs` (HIGH risk)

The most coupled handler.  `on_qbit_torrent_details_received` runs five
checks (new-torrent priority, dead torrents, HnR-at-risk alerts, quota,
active-limit) **plus** writes the `torrents` DB table that
`/api/v1/torrents` and the Torrent Monitor UI read.

| Legacy check                       | New-core equivalent | Status |
|------------------------------------|---------------------|---|
| `check_new_torrents` (no-partials) | §21 / QBIT-10       | Covered |
| `check_dead_torrents`              | §20 / QBIT-9 + DOM-8 | Covered |
| `check_hnr_at_risk` (alerts)       | §19 / QBIT-8 (mostly — alert path differs) | **Verify** Critical "HnR at risk" alert in new path |
| `on_delete_torrent_requested` HnR lock alert | new download path | **Gap** — Warning "HnR lock — cannot delete" alert not yet in new manual-delete flow |
| `check_quota`                      | §25 / QBIT-17/18/19 + DOM-12 | Covered |
| `check_active_limit`               | §24 / QBIT-14/15/16 + DOM-11 | Covered |
| `Action::UpsertTorrentRecords`     | §36 step 6: `QbitPublish::TorrentRecords` → domain DOM-40 → `Db(UpsertTorrent)` | **DONE 2026-06-01** |

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

0. **§38: introduce Docker core** — DONE (2026-05-31).  Docker core
   owns container lifecycle; domain emits the crash-recovery alert via
   DOM-27 on VpnPublish::Crashed; autoheal subsumed; VPN core no longer
   references DockerClient.  See operator-readiness.md §38.
1. **vpn.rs** — DONE (2026-05-31).  Legacy `handlers/vpn.rs` deleted;
   `Event::Init / DockerGluetunDied / LogsDumped / DockerGluetunHealthy /
   PortFileReadResult` dispatches in `windlass-core/src/lib.rs` now
   no-op.  `service_events.rs` continues to translate those events into
   `VpnEvent::*` for `VpnMachine`; crash recovery runs through §38's
   DOM-27 path.  Legacy `state.vpn` stays at `VpnState::Stopped`;
   remaining legacy handlers' `VpnState::Connected` branches no-op until
   their own retirement (steps 2-5).
2. **mam.rs** — DONE (2026-05-31).  Legacy `handlers/mam.rs` deleted;
   `MamUpdateSuccess / MamAsnMismatch / MamStatusObserved / MamUnreachable`
   dispatches now no-op.  `MamMachine` (via the bridge) drives the real
   behaviour; domain DOM-15/16/17/20 cover the alerts.  The legacy
   "NAT frozen" hard-recovery is retired (see §36 step 2 notes).
3. **qbit.rs** — DONE (2026-06-01).  Legacy `handlers/qbit.rs` deleted;
   `QbitAuthSuccess / QbitAuthFailed / QbitConnectionRefused /
   QbitApiError / QbitPortSyncSuccess / QbitPortSyncFailed /
   QbitPreferencesReceived / QbitPreferencesFailed` dispatches now no-op.
   QbitMachine gains `QbitEvent::AuthRejected` (credentials-specific),
   `QbitPublish::AuthRejected` (Critical alert via DOM-30), and
   `QbitPublish::ListenPortPersistentFailure` (Warning alert via
   DOM-31, gated by `QbitConfig::max_sync_attempts`).  Domain DOM-29
   emits the `qbit_authenticated` activity entry on rising-edge
   `Ready`; DOM-32 emits `port_synced` on rising-edge `ListenPortReady`.
   `max_active_torrents` reaches the new core via QbitMachine's own
   `ReadPreferences` action (separate from the legacy event).  Legacy
   `state.max_active_torrents` stays at default 5; legacy
   `compliance.rs::check_active_limit` is inert with fewer than 5
   active torrents until step 7 retires compliance.
4. **monitoring.rs** — DONE (2026-06-01).  Legacy `handlers/monitoring.rs`
   deleted; `Event::DiskSpaceObserved / NewTorrentsObserved / Wakeup /
   MamRateLimitViolation` dispatches now no-op.  `DiskShell` +
   `DiskMachine` spawned in `init_shell` (50 GiB hard floor);
   `service_events.rs` bridges `Event::DiskSpaceObserved` to
   `DiskEvent::DiskSpaceObserved`; domain DOM-9 extended with the
   Warning "Low disk space" alert + EvictOneForDiskPressure.
   QbitMachine publishes new `QbitPublish::NewTorrentsAdded { hashes }`
   on rising-edge newly-seen torrents; domain DOM-33 fires the Info
   "New torrents" alert (hash-only — `TorrentName` legacy feed retired).
   Domain DOM-34 extended on `MamPublish::RateLimited` to also fire a
   Critical "MAM rate limit" alert.  `Event::Wakeup` is now a no-op
   for every `WakeupId` (each had a self-driving timer in the relevant
   core or no remaining consumer).
6. **Torrent-records persistence** — DONE (2026-06-01).  New
   `QbitPublish::TorrentRecords { records }` fired on every
   `TorrentsListed`; domain DOM-40 fans out
   `DbCommand::UpsertTorrent` per record.  `windlass-types::
   TorrentRecord` gains `name: TorrentName` + `seen_at:
   DateTime<Utc>` so the new feed carries the fields the legacy
   `torrents` DB row + UI rely on.  qBit shell populates `name` from
   `QbitTorrentDetails`; service-events bridge passes through legacy
   torrent names; `/api/v1/torrents` (Torrent Monitor UI) keeps
   reading the same table.

**§36 closed at sub-step 9b** (2026-06-01).  The remaining sub-steps
require touching the debug-mode crate (covered by §37) and the
operator dashboard SSE (needs a richer
`domain-core::SystemStateView` before the React app can migrate
without losing IP/port detail).  Tracked as follow-ups; `windlass-core`
stays in the workspace until both land.

9b. **windlass-local typed events** — DONE (2026-06-01).
    `vpn_files::spawn_file_watcher` now takes `Sender<PortFileResult>`
    (typed `Result<(VpnIp, VpnPort), String>`) instead of
    `Sender<Event>`.  init_shell spawns a forwarder task that maps each
    typed result into `VpnEvent::PortFileChanged + PublicIpFromFile`
    on success or `VpnEvent::StateReadFailed` on failure — direct into
    the VpnMachine event channel; the legacy bridge entry is bypassed.
    `DockerClient::spawn_event_watcher` + `spawn_health_poll_watcher`
    deleted (DockerShell owns the bollard watcher since §38 PR 2;
    legacy path was redundant).  `DockerClient::boot` no longer takes
    a `tx` channel.  `windlass-core` dep dropped from
    `windlass-local/Cargo.toml`.

9a. **windlass-clients typed returns** — DONE (2026-06-01).
    `QbitClient::authenticate` / `sync_port` return new
    `QbitAuthResult` / `QbitPortSyncResult` enums in `qbit/types.rs`;
    `MamClient::update_seedbox` returns new `MamSeedboxResult` enum.
    `HttpObserver` moved from `windlass-core` to `windlass-types`.
    Dead methods removed: `QbitClient::list_torrents` (legacy torrent
    poll) and `MamClient::check_connectability` (legacy heartbeat).
    `windlass-core` dep dropped from `windlass-clients/Cargo.toml`.
    qbit_shell + mam_shell updated to consume typed results; 4
    list_torrents tests + 7 check_connectability tests deleted; 1
    test renamed to reflect the honest parse-failure behaviour.

8. **Drop the shadow** — DONE (2026-06-01).  The shell event loop no
   longer runs `process_legacy_event` or `dispatch_event`.  The loop
   now reads each event, sends it through `service_cores.observe` (the
   bridge that routes to the per-system new cores), and that's it.
   Dead helper modules `actions.rs`, `compliance.rs`, `download.rs`
   under `windlass/src/shell/` deleted.  Legacy `Event` type stays as
   the bridge protocol the I/O sites already use (Docker watcher, VPN
   files, MAM/qBit clients).  Legacy `SystemState` is frozen at
   `initial()` — the operator's old dashboard SSE shows a stale view;
   step 9 migrates the SSE to the new `WindlassPublish::SystemState`
   shape and deletes `windlass-core`.

7. **compliance.rs** — DONE (2026-06-01).  Legacy
   `handlers/compliance.rs` + `tests/compliance.rs` deleted; the
   entire `tests/` module dropped (all legacy handler tests were
   already gone).  `Event::QbitTorrentDetailsReceived /
   QbitPreferencesReceived / QbitPreferencesFailed /
   DeleteTorrentRequested` dispatches now no-op.  QbitMachine gains
   `QbitPublish::HnRAtRisk` (fired per at-risk torrent per
   `TorrentsListed` cycle for legacy parity) and
   `QbitPublish::DeleteBlockedHnRLock` (fired from the
   `DeleteTorrent` command path when the HnR seed-time gate
   refuses).  Domain DOM-41 fires the Critical "HnR at risk"
   alert; DOM-42 fires the Warning "HnR lock — cannot delete"
   alert.  `handlers/mod.rs` is now header-only.

5. **download.rs** — DONE (2026-06-01).  Legacy `handlers/download.rs`
   deleted; `Event::ManualDownloadRequested / TorrentAddedToQbit /
   TorrentAddFailed` dispatches now no-op.  Web route now sends
   `WindlassCommand::ManualDownload { mam_id }` directly to the domain
   runtime (via a new `AppState::domain_command_tx`).  Domain
   `handle_manual_download` runs a 3-gate admission subset
   (blacklist / unsatisfied-quota / qBit-ready) mirroring the legacy
   `on_manual_download_requested`; on pass, dispatches
   `MamCommand::FetchTorrent`.  MAM core gains `FetchTorrent` command /
   `FetchTorrentBytes` action / `TorrentBytesFetched`/`Failed` events /
   `TorrentBytesReady`/`Failed` publishes; MAM shell wires
   `mam_client.fetch_torrent`.  qBit core's `AddTorrent` action shape
   changes from `{ mam_id, dl_url }` to `{ mam_id, bytes }`; qBit shell
   un-stubs and calls `qbit_client.add_torrent(cookie, bytes)`.  qBit
   publishes new `TorrentAdded` / `TorrentAddFailed`; domain handlers
   DOM-35 (MAM bytes → qBit add), DOM-36 (ManualDownload admission),
   DOM-37 (MAM fetch fail Warning + Activity), DOM-38 (qBit add success
   Info + Activity), DOM-39 (qBit add fail Warning + Activity).
   `WindlassMachine` gains `blacklisted_mam_ids: HashSet<MamTorrentId>`
   loaded at boot from the DB (`WindlassConfig::initial_blacklist`) and
   extended on every `DeadTorrentRemoved { mam_id: Some(_) }`.
   `ActivitySource::Download` added.
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
