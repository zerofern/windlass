# MAM account safety and bonus points

Status: Draft
Feature ID: B-0011

## Goal

The owner bookmarks torrents on MAM and Windlass does everything else: it
downloads, seeds, and cleans up without ever putting the account outside MAM's
rules. Downloads never cost ratio; bonus points pay for freeleech wedges, and
for VIP and upload credit when they are needed.

## Principles

1. **MAM is the source of truth.** Every invariant is checked against what MAM
   reports, because that is what MAM enforces. Windlass's own records only fill
   the gap between two MAM readings, such as a download admitted since the last
   reading, and never relax a limit MAM reports. When MAM's reading is missing,
   stale, or not understood, the gates it feeds stay closed.
2. **Downloads never cost ratio.** Every torrent is downloaded as a personal
   freeleech, using a wedge, or as a VIP torrent while the account is VIP. If
   neither is possible, the torrent waits.
3. **Gates and watches.** A *gate* is checked before an action, so the action
   never happens when it would break a rule. A *watch* is checked continuously,
   so the owner hears about drift, and Windlass repairs what it can, long before
   MAM acts.

## Sources

- MAM rules, revisions to 2026-10-10: 1.2 duplicate IPs and VPNs, 1.4 ratio, 1.6
  inactive accounts, 1.7 automation, 2.2 reuse of torrent files, 2.3 clients,
  2.4 global ratio, 2.5 seeding, 2.7 partials, 2.8 unsatisfied torrents, 4.4
  sharing files and begging.
