# MAM account safety and bonus points

Status: Draft
Feature ID: B-0011

## Goal

The owner bookmarks torrents on MAM and Windlass does everything else: it
downloads, seeds, and cleans up without ever putting the account outside MAM's
rules, and it spends bonus points on upload credit, freeleech wedges, and VIP
when that keeps the account healthy and downloads flowing.

Windlass treats MAM's rules as invariants. Each invariant is either a **gate**,
checked before an action so the action never happens when it would break a
rule, or a **watch**, checked continuously so the owner hears about drift long
before MAM acts on it. When Windlass cannot tell whether an action is safe, it
does not take it.

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

| ID | Rule | Invariant | Kind | How Windlass keeps it |
| --- | --- | --- | --- | --- |
| A1 | 1.4, 2.4 | Global ratio stays at or above 1.0. | Gate, watch | A download is admitted only if the projected ratio after it stays at or above the ratio floor of 2.0 (A2). The watch notifies when the ratio is below 2.0, which holds new paid downloads, and urgently below 1.1. |
| A2 | 1.4 | A safety margin above 1.0 absorbs rounding, lag, and downloads in flight. | Gate | Projected ratio = uploaded ÷ (downloaded + every admitted, incomplete, non-freeleech torrent's full size + the candidate's size). Formatted totals are rounded against Windlass: uploaded down, downloaded up. |
| A3 | 2.8 | Unsatisfied torrents stay below the class limit: 20 for new members, 50 User, 100 Power User, 150 VIP, 200 above VIP. | Gate, watch | A download is admitted only if unsatisfied + admitted-but-not-yet-counted + 1 stays at least the reserve below the limit. The count is the larger of MAM's and Windlass's own. An unrecognised class uses 20. Crossing the limit blocks downloads for up to 24 hours. |
| A4 | 2.5 | Every downloaded torrent seeds 72 hours within 30 days of completion. | Gate, watch | Windlass never stops, pauses, or removes a torrent until it has seeded 72 hours plus a margin. The watch notifies when a torrent completed 20 or more days ago has not yet seeded 72 hours, which happens if seeding is interrupted, for example while the VPN is down. |
| A5 | 2.7 | No partial torrents. | Gate | Every file stays selected (implemented). Downloads are admitted with their full size. |
| A6 | 2.3 | Only approved, release versions of qBittorrent; no auto-update. | Gate, watch | The NixOS module refuses other versions and pins the package (implemented). If qBittorrent reports a version MAM does not allow, new downloads are held and the owner is notified (implemented). |
| A7 | 1.2 | The VPN IP changes less than once an hour. | Gate | Automatic failover is spaced at least an hour apart, and the spacing survives restarts (implemented). |
| A8 | 1.2 | MAM knows the seedbox IP. | Gate | Dynamic seedbox registration after each IP change, at most once per rolling hour (B-0004). No download is admitted until registration is confirmed for the current exit IP. |
| A9 | 1.6 | The account stays active. | Watch | Windlass cannot log in for the owner (A12). It reminds the owner to visit the site if they have not confirmed a visit recently; see the open questions. |

### Automation boundaries

| ID | Rule | Invariant | Kind | How Windlass keeps it |
| --- | --- | --- | --- | --- |
| A10 | 1.7 | Only documented API functions are automated. | Gate | Windlass calls only the endpoints listed above. Undocumented parameter values, such as the bookmark filter of the search endpoint, are not guessed; they wait for documentation or staff confirmation. |
| A11 | 1.7 | No scraping. | Gate | Windlass reads only JSON API responses. It never parses site pages. |
| A12 | 1.7 | No email/password login. | Gate | Windlass authenticates only with the owner's `mam_id` API cookie (implemented as a setting). |
| A13 | Bonus store docs | `gift` and `sendWedge` are never automated. | Gate | Windlass can only express the purchases `upload`, `wedges`, and `VIP`. |
| A14 | Rules 3.7, 3.15, 4.4 | No begging, requests, or sharing files with members. | Gate | Windlass has no function that posts, requests, gifts, or exports content or `.torrent` files (2.2). |
| A15 | Direction | Request rates stay within the shared token bucket and every endpoint's own limit (IP lookup once a minute, dynamic seedbox once an hour). | Gate | Requests wait for tokens and cooldowns; identical requests are queued once. |
| A16 | Direction | Pause and VPN protection stop MAM requests. | Gate | Implemented for the request path; applies to every MAM call, including purchases. |

### Spending

| ID | Source | Invariant | Kind | How Windlass keeps it |
| --- | --- | --- | --- | --- |
| S1 | Policy | Bonus points never fall below the reserve. | Gate | Every purchase requires points after the purchase at or above the reserve. |
| S2 | Download docs | A wedge is spent only on purpose. | Gate | Windlass adds `fl` to a download only when its policy chose a wedge for that torrent, and only if the torrent is not freeleech, not VIP, and not already a personal freeleech. MAM spends wedges even on VIP torrents and never refunds them. Search flags lag 5 to 20 minutes, so Windlass also records its own wedge use and never wedges a torrent twice. |
| S3 | Store docs | Purchases use only documented amounts. | Gate | Upload credit in whole GiB of at least 50, or `Max Affordable `; VIP only as `max`, which fills to 90 days and buys at least 7 days. |
| S4 | Policy | A purchase is never repeated after an uncertain result. | Gate | A purchase whose outcome is unknown, for example a timeout, is reconciled from account data and bonus history before anything else is bought. |

## Admission: from bookmark to download

When a new bookmark appears, Windlass records it as *Queued* in the torrent
registry. It then admits queued torrents oldest bookmark first, one at a time,
only when all of these hold:

