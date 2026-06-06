# Integration-Test Audit (§34 phase 1)

**Historical document.**  This was §34's planning input — the
per-story coverage analysis of the pre-§34 suite and the punch list
of recommended new tests.  §34 the implementation has now landed
(see `docs/integration-tests.md` for how the harness works today and
`docs/operator-readiness.md` §34 for the locked architecture).

This doc is preserved as the audit trail of what was covered before,
what got reshaped under the contract-verification lens, and which
audit items have been delivered.

## How the suite was shaped (pre-§34)

Two integration-test crates:

- **`windlass/tests/integration.rs`** — 16 `#[tokio::test]` cases, all
  `#[ignore = "requires dev stack"]`, run against the real
  Postgres + chaos-controller + mock qBit/MAM stack.  **Deleted in
  §34 PR 4.**
- **`windlass-clients/tests/qbit_integration.rs`** — qBit client
  integration against a real qBittorrent container; still standalone
  in the post-§34 setup.

Chaos scenarios were available via `POST /scenario/{name}` on
`windlass-testkit/src/chaos.rs`:

| Scenario name              | Used by | Notes |
|----------------------------|---|---|
| `qbit-auth-fail`           | (defined, no test) | Bad credentials response on `/auth/login`. |
| `qbit-connection-refused`  | ✓ `qbit_connection_refused_windlass_stays_alive` | TCP refusal. |
| `mam-rate-limit`           | ✓ `mam_rate_limit_scenario_does_not_break_recovery` | 429 from MAM. |
| `mam-not-connectable`      | ✓ `mam_not_connectable_windlass_stays_alive` | `/jsonLoad.php` reports `connectable: "no"`. |
| `mam-asn-mismatch`         | ✓ `mam_asn_mismatch_windlass_stays_alive` | `/dynamicSeedbox.php` returns ASN-mismatch error. |

Chaos endpoints for Gluetun: `POST /gluetun/set-files`,
`POST /gluetun/health/{up|down}`.

The chaos controller, the WireMock-based mocks, and the 16 legacy
tests were all retired in §34 PRs 2-4.  The replacement architecture
lives in `docs/integration-tests.md`.

## Per-story coverage

Format: ✅ covered, ⚠️ partial, ❌ uncovered.  Status is against the
**integration-test** layer; per-machine property-test coverage is assumed
to be in `docs/invariants.md`.

| Story | Surface tested today | Gap |
|---|---|---|
| §1 (initial UI snapshot) | ✅ `windlass_state_endpoint_returns_system_state`, `boot_sequence_writes_system_snapshot_to_db` | None notable. |
| §5 / §6 / §7 (per-system runtimes) | ✅ `boot_sequence_authenticates_qbit`, `boot_sequence_syncs_port_to_51820`, `boot_sequence_updates_mam_seedbox` | None notable for boot. |
| §10 (property-test scaffolding) | n/a | Lives at machine layer by design. |
| §19 (HnR seed-time lock) | ❌ | No torrent-list scenario exercising the HnR-locked delete path. |
| §20 (zero-byte dead-torrent deletion) | ❌ | No dead-torrent fixture + verify-delete test. |
| §21 (no-partials enforcement) | ❌ | No newly-seen-torrent scenario verifying `SetAllFilesPriority`. |
| §22 (disk floor eviction) | ❌ | No chaos hook for disk pressure today. |
| §23 (qBit privacy auto-revert) | ❌ | No scenario that enables DHT/PeX/LSD and verifies the disable action. |
| §24 (queue orchestration) | ❌ | No fixture for hitting `max_active_torrents` and verifying pause+force-resume. |
| §25 (unsatisfied-quota gate) | ❌ | No scenario at-or-near the class cap. |
| §26 (upload-health gate) | ❌ | No `jsonLoad` fixture with `ratio < 2.0` / low `seedbonus`. |
| §27 (MAM keep-alive heartbeat) | ❌ | Recurring `FetchStatus` chain is timer-driven; no test asserts the chain stays alive, no test verifies `KeepAliveDegraded` Warning after 3 failures. |
| §28 (Unreachable vs NotConnectable) | ⚠️ | `mam_not_connectable_windlass_stays_alive` only smoke-checks `/health`; doesn't verify the DOM-15 Warning alert is written or that DOM-16 (Unreachable) writes Activity only. |
| §29 (fail-closed admission) | ❌ | No `TryAddTorrent` round-trip — neither the success path (Qbit AddTorrent action emitted) nor the blocked path (Activity listing failed gates). |
| §30 (ASN-mismatch) | ⚠️ | `mam_asn_mismatch_windlass_stays_alive` only smoke-checks `/health`; doesn't verify the DOM-20 Critical alert or that admission flips to `Some(false)`. |
| §31 (proactive seedbox update + verification) | ⚠️ | `chaos_gluetun_set_files_updates_state_and_resyncs_port` covers the **port** path; the **IP** path (Vpn `PublicIpObserved` → MAM `ObservedIpChanged` → `UpdateSeedbox`) is untested.  The 6h verification timer is uncovered. |
| §32 (registered-IP dedup + API hygiene) | ❌ | 1/hour client-side rate limit untested; ASN/AS storage from `dynamicSeedbox.php` response untested; `/jsonLoad.php?clientStats=` switch untested. |
| §33 (multi-source verification) | ❌ | No scenario for `MamIpVerified` / `MamIpVerifyFailed`; no test for source-named `PublicIpMismatch`. |
| §35 (Gluetun orchestration) | ❌ | No scenario producing a stale-namespace dependent; no test for the restart circuit breaker or crash-dump dedup. |