- MAM API documentation: Load User Data (`/jsonLoad.php`), Torrent Search
  (`/tor/js/loadSearchJSONbasic.php`), Download Torrent (`/tor/download.php`),
  Bonus Points Store (`/json/bonusBuy.php`), User Bonus History
  (`/json/userBonusHistory.php`), IP lookup (`/json/jsonIp.php`), and Dynamic
  Seedbox (`/json/dynamicSeedbox.php`). The owner keeps these in a private
  artifact ([references](../../references.md#mam-rules-and-api-documentation)).

## Invariants

### Account standing

| ID | Rule | Invariant | Kind | Verified with MAM | How Windlass keeps it |
| --- | --- | --- | --- | --- | --- |
| A1 | 1.4, 2.4 | Global ratio stays at or above 1.0. | Watch | Load User Data `ratio`, `uploaded`, `downloaded` | Downloads never cost ratio (principle 2), so the ratio should only rise. If it falls below the target of 3.0, Windlass buys upload credit (spending policy). Below 2.0 it notifies and holds every download until it understands why; below 1.1 the notice is urgent. |
| A2 | 2.8 | Unsatisfied torrents stay below the class limit: 20 for new members, 50 User, 100 Power User, 150 VIP, 200 above VIP. | Gate, watch | Load User Data `snatch_summary` for the count, `classname` for the limit | Admit only if MAM's unsatisfied count, plus torrents Windlass admitted since that reading, plus 1, stays at least the reserve below the limit for MAM's class. An unrecognised class or a missing count closes the gate. Crossing the limit blocks downloads for up to 24 hours. Notify at 80% of the limit. |
| A3 | 2.5 | Every downloaded torrent seeds 72 hours within 30 days of completion. | Gate, watch | MAM's satisfied status for the torrent (see the open questions) | Windlass never stops, pauses, or removes a torrent until MAM shows it satisfied and qBittorrent shows 72 hours plus the margin. The watch notifies when a torrent completed 20 or more days ago has not yet seeded 72 hours, which happens if seeding is interrupted, for example while the VPN is down. |
| A4 | 2.7 | No partial torrents. | Gate | (none needed) | Every file stays selected (implemented). |
| A5 | 2.3 | Only approved, release versions of qBittorrent; no auto-update. | Gate, watch | Load User Data `clientStats` | The NixOS module refuses other versions and pins the package (implemented). Windlass also checks that MAM lists the client it sees as the running qBittorrent and does not report it as unauthorized; otherwise downloads are held and the owner is notified. |
| A6 | Owner | MAM sees qBittorrent as connectable. | Watch | Load User Data `clientStats` connectable status, cached by MAM for 30 minutes | When MAM reports the client not connectable, Windlass repairs what it can (below) and notifies the owner if MAM still reports it not connectable after the repair and one fresh reading. |
| A7 | 1.2 | The VPN IP changes less than once an hour. | Gate | (Windlass's own switch record) | Automatic failover is spaced at least an hour apart, and the spacing survives restarts (implemented). |
| A8 | 1.2 | MAM knows the seedbox IP. | Gate | Dynamic Seedbox response and IP lookup | Dynamic seedbox registration after each IP change, at most once per rolling hour (B-0004). No download is admitted until MAM confirms registration for the current exit IP. |
| A9 | 1.6 | The account stays active. | Watch | (not reported by the API) | Windlass cannot log in for the owner (A12). It reminds the owner monthly to visit the site; see the open questions. |
| A10 | Download docs | Downloads are not disabled. | Gate | Load User Data, if it reports the state; otherwise a refused download | A refused download closes the admission gate and notifies the owner, rather than being retried. |

### Automation boundaries

| ID | Rule | Invariant | How Windlass keeps it |
| --- | --- | --- | --- |
| A11 | 1.7 | Only documented API functions are automated. | Windlass calls only the endpoints listed above. Undocumented parameter values are not guessed; they wait for documentation or staff confirmation. The owner accepted the bookmark filter `"searchIn": "bookmarks"`, observed working in a JSON POST, without waiting for confirmation. |
| A12 | 1.7 | No scraping and no email/password login. | Windlass reads only JSON API responses and authenticates only with the owner's `mam_id` API cookie. |
| A13 | Bonus store docs | `gift` and `sendWedge` are never automated. | Windlass can only express the purchases `upload`, `wedges`, and `VIP`. |
| A14 | 2.2, 3.7, 3.15, 4.4 | No begging, requests, or sharing files or `.torrent` files. | Windlass has no function that posts, requests, gifts, or exports content or torrent files. |
| A15 | Direction | Request rates stay within the shared token bucket and every endpoint's own limit (IP lookup once a minute, dynamic seedbox once an hour). | Requests wait for tokens and cooldowns; identical requests are queued once. |
| A16 | Direction | Pause and VPN protection stop MAM requests. | Applies to every MAM call, including purchases (implemented for the request path). |
| A17 | MAM staff | Every request names the tool and its version, so MAM can block a faulty version. | Every MAM request sends `User-Agent: Windlass/<version>`, the release version of the running build. |
| A18 | MAM practice | MAM may replace the `mam_id` cookie in a response. | Windlass saves a replaced cookie at once and uses it for every later request, as MLM and Mousehole do. A cookie that fails is never replaced by an older one without the owner. |

### Spending

| ID | Source | Invariant | How Windlass keeps it |
| --- | --- | --- | --- |
| S1 | Owner | Bonus points never fall below the reserve set on the settings page, 20,000 by default. | Every purchase requires MAM's reported points, minus the price, to stay at or above the reserve. |
| S5 | Owner | Upload credit stays at least 25 GiB above download, so VIP can be bought when downloads need it. | When MAM's uploaded minus downloaded falls below 25 GiB, Windlass buys upload credit in 50 GiB steps, within the points reserve. |
| S2 | Download docs | A wedge is spent only on purpose. | Windlass adds `fl` only to a download its policy wedges, and never to a VIP torrent or a torrent MAM already reports as a personal freeleech. Search flags lag 5 to 20 minutes, so Windlass also records its own wedge use and never wedges a torrent twice. |
| S3 | Store docs | Purchases use only documented amounts. | Upload credit in whole GiB of at least 50, or `Max Affordable `; VIP only as `max`, which fills to 90 days and buys at least 7 days. |
| S4 | Owner | A purchase is never repeated after an uncertain result. | A purchase whose outcome is unknown, for example a timeout, is reconciled from Load User Data and User Bonus History before anything else is bought. |

## Admission: from bookmark to download

When a new bookmark appears, Windlass records it as *Queued* in the torrent
registry. It then admits queued torrents oldest bookmark first, one at a time,
only when all of these hold:

1. MAM requests are not paused, VPN protection is available, and MAM confirms
   the seedbox registration for the current exit IP (A8, A16).
2. MAM's account data is fresh: read within the last 5 minutes, and after the
   previous admission.
3. The torrent is not already downloaded, snatched, or active (B-0005 history).
4. qBittorrent is reachable, at an allowed version, and listed by MAM as an
   authorized client (A5). Downloads are not disabled (A10).
5. An unsatisfied slot is free after the reserve, by MAM's count (A2).
6. The download costs no ratio (principle 2):
   - a VIP torrent while MAM reports the account as VIP, without a wedge;
   - any other torrent, including one that is freeleech site-wide, with a
     wedge, so that freeleech cannot end mid-download.
   If no wedge is available, the spending policy may buy one, or buy VIP for a
   VIP torrent. Otherwise the torrent stays queued.
7. Disk space suffices for the full size (Direction, B-0001).

The torrent is then downloaded, added to qBittorrent whole, and seeded
indefinitely. It is removed only by the owner's cleanup permission, once A3 is
satisfied and space is needed.

When a gate holds a torrent, the dashboard names the gate and MAM's value
behind it.

## Keeping qBittorrent connectable

When MAM reports the client not connectable (A6), Windlass works through these
steps. It stops at the first that fixes it, and checks it with MAM's next fresh
reading, at least 30 minutes later because of MAM's cache:

1. Check that qBittorrent listens on the forwarded port, and set it if not
   (implemented while a lease is held).
2. Renew the forwarded-port lease with the VPN gateway.
3. Check that the exit IP MAM sees matches the tunnel's, and that the seedbox
   registration is current (A8); register again if allowed.
4. Check that qBittorrent is bound to the tunnel and its firewall rule admits the
   forwarded port.

If MAM still reports the client not connectable after the steps and one more
fresh reading, Windlass notifies the owner with what it checked and what it
found. It does not switch VPN servers only to fix connectability, because that
changes the IP MAM sees (A7, A8).

## Spending policy

Agreed with the owner. Windlass reads the points balance, class, VIP expiry, and
ratio from Load User Data, and never spends below the points reserve. In order of
priority:

1. **Wedge every download that is not a VIP torrent.** If none is left, buy
   wedges when the points allow.
2. **Buy VIP when downloads need it.** Buy VIP when queued torrents are held only
   by the unsatisfied limit and VIP's limit of 150 would free a slot, or when a
   queued torrent is VIP-only and the account is not VIP. The API buys VIP only
   as `max`, which fills to 90 days and buys at least 7 days, so each purchase
   can be large. Windlass does not renew VIP otherwise.
3. **Protect the ratio.** Downloads never cost ratio, so this should not happen.
   If the ratio is below the target of 3.0, buy upload credit in 50 GiB steps
   until it reaches 3.0 or only the reserve is left, and tell the owner.

Points above the reserve that no rule needs are kept, not spent.

## Watches and notifications

New notification conditions, using B-0002's grace periods:

| Condition | Notify after |
| --- | --- |
| Ratio below 2.0, holding downloads | Immediately; urgent below 1.1 |
| Unsatisfied torrents at 80% of MAM's limit | Immediately |
| A torrent at day 20 with less than 72 hours seeded | Immediately |
| MAM reports qBittorrent not connectable after the repair steps | After the repair and one fresh reading |
| MAM does not list qBittorrent as an authorized client | Immediately |
| Downloads held, and why (gate and MAM's value) | 30 minutes |
| A purchase made, failed, or uncertain | Immediately |
| No wedge available and not enough points to buy one | Immediately |
| VIP expiring within 7 days while downloads depend on it | Immediately |
| Bonus points below the reserve | Immediately |
| MAM reports a notification for the account, as a count without its text | Immediately |
| The account may need a visit to stay active (A9) | Monthly |

## Data each invariant needs

| Data | Source | Freshness |
| --- | --- | --- |
| Ratio, uploaded, downloaded, class, points, VIP expiry | Load User Data | 15 minutes; 5 minutes before admission or purchase |
| Unsatisfied count and per-torrent satisfaction | Load User Data `snatch_summary` | As above |
| Client list, versions, connectable status | Load User Data `clientStats`, cached by MAM for 30 minutes | 30 minutes |
| Bookmarks, freeleech, VIP, personal freeleech, snatched | Torrent Search | Each bookmark poll; flags lag 5 to 20 minutes |
| Seeding time, completion, files | qBittorrent (implemented) | 10 seconds |
| Wedges used and bought | User Bonus History, and Windlass's own records | Before spending |
| Exit IP and ASN as MAM sees them | IP lookup, once a minute at most | B-0004 |

## Evidence from MLM

MLM's MAM client (`mlm_mam/src/api.rs` and `user_data.rs`) suggests answers to
several questions below. MLM was reportedly banned for breaking API rules, so
each answer is a lead to confirm with the owner's own responses, not proof:

- `GET /jsonLoad.php?snatch_summary=true` returns `uploaded_bytes` and
  `downloaded_bytes` as numbers, `wedges` (the wedge count), and
  `snatch_summary.unsat` with `count`, `limit`, and `size`, beside `seedUnsat`,
  `inactUnsat`, and a `connectable` value. MAM supplies the unsatisfied limit
  itself, so A2 need not map class names.
- Bookmarks are listed with `"searchIn": "bookmarks"` in a JSON POST to the
  search endpoint.
- MLM spends wedges with `fl` on download, and notes that the bonus store no
  longer sells wedges through the API.
- MLM sends `User-Agent: MLM/<version>`, saves replaced cookies, and also calls
  `/json/checkCookie.php` and `cdn.myanonamouse.net/json/loadUserDetailsTorrents.php`.
  Neither is in the supplied documentation, so Windlass does not use them (A11).

## Observed responses

Captured by the owner on 2026-10-24 from the phone, with a test session; personal
details are not recorded here.

`GET /jsonLoad.php?snatch_summary=true` (calls 1 and 2) returned:

- `uploaded_bytes` and `downloaded_bytes` as JSON integers. `ratio` is a display
  string, `"∞"` when nothing has been downloaded, so Windlass computes the ratio
  and the upload buffer from the byte counts and never parses `ratio`.
- `wedges` as an integer, so Load User Data does report the wedge balance.
- `seedbonus` as an integer, `classname` as text (`"VIP"` seen), and `vip_until`
  as `"YYYY-MM-DD HH:MM:SS"` with no zone. `snatch_summary.created_at` uses the
  same format and matches `created`, a Unix time, read as UTC, so MAM's times
  are taken as UTC.
- `snatch_summary` holds categories, each with `name`, `count`, `red`, and, for
  most, `size`. `size` is `null`, not 0, when the category is empty.
  - `unsat`: `count` and `limit` (150 for this VIP account). MAM supplies the
    limit, so A2 needs no table of classes.
  - `seedUnsat`, `inactUnsat`, `seedHnr`, `inactHnr`, `sSat`, `inactSat`,
    `leeching`, `upAct`, `upInact`: counts by seeding and satisfaction state.
    They do not name torrents, so per-torrent satisfaction is still open (A3).
  - `duplicates`: duplicate peer entries, marked red.
  - `ite`: important tracker errors, with `count` and `latest`.
  - `reseed`: reseed requests, with `count` and `inactive`.
  - `connectable`: a string; `"offline"` while nothing was seeding. Other values
    still to be seen. `v6_connectable` was `null`.
  - `partial`: a boolean, so MAM's view of partial downloads can confirm A4.
  - `recently_deleted`, `ipv6_mac`.
- Flag parameters need a value: `clientStats` alone was ignored, while
  `clientStats=true` was honoured, as was `snatch_summary=true`. Windlass sends
  every flag as `=true`.
- `GET /jsonLoad.php?clientStats=true` (call 3) returned `"clientStats": []`
  while nothing was seeding. The shape of an entry is still to be captured once
  qBittorrent seeds through the VPN.
- `GET /jsonLoad.php?notif=true` (call 4) added `notifs`, an object of integer
  counts: `pms`, `tickets`, `waiting_tickets`, `requests`, `topics`, and
  `aboutToDropClient`. A10 reports the counts without any text; a nonzero
  `aboutToDropClient` is urgent, since it suggests MAM is about to stop
  accepting a client. No disabled-download state appeared; it may only show
  when it applies.
- Torrent Search (calls 5 and 6) accepts GET with `tor[...]` parameters:
  `tor[id]=<id>` returned that one torrent. Each result gives `id` as an
  integer; `free`, `vip`, `fl_vip`, and `personal_freeleech` as 0 or 1;
  `vip_expire` as a Unix time, 0 when the torrent's VIP status does not end;
  `added` as UTC text; `bookmarked` as `null` when not bookmarked; and `size`
  only as display text such as `"375.8 MiB"`, so exact sizes come from the
  torrent file or qBittorrent. `found` counts all matches and `total` the
  results returned. VIP torrents seen had `fl_vip` 1, which suggests they are
  freeleech for VIP members.
- Bookmarks were not filtered by GET with `tor[searchIn]=bookmarks` (MLM's value,
  which MLM sends as a JSON POST), with a top-level `bookmarks=true`, or with
  `tor[bookmarked]=only` (the value the site's own search page uses): each
  returned the whole site. A JSON POST of
  `{"perpage":5,"tor":{"searchIn":"bookmarks","sortType":"","startNumber":"0"}}`
  did filter: `found` was the owner's bookmark count, and `bookmarked` came back
  as a Unix time, not the datetime the documentation describes. A JSON POST of
  `{"tor":{"bookmarked":"only"}}` returned the whole site. So `searchIn` is read
  only from a JSON POST, and Windlass sends every search that way.
- `GET /json/jsonIp.php` (call 8, on `www`) needs no cookie and returns `ip` as
  text, `ASN` as an integer, `AS` as the provider's name, and `time` as a Unix
  time. Through ProtonVPN it reported `"AS":"ProtonVPN Proton AG"`.
- `GET /json/userBonusHistory.php?type[]=wedgePF&type[]=wedgeGFL` (call 7)
  returned a JSON array, newest first, of entries with `timestamp` (Unix time
  with a fraction), `amount` (-1 per personal freeleech), `type`, `tid` as an
  integer, and the torrent's `title`. It gives no balance; the balance comes
  from Load User Data's `wedges`. The literal brackets in `type[]` must be sent
  as they are or encoded, so curl needs `-g`.
- `GET /tor/download.php?tid=0` (call 9) returned HTTP 200 with
  `content-type: text/html` and an empty body. A refused download is therefore
  not an HTTP error: Windlass accepts a download only when the body parses as a
  torrent file, and treats anything else as refused.
- Every response so far set `mam_id` again with `Max-Age=1296000` (15 days),
  `domain=.myanonamouse.net`, `secure`, and `SameSite=Lax`. The value is new
  each time, not the same value encoded differently, and the old value kept
  working after the new one was issued. A18 stands: Windlass saves each new
  value and sends the latest, but a save that fails is not an immediate
  lockout. How long an old value stays valid is unknown. A session left unused
  for 15 days would expire, so Windlass's regular polling keeps it alive.
- The `red` flag is MAM's own marking of categories that need attention. Any red
  category with a nonzero count is reported, beside the specific gates.
- The session was locked to the network provider (ASN). A request from another
  provider was refused with `Error, you are not signed in <br />Session ASN
  mismatch`, and a request without a cookie with `... <br />Invalid/missing
  cookie`. Both arrive as text, not JSON, and are classed as a rejected session.
  An ASN-locked session for Windlass must be created from the VPN's provider,
  and failover between servers of different providers would break it (B-0004).

## API calls to verify

The owner runs these in a browser while logged in, or with the API cookie, and
keeps the responses with personal details removed. They become the fake MAM
server's fixtures. Read-only calls first; calls that spend only when a purchase
is wanted anyway.

| # | Call | What it settles | Cost |
| --- | --- | --- | --- |
| 1 ✓ | `GET /jsonLoad.php?pretty` | Field types: numbers or strings, units of `uploaded` and `downloaded`, `classname` values, `vip_until` format | None |
| 2 ✓ | `GET /jsonLoad.php?snatch_summary&pretty` | The unsatisfied count, and whether satisfied status is given per torrent (A2, A3) | None |
| 3 ½ | `GET /jsonLoad.php?clientStats=true&pretty` | How clients, versions, and connectable status appear (A5, A6) | None |
| 4 ✓ | `GET /jsonLoad.php?notif=true&pretty` | The notification shape; whether a wedge count or a download-disabled state appears (A10) | None |
| 5 ✓ | `POST /tor/js/loadSearchJSONbasic.php` listing bookmarks | The `searchIn` value for bookmarks, which only staff can confirm (A11); then `bookmarked`, `free`, `vip`, `personal_freeleech`, `my_snatched`, and `size` | None |
| 6 ✓ | Torrent Search with `tor[id]` for one torrent | Flags for a single torrent before download | None |
| 7 ✓ | `GET /json/userBonusHistory.php?type[]=wedgePF&type[]=wedgeGFL` | Wedge use; whether a wedge balance appears | None |
| 8 ½ | `GET /json/jsonIp.php` on `www` and `t` | IP and ASN as MAM sees them, once a minute at most | None |
| 9 ✓ | `GET /tor/download.php?tid=<invalid>` | What a refused download returns instead of a torrent file | None |
| 10 | `GET /json/bonusBuy.php?spendtype=upload&amount=10` | The error shape, without buying: under 50 GiB is refused | None |
| 11 ✓ | Any call with an invalid `mam_id` | The error for a rejected cookie | None |
| 12 | `GET https://t.myanonamouse.net/json/dynamicSeedbox.php` through the VPN | Registration responses, once an hour at most | None |
| 13 | `GET /tor/download.php?tid=<id>` | A torrent file for a bookmarked torrent | Downloads it |
| 14 | `GET /tor/download.php?tid=<id>&fl` | Wedge use; `personal_freeleech` and wedge history afterwards | One wedge |
| 15 | `GET /json/bonusBuy.php?spendtype=wedges` | Wedge price; whether the response gives the new balance | Points |
| 16 | `GET /json/bonusBuy.php?spendtype=upload&amount=50` | Price per GiB; the response | Points |
| 17 | `GET /json/bonusBuy.php?spendtype=VIP&duration=max` | VIP price; `vip_until` afterwards | Points |

For each call, also note whether GET or POST is accepted, and any rate-limit
response.

## Feature-wide constraints and acceptance

- Every gate is decided in the core from supplied events and time; the shell only
  makes the requests. Core tests cover each invariant at, just below, and just
  above its threshold, and with MAM's reading missing or stale.
- Integration tests use a fake MAM server, never the real site.
- The dashboard shows every invariant's value as MAM reports it, its limit, and
  what it is holding back.

## Scope

Account monitoring, admission gates for new downloads, seeding obligations,
connectability, bonus point purchases, and their notifications.

## Non-goals

Uploading, requests, forum activity, gifts, the millionaire's vault, invites,
and anything outside the documented API.

## Related records

- [Direction: MAM API compliance](../../direction.md#mam-api-compliance)
- [MAM initialization](../B-0004/brief.md)
- [MAM acquisition](../B-0005/brief.md)
- [qBittorrent integration](../B-0007/brief.md)
- [VPN confinement and port forwarding](../B-0010/brief.md)
- [Torrent registry](../../decisions/adr/0004-torrent-registry.md)
- [Notifications](../B-0002/brief.md)

## Open questions and assumptions

Agreed with the owner: MAM's figures are authoritative; downloads never cost
ratio, using a wedge on every torrent that is not VIP; VIP bought only when
downloads need it; upload credit bought only to restore a ratio below 3.0; a
fixed points reserve set on the settings page; MAM's view of connectability is
checked, repaired where possible, and reported otherwise.

Agreed with the owner: a points reserve of 20,000; an upload buffer of 25 GiB;
2 unsatisfied slots kept free; 24 hours of seeding beyond the 72 required before
removal, so 96 hours in total.

Agreed with the owner: the bookmark filter `"searchIn": "bookmarks"` is used as
observed, without waiting for staff confirmation.

Facts the documentation does not settle, to confirm with MAM staff or from a
response the owner captures in a browser. Until each is settled, the gates that
depend on it stay closed:

- Which torrents are satisfied: `snatch_summary` gives the unsatisfied count but
  only counts per category. Without per-torrent status, A3 blocks automatic
  removal.
- The other values of `snatch_summary.connectable` besides `"offline"`.
- The shape of `clientStats`: how clients, versions, and connectable status are
  reported (A5, A6).
- Prices in the bonus store, and whether purchases report the new balance.
- Whether VIP torrents are freeleech for VIP members, as the API's warning about
  wedges on VIP torrents suggests. Until confirmed, VIP torrents wait.
- Whether API use counts as activity for rule 1.6.
- Whether Load User Data reports a disabled-download or seed-only state (A10).
