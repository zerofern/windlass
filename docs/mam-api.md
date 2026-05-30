# MAM API Reference

A consolidated reference for the MyAnonaMouse JSON API endpoints Windlass
touches (or plans to touch), with cross-references to the two open-source
projects whose patterns we follow: [Mousehole][mousehole] (the dynamic-seedbox
keeper) and [MLM][mlm] (the library manager / autograb).

This doc is the source of truth for endpoint shapes, rate limits, and known
response values — operator-readiness stories cross-reference it instead of
re-describing the API surface.

[mousehole]: https://github.com/t-mart/mousehole
[mlm]: https://github.com/stirlingmouse/MLM

## Endpoints we use

### `/json/dynamicSeedbox.php`

The dynamic-seedbox endpoint. Tells MAM what IP we're coming from now.

- **URL**: `https://t.myanonamouse.net/json/dynamicSeedbox.php`
- **Auth**: requires an IP- or ASN-locked session cookie (`mam_id`) with the
  *Set dynamic seedbox IP* permission, set up in MAM security preferences.
  Distinct from the browser session cookie.
- **Rate limit**: **once per hour (rolling window)**. We must enforce
  client-side too, otherwise we keep getting `429 Last Change too recent`.
- **Input**: none.
- **Output**:

  ```json
  {
    "Success": true,
    "msg": "Completed",
    "ip": "a.b.c.d",
    "ASN": 1234,
    "AS": "Org for 1234"
  }
  ```

- **Known `msg` values** (typed in code as `DynamicSeedboxMsg`):

  | HTTP | `msg`                                              | Meaning                              |
  |------|----------------------------------------------------|--------------------------------------|
  | 200  | `Completed`                                        | Update applied                       |
  | 200  | `No Change`                                        | IP already matches MAM's record      |
  | 429  | `Last change too recent`                           | Rate-limited (1/h rolling window)    |
  | 403  | `No Session Cookie`                                | mam_id not sent                      |
  | 403  | `Invalid session`                                  | mam_id rejected / outside locked IP  |
  | 403  | `Invalid session - IP mismatch`                    | IP-locked session, wrong IP          |
  | 403  | `Invalid session - ASN mismatch`                   | ASN-locked session, wrong ASN (§30)  |
  | 403  | `Invalid session - Invalid Cookie`                 | Bad/corrupted cookie                 |
  | 403  | `Incorrect session type - not allowed this function` | Session lacks dynamic-seedbox perm |
  | 403  | `Incorrect session type - non-API session`         | Browser session, not API session     |

- **Windlass use**:
  - §30 listens for `Invalid session - ASN mismatch` → `MamEvent::AsnMismatch`.
  - §31 calls this on every `MamCommand::ObservedIpChanged` and on the 24h
    `StaleRegistrationRefresh` timer; deduped against the last observed IP.
  - §32 will store `ip`/`ASN`/`AS` from successful responses as the
    "registered" trio in the MAM machine and dedup further updates against
    `registered_ip` (Mousehole semantics).

- **Mousehole reference**: schema is
  `{ Success, msg, ip (ipv4), ASN (number), AS (string) }`. Mousehole's
  `getUpdateReason()` compares `hostInfo.ip != lastMamResponse.body.ip`
  and `hostInfo.asn != lastMamResponse.body.ASN`.

### `/jsonLoad.php`

User stats endpoint. Lightweight, no `mam_id` IP-lock requirement.

- **URL**: `https://www.myanonamouse.net/jsonLoad.php`
- **Auth**: session cookie (any session works).
- **Rate limit**: none documented; safe at our 5-min keep-alive cadence.
- **Query params**:
  - `?clientStats` — adds `connectable` status (30-min cached server-side),
    plus client list and per-client torrent counts.
  - `?notif` — adds notification banner.
  - `?snatch_summary` — adds torrent breakdown (this is what MLM uses to
    track unsat counts and ratio for autograb).
  - `?id=<userid>` — load another user's public info instead.
  - `?pretty` — pretty-printed JSON.

- **Output (no query params)** — Windlass's current call:

  ```json
  {
    "classname": "VIP",
    "country_code": "dk",
    "country_name": "Denmark",
    "downloaded": "0.00 KiB",
    "downloaded_bytes": 0,
    "ratio": "∞",
    "seedbonus": 24425,
    "uid": 274455,
    "uploaded": "25.32 GiB",
    "uploaded_bytes": 27185934914,
    "username": "BrightVoyage",
    "vip_until": "2026-08-14 10:59:44",
    "wedges": 78
  }
  ```

- **Notes**:
  - `ratio` is a *string* (not f64) and may contain `"∞"` (∞) for VIPs
    with zero downloaded bytes. Our current `f64` parse fails on this.
  - `connectable` is **absent** without `?clientStats`. Our current code
    silently treats absence as `false`, which means §28's `NotConnectable`
    publish has been firing in steady state — a real bug. §32 fixes this
    by switching to `/jsonLoad.php?clientStats=`.
  - `?snatch_summary` adds an `unsat` block with `{ count, red, size, limit }`.
    MLM uses this for the autograb quota gate. Worth adopting later for
    §25 quota tracking.
  - **No IP or ASN field.** The registered IP/ASN only come from the
    dynamic-seedbox response.

- **Windlass use**:
  - §27 keep-alive heartbeat uses this every 5 min.
  - §26 upload-health gate reads `ratio` and `seedbonus` (as upload-credit
    proxy).

### `/json/checkCookie.php`

Session validation. Returns 200 on a valid session, non-200 on rejection.

