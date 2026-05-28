# Operator Invariants

This document is organised in two levels:

1. **Product guarantees** (A-G) — what the Windlass operator promises the user,
   in plain terms, and *why each one matters*. This is the layer to read first.
2. **Technical invariants** — the per-machine, code-level rules that *enforce*
   the guarantees. These are what the property tests actually assert
   (operator-readiness story 10 and the per-invariant stories). Each is tagged
   with the guarantee(s) it serves.

A guarantee can be backed by both already-implemented invariants and ones that
arrive with later operator-readiness stories; unbuilt pieces are marked.

## Scope

This covers the sans-I/O cores and the generic service runtime:
`windlass-vpn-core` (`VpnMachine`), `windlass-qbit-core` (`QbitMachine`),
`windlass-mam-core` (`MamMachine`), `windlass-db-core` (`DbMachine`),
`windlass-domain-core` (`WindlassMachine`), `windlass-machine`
(`ServiceRuntime`). It does **not** describe the legacy
`windlass-core::SystemState`, which is being retired.

---

## Product guarantees

### Guarantee A — Never risk the tracker account

No autonomous action Windlass takes can get the user's MAM account banned or
penalised.

*Why it matters:* a single hit-and-run, a partial download, or a leaked
DHT/PeX setting can cost the account. The operator must be incapable of these,
not merely unlikely to do them.

*Enforced by:* HnR seed-time lock (§19), zero-byte-only deletion (§20),
no-partials (§21), privacy auto-revert (§23), unsatisfied-quota gate (§25),
upload-health gate (§26), VPN-IP-compliance gate (§30), fail-closed download
admission (§29). *(All not-yet-implemented — these are the operator-readiness
compliance stories.)*

### Guarantee B — Stay inside the VPN, always

The dependent stack (qBittorrent, etc.) never sends traffic outside the VPN
tunnel. If network isolation is uncertain, the operator stops rather than risk a
leak.

*Why it matters:* a leak deanonymises the user and exposes the real IP to the
tracker and peers.

*Enforced by:* VPN-1 + DOM-2 (a VPN drop immediately clears downstream port/
state), and Gluetun stack orchestration — no dependent on a stale namespace, no
start before Gluetun is healthy and IP-compliant (§31, not yet implemented).

### Guarantee C — What we advertise is the real forwarded port

The user never has to manually fix qBittorrent's listen port. The port qBit
listens on, and the port reported to MAM, are always the VPN's *actual* current
forwarded port — and only the VPN can originate a port; qBit and MAM only
converge on a port handed to them.

*Why it matters:* a stale or wrong port makes MAM mark the client
unconnectable and seeding silently breaks, hurting ratio.

*Enforced by:* VPN-2/VPN-3 (only ever publish a port it actually holds), DOM-1 +
DOM-3 (domain only converges the VPN's forwarded port, never a port it doesn't
hold), QBIT-4 / MAM-1 (never advertise a port that disagrees with the desired
target), and the one-directional VPN → domain → qBit/MAM data flow. *(This
guarantee subsumes what were previously the abstract GLOBAL-3 "no action carries
a credential/port it doesn't hold" and GLOBAL-4 "port authority is
one-directional" — they are this promise plus its enforcing invariants, not
separate test targets.)*

### Guarantee D — Under uncertainty, do nothing risky (fail closed)

When state is unknown, stale, or a dependency is unhealthy, the operator
declines to take risky autonomous actions rather than guessing.

*Why it matters:* guessing under uncertainty is how an automated operator does
damage a human never would.

*Enforced by:* QBIT-1 (no authenticated action without a cookie), MAM
not-connectable / unreachable handling (§28), the fail-closed admission predicate
and unknown-MAM-health gate (§29, not yet implemented).

### Guarantee E — Never silently lose the user's data or history

Deleting a torrent's media never deletes the user's reviews, ratings, or
listening history.

*Why it matters:* media is replaceable; the user's reading ledger and reviews
are not.

*Enforced by:* the no-history-cascade rule on media deletion and `reading_ledger`
retention (§22, not yet implemented; preferably structural — no such delete
action exists).

### Guarantee F — Always recover, never wedge or spin

Transient failures are retried with backoff; the operator never tight-loops,
never storms restarts, and its background work (torrent refresh, keep-alive,
snapshots) never silently dies.

*Why it matters:* an operator that wedges needs babysitting; one that storms
makes outages worse.

*Enforced by:* QBIT-5 / MAM-2 / MAM-3 (failures schedule a single backed-off
retry, never an immediate one), QBIT-3 + DOM-5 (self-perpetuating refresh /
snapshot chains never stop), DB-2 + DB-3 (DB failure handling emits no action,
so it cannot recurse), plus the restart circuit breaker and crash-dump-once
rules (§31) and MAM keep-alive (§27) — last two not yet implemented.