## Recommended new integration tests

Prioritised by §36 cutover risk (highest first — these are the
behaviors most coupled to the legacy compliance/torrent path that §36
will eventually remove).

### Tier 1 — must land before §36 step 7 (compliance retirement)

1. **Torrent-records DB persistence end-to-end.** Drive a
   `qbit-torrent-list` chaos scenario (one to add to the chaos
   controller — see "New chaos hooks" below) and assert
   `/api/v1/torrents` reflects the rows.  This is the persistence path
   `docs/legacy-retirement-plan.md` calls out as the compliance-removal
   blocker; an integration test makes the cutover bisectable.
2. **§29 admission gate, blocked path.** With MAM healthy but qBit
   privacy `dht: true`, issue `TryAddTorrent` through whatever entry
   point the web layer exposes (likely the manual-download route post-
   §29 wiring) and assert no `AddTorrent` HTTP call lands on qBit and
   an Activity entry naming the failed gate is written.
3. **§29 admission gate, success path.** Same flow with every gate
   satisfied — assert qBit observes a `/api/v2/torrents/add` request
   and the activity log records the snatch.
4. **§30 ASN-mismatch alert routing.** Extend
   `mam_asn_mismatch_windlass_stays_alive` to also assert: a `Critical`
   alert row appears in `/api/v1/alerts` with title `"MAM ASN
   mismatch"` and the `/api/v1/operator/state` view shows admission
   blocked.

### Tier 2 — high-value but not §36 blockers

5. **§31 IP-driven seedbox update.** Chaos `gluetun/set-files` with a
   fresh IP; assert MAM observes a new `/dynamicSeedbox.php` POST
   (mock should record), and that subsequent ticks dedup against the
   newly-registered IP (no second call within the 1/h window).
6. **§28 Unreachable vs NotConnectable distinction.** New chaos
   scenario `mam-status-network-error` (DNS-fail / TCP-refuse the
   `/jsonLoad.php` call); assert no Warning alert (Unreachable path
   gets Activity only per DOM-16) and admission stays at last-known
   value rather than flipping.  Contrast with the existing
   `mam-not-connectable` which should produce the Warning (DOM-15).
7. **§27 keep-alive degraded.** Apply `mam-rate-limit` for long
   enough to trigger 3 consecutive failures, assert a `Warning` alert
   with title `"MAM heartbeat failing"` (DOM-14) appears.
8. **§32 client-side 1/h rate limit.** Trigger two
   `UpdateSeedbox`-driving events within 5 seconds; assert MAM only
   sees the first POST.
9. **§33 multi-source verification.** Mock `/json/jsonIp.php`
   returning an IP different from Gluetun's file; assert a `Critical`
   `PublicIpMismatch` alert with title naming the MAM source and
   `admission.vpn_ip_compliant` flipped to `Some(false)`.

### Tier 3 — defensible to leave at property-test layer

10. §10 / §23 / §24 / §25 / §26 invariants (mostly per-machine
    output-shape checks already covered by proptest).  An integration
    test would re-verify what the property tests prove, at higher cost.
    Skip unless a regression motivates one.