- **URL**: `https://www.myanonamouse.net/json/checkCookie.php`
- **Rate limit**: none documented; we call it sparingly on boot.
- **Windlass use**: validates the configured `mam_id` at startup.

### `/json/jsonIp.php`

IP + ASN observation, as MAM sees us. **New for §32.**

- **URLs** (any works — useful for routing-redundancy checks):
  - `https://www.myanonamouse.net/json/jsonIp.php`
  - `https://t.myanonamouse.net/json/jsonIp.php`
  - `https://t1.myanonamouse.net/json/jsonIp.php` … `t4.myanonamouse.net`
- **Auth**: none required.
- **Rate limit**: **1 per minute**.
- **Output**:

  ```json
  { "ip": "51.254.0.0", "ASN": 16276, "AS": "OVH SAS", "time": 1776193859 }
  ```

- **Why we want it (§32)**: this is the *compliance* equivalent of
  ifconfig.co — it tells us what MAM sees as our IP/ASN, not what the
  generic public internet sees. The two are *usually* the same (both look
  at the connection's source IP) but disagreement is meaningful: an
  ifconfig.co vs MAM disagreement points at routing/proxy edge cases that
  Gluetun-only or world-only checks would miss.
- **§32 uses both `/json/jsonIp.php` and ifconfig.co** — the multi-source
  cross-check increases edge-case coverage. The 6h verification timer
  fires both checks; either disagreeing with Gluetun's file is a leak
  signal that flips the §29 admission gate.

## Endpoints we don't use yet

### `/json/bonusBuy.php`

Bonus-point store: buy VIP, upload, or freeleech wedges.

- **URL**: `https://www.myanonamouse.net/json/bonusBuy.php/{unix_ms}`
- **Query**: `spendtype=VIP|upload|wedges`, plus `amount` (upload) /
  `duration` (VIP) / `torrentid` (wedge).
- **Windlass use**: deferred to **librarian A1** (`try_wedge` cost rule
  for the bookmark autograb).

### `/tor/js/loadSearchJSONbasic.php`

Torrent search — the primary autograb input.

- **Methods**: GET or POST (JSON / form / multipart).
- **Returns**: list of torrents with all the fields the librarian needs
  (`id`, `title`, `numfiles`, `size`, `dl` hash, `free`, `vip`,
  `personal_freeleech`, `my_snatched`, `author_info`, `narrator_info`,
  `series_info`, `tags`, `description`, etc.).
- **Windlass use**: deferred to **librarian A1** (bookmark scan).

### `/tor/download.php/{dl_hash}` (or `?tid=<id>` with cookie)

`.torrent` file fetch.

- **Windlass use**: deferred to **librarian A1** (actually-add path; the
  qBit-shell `AddTorrent` stub will route through this).

### `/json/loadUserDetailsTorrents.php`

User's snatch list.

- **Windlass use**: deferred to **librarian A2** (the `already-snatched`
  gate; the §29 admission predicate consumes the `my_snatched` flag from
  the search response today, but a periodic snatch-list reconciliation
  would catch external snatches too).

### `/json/userBonusHistory.php`

Bonus-point and wedge history. Informational; no current Windlass use.

## Reference projects

### Mousehole (TypeScript)

- Repo: <https://github.com/t-mart/mousehole>
- Source-of-truth file for the dynamic-seedbox schema:
  `src/backend/types.ts` (`mamUpdateDynamicSeedboxResponseBodySchema`).
- Update-decision logic: `src/backend/update.ts` (`getUpdateReason`).
- Defaults: `CHECK_INTERVAL_SECONDS=300` (5 min cadence — same as our
  keep-alive),  `STALE_RESPONSE_SECONDS=86400` (force-update once per day —
  same as our `StaleRegistrationRefresh`).
- Schema: `{ Success: bool, msg: string, ip: ipv4, ASN: number, AS: string }`.
- Skip-update logic: skips when prior response exists, last HTTP was
  success, IP matches, ASN matches, cookie consistent, response not older
  than the stale window.

### MLM (Rust)

- Repo: <https://github.com/stirlingmouse/MLM>
- MAM client: `mlm_mam/src/api.rs`.
- Endpoints called: `/json/checkCookie.php`,
  `/jsonLoad.php?snatch_summary=true`, `/tor/download.php/{hash}`,
  `/tor/js/loadSearchJSONbasic.php`,
  `cdn.myanonamouse.net/json/loadUserDetailsTorrents.php`,
  `/json/bonusBuy.php`.
- **Does not** call the dynamic-seedbox endpoint — Mousehole owns that
  side; MLM is the library manager.
- Pattern worth borrowing: per-user-info caching with a TTL
  (`UserResponse` cached for 60s in `MaM::user_info`).

## Rate-limit summary

| Endpoint              | Limit                | Source           |
|-----------------------|----------------------|------------------|
| `dynamicSeedbox.php`  | 1 per hour (rolling) | MAM docs         |
| `jsonIp.php`          | 1 per minute         | MAM docs         |
| `jsonLoad.php`        | None documented      | n/a              |
| `checkCookie.php`     | None documented      | n/a              |
| `bonusBuy.php`        | None documented      | n/a (deferred)   |
| `loadSearchJSONbasic` | None documented      | n/a (deferred)   |

The Windlass MAM client enforces a 400 ms inter-request guard locally
(`MamClient::check_rate_limit`) on top of the per-endpoint limits.
§32 adds a 1-hour gate specifically for `dynamicSeedbox.php` so the
rolling-window 429 path becomes unreachable.