### Guarantee G — The dashboard always shows current truth

Opening or refreshing the UI shows the real current operator state immediately,
and every state change is published to subscribers.

*Why it matters:* an operator the user can't trust to reflect reality is one
they stop trusting.

*Enforced by:* initial state snapshot on SSE connect (story 1, implemented) and
DOM-7 (every observable state change publishes a `SystemState` snapshot).

---

## Technical invariants

These are the testable rules the property tests assert. Tag key:
- **[safety]** — something bad never happens; checkable after a single
  `handle` / `handle_command`.
- **[liveness]** — something good eventually happens across a sequence; harder to
  test, deferred (see story 10).
- **[purity]** — pure function of (state, event): no I/O, no blocking, no panic,
  bounded time.

Each invariant ends with `→` the guarantee(s) it serves.

### Global (all machines)

- **GLOBAL-1 [purity]** No `handle` / `handle_command` call panics, for any state
  and any event/command. → underpins all guarantees.
- **GLOBAL-2 [purity]** A single call returns within a small bounded time (no
  blocking I/O, sleeps, or accidentally quadratic work). → underpins all.

(The former GLOBAL-3 and GLOBAL-4 are now expressed as **Guarantee C** plus its
enforcing per-machine invariants, since neither was a single testable property —
GLOBAL-3 was an umbrella over QBIT-1/QBIT-4/MAM-1/VPN-2, and GLOBAL-4 was a
structural/composition property the type system enforces.)

### VPN machine (`VpnMachine`)

State: `connected: bool`, `port: Option<VpnPort>`.

- **VPN-1 [safety]** `ContainerUnhealthy` clears the port, sets
  `connected == false`, and publishes both `Disconnected` and `PortUnavailable`.
  A VPN death never leaves a stale forwarded port. → B, C
- **VPN-2 [safety]** Every published `PortReady { port }` carries `self.port`, and
  is only published when `self.port` is `Some`. → C
- **VPN-3 [safety]** `PortFileChanged { port }` sets `self.port = Some(port)` and
  publishes exactly `PortReady { port }`. → C
- **VPN-4 [safety]** `StateRead` publishes are consistent with the values written
  to state: `Connected` iff `connected`, `PortReady` iff `port.is_some()`. → C
- **VPN-5 [safety]** `StateReadFailed` schedules exactly one `PortReadRetry`
  timer, mutates no state, publishes nothing. → F
- **VPN-6 [safety]** Health polling is side-effect free on state:
  `TimerFired(HealthPoll)` emits only `InspectContainer`. → F
- **VPN-7 [safety]** `PublicIpChanged` is currently a no-op. (Will gain meaning
  with the IP-compliance gate, §30 / Guarantee A.) → A (future)

Shell contracts:

- `StateRead { connected: false, port: Some(_) }` is not expected from the
  shell, but the machine now **defends** against it regardless: a disconnected
  `StateRead` always clears the port and publishes `PortUnavailable`, enforcing
  VPN-1 for any event sequence. This is an enforced invariant, not a trusted
  contract. All four `connected × port` shapes are covered by explicit example
  unit tests. VPN-4 is accordingly strengthened: a disconnected `StateRead`
  never publishes `PortReady`, even if the shell reports a port.

  Updated **VPN-4**: `StateRead` publishes are consistent with the values written
  to state — `Connected` iff `connected == true`, `PortReady { port }` iff
  `connected == true` and `port.is_some()`. When `connected == false`, the port
  is always cleared and `PortUnavailable` is published regardless of the
  reported `port` field.

### qBit machine (`QbitMachine`)

State: `cookie: Option<AuthCookie>`, `listen_port: Option<VpnPort>`,
`desired_listen_port: Option<VpnPort>`, `refresh_scheduled: bool`,
`torrents: HashMap<TorrentHash, TorrentRecord>` (per-torrent seed-time and
download tracking; populated on every `TorrentsListed` event),
`privacy: PrivacySettings { dht, pex, lsd }` (last-observed privacy settings;
all must be false per MAM Rule 6.1).

Topics: `Availability`, `ListenPort`, `Torrents`, `Privacy` (§23).

- **QBIT-1 [safety]** No cookie-bearing action (`ReadPreferences`,
  `SetListenPort`, `ListTorrents`, `PauseTorrent`, `ResumeTorrent`,
  `DeleteTorrent`, `SetAllFilesPriority`, `DisableBannedPrivacySettings`) is
  emitted while `cookie == None`. → C, D
- **QBIT-2 [safety]** The `TorrentRefresh` timer chain is started at most once;
  repeated `AuthSucceeded` never spawns a second chain. → F
- **QBIT-3 [liveness]** Once started, the `TorrentRefresh` timer always
  re-schedules itself. → F