11. §35 stale-namespace path.  The bollard wiring is currently
    stubbed (per §35's commit message); add an integration test only
    once a chaos hook can produce a real `StartedAt` predating
    Gluetun's `healthy_since`.

## Punch list reshape under the locked §34 purpose (2026-06-05)

§34's planning lock (operator-readiness.md) restated integration tests'
purpose as **contract verification between Windlass and its external
dependencies** — wire-format fidelity for services we don't own
(qBit, MAM, Gluetun), side effects across the trust boundary, and
real-I/O wiring between cores.  Pure behavior tests move to the
property-test layer.

Applied to the original punch list, with delivery status as of
2026-06-06:

| Audit # | Disposition under contract framing | Status |
|---|---|---|
| 1 (torrent-records persistence) | **Keep.**  Real qBit `/torrents/info` parses; fields round-trip to DB.  Uses a real magnet fixture. | ✅ `qbit_torrent_persists_to_db_via_api` (PR 5).  Uncovered + fixed a real bug in `torrent_state_str` along the way. |
| 2 (§29 blocked path) | **Keep, slim.**  Real qBit `/app/preferences` shape; flipping `dht: true` is observable; assert no `add` call lands. | ⏸ Deferred.  Requires a working manual-download flow with valid `.torrent` bytes from fake MAM; revisit when librarian work lands. |
| 3 (§29 success path) | **Keep.**  Real qBit `/torrents/add` accepts Windlass's multipart body. | ⏸ Deferred (same reason as #2). |
| 4 (§30 ASN-mismatch alert routing) | **Move to proptest.**  Pure behavior — fake MAM's ASN-mismatch shape parsing covered by the drift smoke pass. | ⏭ Proptest at `windlass-domain-core`. |
| 5 (§31 IP-driven seedbox update) | **Keep.**  Gluetun IP-file write triggers a fresh POST to fake MAM (the body itself is empty per `docs/mam-api.md`; the contract is "call happens after IP change"). | ✅ `gluetun_ip_change_triggers_new_seedbox_call` (PR 5). |
| 6 (§28 Unreachable vs NotConnectable) | **Move to proptest.**  Pure behavior. | ⏭ Proptest at `windlass-mam-core`. |
| 7 (§27 keep-alive degraded) | **Move to proptest.**  Timer-driven behavior. | ⏭ Proptest at `windlass-mam-core`. |
| 8 (§32 client-side 1/h rate limit) | **Keep.**  Fake MAM journal asserts exactly-one POST in window. | ✅ `seedbox_rate_limit_suppresses_second_call_within_hour` (PR 5). |
| 9 (§33 multi-source verification) | **Move to proptest.**  Alert wire is deferred per §34 lock #9; the `/jsonIp.php` shape itself is pinned by the mam_drift smoke pass. | ⏭ Proptest at `windlass-domain-core` / `windlass-vpn-core`. |

Plus contract tests beyond the original audit that landed as part of
the rebuild:

- `boot_authenticates_qbit`, `boot_syncs_default_port_to_qbit_preferences`,
  `boot_updates_mam_seedbox`, `boot_writes_system_snapshot_to_db`,
  `gluetun_set_files_resyncs_port_to_qbit` — the five §34 PR 4 ports
  from the pre-§34 suite.
- `qbit_endpoints_match_windlass_clients_types` — qBit API-drift
  smoke pass (PR 5).

## API-drift smoke pass (new class, introduced by §34)

For every qBit endpoint Windlass calls, a smoke test issues the call
against real qBit and asserts the response parses through
`windlass-clients`'s types.  A qBit version bump that changes a field
shape fails here loudly — before any operator test exercises the
field.

Equivalent pass over fake MAM: every endpoint's canned response
decodes through `windlass-clients`'s MAM types.  The fake is only as
good as the contract it encodes; this test pins that contract.

## Dropped chaos hooks (legacy plan)

The four chaos hooks the original plan called for are no longer
prerequisites; the new harness reshapes them:

- ~~`qbit-torrent-list`~~ — replaced by **real torrent fixtures**
  (small `.torrent` from a 1 KB local file, qBit reaches
  `Complete + Seeding` in seconds).
- ~~`mam-status-network-error`~~ — replaced by fake-MAM control-plane
  endpoint to inject TCP RST / connection drop on `/jsonLoad.php`.
- ~~`mam-jsonip-mismatch`~~ — replaced by fake-MAM control-plane
  setter for `/json/jsonIp.php`.
- ~~`mam-update-recorder`~~ — replaced by the fake-MAM request
  journal; tests query it directly.

## Behaviors intentionally left at property-test layer

- **Per-machine output-shape invariants.**  Every `[safety]` invariant
  in `docs/invariants.md` is covered by a fully-arbitrary-state
  property test.  Integration is for contract verification, not for
  re-verifying the per-event predicate.
- **Total-vs-reachable classification.**  Story 10's argument is that
  proptest covers the full `(state × event)` cross-product; integration
  cannot beat that.
- **Synthetic torrent states.**  qBit's web API doesn't let tests
  write `seed_time`, `downloaded_bytes`, or internal `state` fields.
  Tests that need synthetic state stay at proptest.
- **Notifications / alerts.**  The current notification surface is
  in-app only: `SendAlert` action → `alerts` DB table → `GET
  /api/v1/alerts`.  Trigger logic is proptested per core.  Integration
  coverage is deferred until external notification surfaces are
  added (Telegram / Pushover / webhook / SMTP); each new surface
  will get its own contract test against a fake notification
  endpoint in the testkit.
- **§35 stale-namespace / crash-recovery netns side effects.**
  Fake Gluetun cannot reproduce real netns invalidation on restart.
  Integration tests cover Windlass's reaction to "Gluetun
  unhealthy" signals from the fake; the real netns behavior is
  Docker's contract, not Windlass's, and is verified by proptest at
  the docker-core layer.

## Action plan

This document was §34 phase 1.  §34 phase 2 — the harness rebuild
plus the reshaped punch list — landed in 6 PRs through 2026-06-06.
For the current state of the suite see
`docs/integration-tests.md`.

Open follow-ups from the reshape:

- §29 admission gate (audit #2 + #3) needs the manual-download path
  to round-trip through fake MAM with valid `.torrent` bytes.
  Revisit when librarian work lands a torrent-bytes fixture in
  the testkit.
- Notifications (per §34 lock #9): when external surfaces are
  added (Telegram / Pushover / webhook / SMTP), each one gets its
  own contract test against a fake delivery endpoint in the
  testkit — same pattern as fake MAM.
