# Integration-Test Audit (§34)

State of `just integration` after §27–§35 shipped.  This document is the
§34 deliverable: a per-story coverage table, a punch list of recommended
new integration tests, and the explicit list of behaviors we have decided
to keep at the property-test layer only.

Used by §36's per-handler cutover as the regression safety net.

## How the existing suite is shaped

Two integration-test crates:

- **`windlass/tests/integration.rs`** — 16 `#[tokio::test]` cases, all
  `#[ignore = "requires dev stack"]`, run against the real
  Postgres + chaos-controller + mock qBit/MAM stack.
- **`windlass-clients/tests/qbit_integration.rs`** — qBit client
  integration against a real qBittorrent container; not covered here.

Chaos scenarios available via `POST /scenario/{name}`
(`windlass-testkit/src/chaos.rs`):

| Scenario name              | Used by | Notes |
|----------------------------|---|---|
| `qbit-auth-fail`           | (defined, no test) | Bad credentials response on `/auth/login`. |
| `qbit-connection-refused`  | ✓ `qbit_connection_refused_windlass_stays_alive` | TCP refusal. |
| `mam-rate-limit`           | ✓ `mam_rate_limit_scenario_does_not_break_recovery` | 429 from MAM. |
| `mam-not-connectable`      | ✓ `mam_not_connectable_windlass_stays_alive` | `/jsonLoad.php` reports `connectable: "no"`. |
| `mam-asn-mismatch`         | ✓ `mam_asn_mismatch_windlass_stays_alive` | `/dynamicSeedbox.php` returns ASN-mismatch error. |

Chaos endpoints for Gluetun: `POST /gluetun/set-files`,
`POST /gluetun/health/{up|down}`.

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

## New chaos hooks needed

These don't exist yet in `windlass-testkit/src/chaos.rs` but are
prerequisites for the Tier 1/2 tests above.

- `qbit-torrent-list` — return a configurable torrent list from
  `/api/v2/torrents/info`, with fields for downloaded_bytes, seed_time,
  state.  Needed by tests 1, and indirectly by §19/§20/§21/§24/§25
  integration coverage.
- `mam-status-network-error` — make `/jsonLoad.php` respond with
  TCP RST / connection drop (not 5xx).  Needed by test 6.
- `mam-jsonip-mismatch` — `/json/jsonIp.php` returns an IP that
  disagrees with Gluetun's file.  Needed by test 9.
- `mam-update-recorder` — track every `/dynamicSeedbox.php` POST and
  its IP-source.  Needed by tests 5 and 8.

## Behaviors intentionally left at property-test layer

- **Per-machine output-shape invariants.**  Every `[safety]` invariant
  in `docs/invariants.md` is covered by a fully-arbitrary-state
  property test.  Integration is for wiring + ordering, not for
  re-verifying the per-event predicate.
- **Total-vs-reachable classification.**  Story 10's argument is that
  proptest covers the full `(state × event)` cross-product; integration
  cannot beat that.
- **Debug-mode replay** (deferred to §37).

## Action plan

This story (§34) **does not write** the new tests — that work is sized
per-tier and will land as its own commits.  This story:

- Produces this document (the audit + punch list).
- Identifies the **chaos-hook additions** needed before Tier 1 / Tier 2
  tests can be written.
- Provides §36 with a concrete safety-net plan: Tier 1 must precede
  step 7 of the cutover plan.

The new test-writing work tracks against §36's per-handler cutover
commits; expect Tier 1 tests to land in the same PRs as the cutover
steps that retire the corresponding legacy behavior.