- **QBIT-4 [safety]** `ListenPortReady { port }` is only published for a port
  equal to `desired_listen_port` (or when none is desired). → C
- **QBIT-5 [safety]** Every retryable failure (`AuthFailed`, `PreferencesFailed`,
  `ListenPortSetFailed`) schedules exactly one retry timer and publishes
  `Unavailable`; no immediate-retry action. → F
- **QBIT-6 [safety]** `EnsureListenPort { port }` records the desired port and
  either publishes `ListenPortReady` when already converged or emits one of
  `SetListenPort` / `Login`, never both. → C
- **QBIT-7 [liveness]** While desired ≠ current, a retry path eventually
  re-issues `SetListenPort`. → C, F
- **QBIT-8 [safety]** *(HnR seed-time lock — §19)* No `DeleteTorrent` action is
  ever emitted for a torrent that is known to the machine with
  `downloaded_bytes > 0 && seed_time < hnr_seed_time`. A torrent is deletable
  only when: it is unknown to the machine, or `downloaded_bytes == 0` (zero-byte
  — nothing was downloaded), or `seed_time >= hnr_seed_time` (fully
  `HnR`-satisfied). The machine has no cookie → no action at all. This invariant
  is total (holds for any generated machine state, including unreachable ones). → A
- **QBIT-9 [safety]** *(Zero-byte dead-torrent deletion — §20)* Every
  `DeleteTorrent` action emitted by the `TorrentsListed` dead-torrent path
  targets a torrent whose `downloaded_bytes == 0`. Stalled/error/paused torrents
  with any downloaded data are never auto-deleted by this path; they fall under
  the HnR seed-time lock (QBIT-8) instead. A dead torrent is zero-byte by
  definition, so QBIT-8 and QBIT-9 compose: the gate allows it whenever a
  cookie is present. This invariant is total. → A
- **QBIT-10 [safety]** *(No-partials enforcement — §21)* Every newly-seen
  torrent (a hash not present in `self.torrents` before a `TorrentsListed`
  event) triggers exactly one `SetAllFilesPriority { hash, .. }` action when
  `cookie` is `Some`, and none when `cookie` is `None`. A hash already in
  `self.torrents` never triggers `SetAllFilesPriority` again (fire-once
  semantics). This invariant is total. → A

  *Note:* the broader `AddTorrent { file_selection == All }` invariant from
  story 21's acceptance criteria is deferred to story 29 (fail-closed download
  admission control), when `AddTorrent` is introduced.

Shell contract: `ListenPortSet { port }` is now routed through the
desired-port filter (`listen_port_publish`), so QBIT-4 holds for any event —
including a dishonest `ListenPortSet` carrying a port that differs from the
desired target. This is an enforced invariant, not a trusted contract. The
QBIT-4 property test uses an unconstrained generator.

### MAM machine (`MamMachine`)

State: `authenticated: bool`, `seedbox_port: Option<VpnPort>`,
`desired_seedbox_port: Option<VpnPort>`.

- **MAM-1 [safety]** `SeedboxPortReady { port }` is only published for a port
  equal to `desired_seedbox_port` (or when none is desired). → C
- **MAM-2 [safety]** Every retryable failure schedules exactly one `StatusRetry`
  and publishes `Unavailable`; no immediate-retry action. → F
- **MAM-3 [safety]** `RateLimited { retry_after }` schedules one
  `RateLimitExpired` timer for `retry_after` and publishes `RateLimited`; the
  machine backs off. → F
- **MAM-4 [safety]** `EnsureSeedboxPort { port }` records the desired port and
  either publishes `SeedboxPortReady` when already converged or emits exactly
  `UpdateSeedbox`. → C
- **MAM-5 [safety]** `SeedboxUpdated` with no desired port is a no-op; with a
  desired port `p` it sets `seedbox_port = Some(p)` and publishes
  `SeedboxPortReady { p }`. → C
- **MAM-6 [liveness]** While desired ≠ current, a retry path eventually re-issues
  `UpdateSeedbox`. → C, F

### DB machine (`DbMachine`)

Stateless.

- **DB-1 [safety]** Every command produces exactly one `Execute(cmd)` action and
  `Accepted`, no publishes. → (mechanism)
- **DB-2 [safety]** Every `DbEvent` produces exactly one publish and **no
  actions**. → F
- **DB-3 [safety]** DB failure handling cannot recurse: `handle` never emits an
  action, so a `DbEvent::Failed` can never trigger another DB command. With
  DOM-4, the `DB fails → domain → DB` loop is structurally broken. → F

### Domain machine (`WindlassMachine`)

State: `SystemStateView { vpn, qbit, mam: ServiceStatus, forwarded_port:
Option<VpnPort> }`.

- **DOM-1 [safety]** *(marquee)* Whenever an outcome's actions contain
  `Qbit(EnsureListenPort { port })` or `Mam(EnsureSeedboxPort { port })`, then
  immediately after, `forwarded_port == Some(port)`. The domain never converges a
  port the VPN does not currently have. → C
