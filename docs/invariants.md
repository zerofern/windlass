# Operator Invariants

This document catalogs the rules the Windlass operator cores must always obey.
It exists for two reasons:

1. The invariants are worth writing down on their own — they are the contract
   each core promises to the rest of the system.
2. They are the specification the property-based tests (operator-readiness
   story 10) assert. Each invariant has a stable ID so a test can cite it.

## Scope

These invariants describe the sans-I/O cores and the generic service runtime:

- `windlass-vpn-core` (`VpnMachine`)
- `windlass-qbit-core` (`QbitMachine`)
- `windlass-mam-core` (`MamMachine`)
- `windlass-db-core` (`DbMachine`)
- `windlass-domain-core` (`WindlassMachine`)
- `windlass-machine` (`ServiceRuntime`)

They do **not** describe the legacy `windlass-core::SystemState`, which is being
retired in favor of these per-system machines.

## Invariant kinds

- **[safety]** — something bad never happens. Checkable after every single
  `handle` / `handle_command` call. These are the story-10 priority.
- **[liveness]** — something good eventually happens across a sequence (e.g. a
  retry is re-issued). Harder to test; deferred to a story-10 follow-up.
- **[purity]** — the machine is a pure function of (state, event): no I/O, no
  blocking, no panics, bounded time.

## A note on shell contracts