1. MAM requests are not paused, VPN protection is available, and the seedbox is
   registered for the current exit IP (A8, A16).
2. Account data is fresh: read within the last 5 minutes, and newer than the
   last admission.
3. The torrent is not already downloaded, snatched, or active (B-0005 history).
4. qBittorrent is reachable and at an allowed version (A6).
5. An unsatisfied slot is free after the reserve (A3).
6. The ratio allows it (A1, A2). Freeleech and personal-freeleech torrents cost
   no ratio, but are admitted only if the account would still be at or above
   1.0 if counted in full, in case freeleech ends mid-download.
7. A VIP-only torrent needs the account to be VIP; the spending policy may buy
   VIP for it.
8. If the ratio does not allow it, the spending policy may make it fit, by
   buying upload credit. Otherwise the torrent stays queued and the dashboard
   says why.
9. Disk space suffices for the full size (Direction, B-0001).

The torrent is then downloaded, added to qBittorrent whole, and seeded
indefinitely. It is removed only by the owner's cleanup permission, after A4 is
satisfied with margin and space is needed.

## Spending policy

Agreed with the owner. Windlass reads the points balance, VIP expiry, and ratio
from Load User Data, and never spends below the points reserve, a fixed number
of points set on the settings page. In order of priority:

1. **Protect the ratio.** When the ratio is below the target of 3.0, buy upload
   credit in 50 GiB steps until it reaches 3.0 or only the reserve is left.
2. **Wedge every paid download.** Download every torrent that is not freeleech,
   not VIP, and not already a personal freeleech with `fl`, spending a wedge.
   When none is left, buy one if the points allow; otherwise the download costs
   ratio and must pass the ratio floor.
3. **Buy VIP when downloads need it.** Buy VIP when queued torrents are held only
   by the unsatisfied limit and VIP's limit of 150 would free a slot, or when a
   queued torrent is VIP-only and the account is not VIP. The API buys VIP only
   as `max`, which fills to 90 days and buys at least 7 days, so each purchase
   can be large. Windlass does not renew VIP otherwise.

Points above the reserve that no rule needs are kept, not spent.

## Watches and notifications

New notification conditions, using B-0002's grace periods:

| Condition | Notify after |
| --- | --- |
| Ratio below 2.0, holding paid downloads | Immediately; urgent below 1.1 |
| Unsatisfied torrents at 80% of the limit | Immediately |
| A torrent at day 20 with less than 72 hours seeded | Immediately |
| Downloads held, and why (ratio, slots, registration, space) | 30 minutes |
| A purchase made, failed, or uncertain | Immediately |
| VIP expiring within 7 days while downloads depend on it | Immediately |
| Bonus points below the reserve | Immediately |
| MAM reports a notification for the account, as a count without its text | Immediately |
| The account may need a visit to stay active (A9) | Monthly |

## Data each invariant needs

| Data | Source | Freshness |
| --- | --- | --- |
| Ratio, uploaded, downloaded, class, points, VIP expiry | Load User Data | 15 minutes; 5 minutes before admission or purchase |
| Unsatisfied count | Load User Data `snatch_summary`, and the registry | As above |
| Bookmarks, freeleech, VIP, personal freeleech, snatched | Torrent Search | Each bookmark poll; flags lag 5 to 20 minutes |
| Seeding time, completion, files | qBittorrent (implemented) | 10 seconds |
| Wedges used and bought | User Bonus History, and Windlass's own records | Before spending |
| Exit IP and ASN as MAM sees them | IP lookup, once a minute at most | B-0004 |

## Feature-wide constraints and acceptance

- Every gate is decided in the core from supplied events and time; the shell only
  makes the requests. Core tests cover each invariant at, just below, and just
  above its threshold.
- Integration tests use a fake MAM server, never the real site.
- The dashboard shows every invariant's current value, its limit, and what it is
  holding back.

## Scope

Account monitoring, admission gates for new downloads, seeding obligations,
bonus point purchases, and their notifications.

## Non-goals

Uploading, requests, forum activity, gifts, the millionaire's vault, invites,
and anything outside the documented API.

## Related records

- [Direction: MAM API compliance](../../direction.md#mam-api-compliance)
- [MAM initialization](../B-0004/brief.md)
- [MAM acquisition](../B-0005/brief.md)
- [qBittorrent integration](../B-0007/brief.md)
- [Torrent registry](../../decisions/adr/0004-torrent-registry.md)
- [Notifications](../B-0002/brief.md)

## Open questions and assumptions

Agreed with the owner: a ratio floor of 2.0 for admission and a target of 3.0
for buying upload credit; a wedge on every paid download; VIP bought only when
downloads need it; a fixed points reserve set on the settings page.

Still to decide:

- The points reserve's starting value.
- The unsatisfied reserve. Proposed: 2 slots.
- The seeding margin before removal. Proposed: 24 hours, so 96 hours in total.

Facts the documentation does not settle, to confirm with MAM staff or from a
response the owner captures in a browser:

- The bookmark filter value for `searchIn`. The documentation says "list to
  come". Until it is known, A10 blocks bookmark polling.
- The shape of `snatch_summary`, and whether it gives the unsatisfied count.
  Until it is known, A3 uses only Windlass's own count, which cannot see
  downloads made outside Windlass.
- Prices in the bonus store, and whether purchases report the new balance.
- Whether VIP torrents are freeleech for VIP members. The owner reports that
  VIP-only torrents need a VIP account. Until confirmed, Windlass treats VIP
  torrents as costing ratio and never wedges them.
- How many wedges the account holds; Load User Data does not document it.
- Whether API use counts as activity for rule 1.6.
- Whether Load User Data reports a disabled-download or seed-only state.