- **DOM-2 [safety]** `Vpn(Disconnected)` and `Vpn(PortUnavailable)` clear
  `forwarded_port`. → B, C
- **DOM-3 [safety]** `Vpn(PortReady { port })` sets `forwarded_port = Some(port)`
  and converges qBit and MAM to that exact port. → C
- **DOM-4 [safety]** `DbFailed` emits no action — only an `Activity` publish. → F
- **DOM-5 [liveness]** The `Snapshot` timer always re-schedules itself. → F
- **DOM-6 [safety]** `Refresh` fans out to exactly one refresh action per service.
  → (mechanism)
- **DOM-7 [safety]** Every event that changes `SystemStateView` publishes a
  `SystemState` snapshot. → G
- **DOM-8 [safety]** *(Dead-torrent blacklist — §20)* A
  `Qbit(DeadTorrentRemoved { mam_id: Some(id) })` event emits exactly one
  `Db(MarkDownloadState { mam_id: id, status: Blacklisted })` action and
  exactly one `Activity` publish. A `DeadTorrentRemoved { mam_id: None }` event
  emits no action and no publish. → A

### Disk machine (`DiskMachine`)

State: `free_bytes: Option<u64>`.

- **DISK-1 [safety]** *(disk floor — §22)* `BelowFloor { free_bytes }` is
  published iff `free_bytes < config.hard_floor_bytes`; otherwise `AboveFloor`.
  Total invariant. → A

### qBit machine additions (§22)

- **QBIT-11 [safety]** *(disk-pressure eviction gate — §22)*
  `EvictOneForDiskPressure` emits at most one `DeleteTorrent`, and only for a
  known HnR-satisfied torrent; composes with QBIT-8. The selected candidate has
  the largest `seed_time` among satisfied torrents (placeholder rank). → A
- **QBIT-12 [safety]** *(banned privacy auto-revert — §23)* A `PreferencesRead`
  with any of `dht|pex|lsd` true emits exactly one `DisableBannedPrivacySettings`
  action when `cookie == Some` and publishes `BannedPrivacySettingsObserved`
  (regardless of cookie — the domain needs to alert even when unauthenticated).
  No banned setting → no action, no publish. Total invariant. → A
- **QBIT-13 [safety]** *(privacy retry — §23)* `PrivacySettingsDisableFailed`
  (merged into the shared retryable-failures arm with QBIT-5) schedules exactly
  one `SyncRetry` and publishes `Unavailable`; no immediate retry. → F

### Domain machine additions (§22)

- **DOM-9 [safety]** *(disk-pressure routing — §22)* `Disk(BelowFloor)` produces
  exactly one `Qbit(EvictOneForDiskPressure)` and one `Activity` publish;
  `Disk(AboveFloor)` produces nothing. → A

### qBit machine additions (§23)

- **QBIT-12 [safety]** *(see above)*
- **QBIT-13 [safety]** *(see above)*

### Domain machine additions (§23)

- **DOM-10 [safety]** *(privacy alert routing — §23)*
  `Qbit(BannedPrivacySettingsObserved { any true })` emits exactly one
  `Db(RecordAlert{ priority: Critical })`, one `Db(RecordActivity)`, and one
  `Activity` publish. Total invariant. → A

### Deferred rank classes and structural invariants (§22)

The four real deletion-value rank classes — (1) completed + low rating (≤2★)
+ HnR-satisfied, (2) DNF + HnR-satisfied, (3) completed + high rating but long
since listened + HnR-satisfied, (4) unstarted + long wait + low AI score —
require librarian data outside operator scope and are deferred to librarian
integration. The current placeholder rank (longest `seed_time` first among
HnR-satisfied torrents) holds the spot until then.

The "no history cascade" invariant is structurally satisfied: no
`DeleteReadingLedger` or `DeleteReview` action variants exist in any of the new
core crates (`windlass-disk-core`, `windlass-qbit-core`, `windlass-domain-core`,
or any other `windlass-*-core`). There is no such action to emit, so Guarantee E
is enforced by the type system.

### Service runtime (`ServiceRuntime`)

Test coverage deferred (async; see story 10). → underpins all.

- **RT-1 [safety]** Each event invokes `handle` once; each command invokes
  `handle_command` once and sends exactly one response.
- **RT-2 [safety]** Actions are dispatched to the shell in order; publishes are
  routed to the fanout; nothing is added, dropped, or reordered.
- **RT-3 [safety]** The loop exits cleanly once both channels close.
- **RT-4 [liveness/determinism]** For a pure machine, replaying the same event
  sequence from the same initial state yields the same state, actions, and
  publishes. (Testable once debug-mode replay lands; deferred.)