A machine can only guarantee its invariants if the shell reports honest events.
Where a machine invariant depends on a shell promise, it is listed under
**Shell contracts** for that machine. These are assumptions the property tests
encode in their event generators (e.g. "a `ListenPortSet` event only follows a
`SetListenPort` action for the desired port").

---

## Global invariants (all machines)

- **GLOBAL-1 [purity]** No `handle` or `handle_command` call panics, for any
  reachable state and any event or command.
- **GLOBAL-2 [purity]** A single `handle` / `handle_command` call returns within
  a small bounded time (no blocking I/O, sleeps, or accidentally quadratic
  work). Inherited from the existing `windlass-core` timing properties.
- **GLOBAL-3 [safety]** A machine never emits an action that carries a
  credential or port it does not currently hold. Concretely: no action carrying
  an `AuthCookie` is emitted while unauthenticated; no published port value is
  one the machine does not currently believe is active or desired (see per-core
  port-publish rules).
- **GLOBAL-4 [safety]** Port authority flows in one direction only: VPN observes
  the forwarded port → domain → qBit/MAM. No core other than VPN ever invents a
  port; qBit and MAM only converge on a port handed to them.

---

## VPN machine (`VpnMachine`)

State: `connected: bool`, `port: Option<VpnPort>`.

- **VPN-1 [safety]** `ContainerUnhealthy` always clears the port
  (`port == None`), sets `connected == false`, and publishes both `Disconnected`
  and `PortUnavailable`. A VPN death never leaves a stale forwarded port behind.
- **VPN-2 [safety]** Every published `PortReady { port }` carries a port equal to
  the machine's current `self.port`, and is only published when `self.port` is
  `Some`.
- **VPN-3 [safety]** `PortFileChanged { port }` sets `self.port = Some(port)` and
  publishes exactly `PortReady { port }` for that same port.
- **VPN-4 [safety]** Connectivity and port publishes from `StateRead` are
  consistent with the values written to state: `Connected` iff `connected`,
  `PortReady` iff `port.is_some()`, else `Disconnected` / `PortUnavailable`.
- **VPN-5 [safety]** `StateReadFailed` schedules exactly one `PortReadRetry`
  timer, mutates no state, and publishes nothing.
- **VPN-6 [safety]** Health polling is side-effect free on state:
  `TimerFired(HealthPoll)` emits only `InspectContainer`, with no state mutation
  and no publish.
- **VPN-7 [safety]** `PublicIpChanged` is a no-op: no state change, no actions,
  no publishes.

Shell contracts:

- A `StateRead { connected: false, port: Some(..) }` event is not expected; the
  shell should not report a disconnected VPN that still has a port. If it does,
  VPN-4 would publish `Disconnected` alongside `PortReady`. The generator should
  not produce this combination unless we decide the machine must defend against
  it.

---

## qBit machine (`QbitMachine`)

State: `cookie: Option<AuthCookie>`, `listen_port: Option<VpnPort>`,
`desired_listen_port: Option<VpnPort>`, `refresh_scheduled: bool`.

- **QBIT-1 [safety]** No cookie-bearing action (`ReadPreferences`,
  `SetListenPort`, `ListTorrents`, `PauseTorrent`, `ResumeTorrent`) is ever
  emitted while `cookie == None`. When unauthenticated, port/auth paths emit
  `Login` instead and torrent paths emit nothing.
- **QBIT-2 [safety]** The self-perpetuating `TorrentRefresh` timer chain is
  started at most once. Repeated `AuthSucceeded` events (e.g. a dual-init login
  race) never spawn a second chain (`refresh_scheduled` guard).
- **QBIT-3 [liveness]** Once started, the `TorrentRefresh` timer always
  re-schedules itself, so the refresh chain never dies.
- **QBIT-4 [safety]** `ListenPortReady { port }` is only published for a port
  equal to `desired_listen_port` (or when no port is desired). The machine never
  advertises a listen port that disagrees with the desired target.
- **QBIT-5 [safety]** Every retryable failure (`AuthFailed`,
  `PreferencesFailed`, `ListenPortSetFailed`) schedules exactly one retry timer
  (`AuthRetry` or `SyncRetry`) and publishes `Unavailable`. No failure path
  emits an immediate retry action (no tight loop).
- **QBIT-6 [safety]** `EnsureListenPort { port }` records `desired_listen_port`
  and, if already converged (`listen_port == Some(port)`), emits no action and
  publishes `ListenPortReady { port }`; otherwise it emits `SetListenPort` (with
  cookie) or `Login` (if unauthenticated), never both.
- **QBIT-7 [liveness]** While `desired_listen_port` is `Some(p)` and
  `listen_port != Some(p)`, a retry path eventually re-issues `SetListenPort` for
  `p` (given continued retry timer fires and a cookie).

Shell contracts:

- A `ListenPortSet { port }` event is only delivered as the success result of a
  `SetListenPort` action the machine issued, so `port` equals the desired
  target. The `ListenPortSet` arm publishes `ListenPortReady { port }` directly
  (not through the desired-port filter), so this contract is what keeps it
  consistent with QBIT-4.

---

## MAM machine (`MamMachine`)

State: `authenticated: bool`, `seedbox_port: Option<VpnPort>`,
`desired_seedbox_port: Option<VpnPort>`.

- **MAM-1 [safety]** `SeedboxPortReady { port }` is only published for a port
  equal to `desired_seedbox_port` (or when no port is desired). MAM never
  advertises a seedbox port that disagrees with the desired target.
- **MAM-2 [safety]** Every retryable failure (`AuthFailed`, `StatusFailed`,
  `SeedboxUpdateFailed`) schedules exactly one `StatusRetry` timer and publishes
  `Unavailable`. No failure path emits an immediate retry action.
- **MAM-3 [safety]** `RateLimited { retry_after }` schedules exactly one
  `RateLimitExpired` timer for `retry_after` and publishes
  `RateLimited { retry_after }`. The machine backs off rather than retrying
  immediately.
- **MAM-4 [safety]** `EnsureSeedboxPort { port }` records `desired_seedbox_port`
  and, if already converged (`seedbox_port == Some(port)`), emits no action and
  publishes `SeedboxPortReady { port }`; otherwise it emits exactly
  `UpdateSeedbox`.
- **MAM-5 [safety]** `SeedboxUpdated` with no desired port is a no-op (no state
  change, no publish). With a desired port `p`, it sets `seedbox_port = Some(p)`
  and publishes `SeedboxPortReady { p }`.
- **MAM-6 [liveness]** While `desired_seedbox_port` is `Some(p)` and
  `seedbox_port != Some(p)`, a retry path eventually re-issues `UpdateSeedbox`
  for `p`.

---

## DB machine (`DbMachine`)

Stateless.

- **DB-1 [safety]** Every command produces exactly one `Execute(cmd)` action and
  the response `Accepted`, with no publishes.
- **DB-2 [safety]** Every `DbEvent` produces exactly one publish (`Failed` for
  `DbEvent::Failed`, `Succeeded` otherwise) and **no actions**.
- **DB-3 [safety]** DB failure handling cannot recurse: because `handle` never
  emits an action, a `DbEvent::Failed` can never trigger another DB command from
  within the DB machine. Combined with DOM-4, the
  `DB fails → domain → DB` loop is structurally broken.

---

## Domain machine (`WindlassMachine`)

State: `SystemStateView { vpn, qbit, mam: ServiceStatus, forwarded_port:
Option<VpnPort> }`.

- **DOM-1 [safety]** *(marquee invariant)* The domain never commands qBit or MAM
  to converge on a port unless VPN currently has that forwarded port.
  Concretely: whenever an outcome's actions contain
  `Qbit(EnsureListenPort { port })` or `Mam(EnsureSeedboxPort { port })`, then
  immediately after that call `self.state.forwarded_port == Some(port)`.
- **DOM-2 [safety]** `Vpn(Disconnected)` and `Vpn(PortUnavailable)` clear
  `forwarded_port` to `None`. Loss of VPN connectivity always drops the
  forwarded port.
- **DOM-3 [safety]** `Vpn(PortReady { port })` sets
  `forwarded_port = Some(port)` and emits convergence commands for qBit and MAM
  carrying that exact `port`.
- **DOM-4 [safety]** `DbFailed` emits no action — only an `Activity` publish. The
  domain never reacts to a DB failure by issuing more DB work (see DB-3).
- **DOM-5 [liveness]** The `Snapshot` timer always re-schedules itself, so
  periodic snapshotting never stops.
- **DOM-6 [safety]** The `Refresh` command fans out to exactly one refresh action
  per service (`Vpn::RefreshState`, `Qbit::RefreshTorrents`,
  `Mam::RefreshStatus`).
- **DOM-7 [safety]** Every event that changes `SystemStateView` publishes a
  `SystemState` snapshot, so subscribers never miss an observable state change.

---

## Service runtime (`ServiceRuntime`)

- **RT-1 [safety]** Each received event invokes `Machine::handle` exactly once;
  each received command invokes `Machine::handle_command` exactly once and sends
  exactly one response on its reply channel.
- **RT-2 [safety]** Actions returned by the machine are dispatched to the shell
  in order, and publishes are routed to the topic fanout. The runtime adds,
  drops, or reorders nothing.
- **RT-3 [safety]** The loop exits cleanly once both the event and command
  channels are closed.
- **RT-4 [liveness/determinism]** For a pure machine, replaying the same event
  sequence from the same initial state yields the same final state and the same
  sequence of actions and publishes. (Testable once debug-mode replay is
  integrated into the runtime; deferred per story 10.)
