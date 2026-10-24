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
| A11 | 1.7 | Only documented API functions are automated. | Windlass calls only the endpoints listed above. Undocumented parameter values, such as the bookmark filter of the search endpoint, are not guessed; they wait for documentation or staff confirmation. |
| A12 | 1.7 | No scraping and no email/password login. | Windlass reads only JSON API responses and authenticates only with the owner's `mam_id` API cookie. |
| A13 | Bonus store docs | `gift` and `sendWedge` are never automated. | Windlass can only express the purchases `upload`, `wedges`, and `VIP`. |
| A14 | 2.2, 3.7, 3.15, 4.4 | No begging, requests, or sharing files or `.torrent` files. | Windlass has no function that posts, requests, gifts, or exports content or torrent files. |
| A15 | Direction | Request rates stay within the shared token bucket and every endpoint's own limit (IP lookup once a minute, dynamic seedbox once an hour). | Requests wait for tokens and cooldowns; identical requests are queued once. |
| A16 | Direction | Pause and VPN protection stop MAM requests. | Applies to every MAM call, including purchases (implemented for the request path). |

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

## API calls to verify

The owner runs these in a browser while logged in, or with the API cookie, and
keeps the responses with personal details removed. They become the fake MAM
server's fixtures. Read-only calls first; calls that spend only when a purchase
is wanted anyway.

| # | Call | What it settles | Cost |
| --- | --- | --- | --- |
| 1 | `GET /jsonLoad.php?pretty` | Field types: numbers or strings, units of `uploaded` and `downloaded`, `classname` values, `vip_until` format | None |
| 2 | `GET /jsonLoad.php?snatch_summary&pretty` | The unsatisfied count, and whether satisfied status is given per torrent (A2, A3) | None |
| 3 | `GET /jsonLoad.php?clientStats&pretty` | How clients, versions, and connectable status appear (A5, A6) | None |
| 4 | `GET /jsonLoad.php?notif&pretty` | The notification shape; whether a wedge count or a download-disabled state appears (A10) | None |
| 5 | `POST /tor/js/loadSearchJSONbasic.php` listing bookmarks | The `searchIn` value for bookmarks, which only staff can confirm (A11); then `bookmarked`, `free`, `vip`, `personal_freeleech`, `my_snatched`, and `size` | None |
| 6 | Torrent Search with `tor[id]` for one torrent | Flags for a single torrent before download | None |
| 7 | `GET /json/userBonusHistory.php?type[]=wedgePF&type[]=wedgeGFL` | Wedge use; whether a wedge balance appears | None |
| 8 | `GET /json/jsonIp.php` on `www` and `t` | IP and ASN as MAM sees them, once a minute at most | None |
| 9 | `GET /tor/download.php?tid=<invalid>` | What a refused download returns instead of a torrent file | None |
| 10 | `GET /json/bonusBuy.php?spendtype=upload&amount=10` | The error shape, without buying: under 50 GiB is refused | None |
| 11 | Any call with an invalid `mam_id` | The error for a rejected cookie | None |
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

Facts the documentation does not settle, to confirm with MAM staff or from a
response the owner captures in a browser. Until each is settled, the gates that
depend on it stay closed:

- The bookmark filter value for `searchIn`; the documentation says "list to
  come". Blocks bookmark polling (A11).
- The shape of `snatch_summary`: whether it gives the unsatisfied count, and
  whether it identifies which torrents are satisfied. Without the count, A2
  closes admission; without per-torrent status, A3 blocks automatic removal.
- The shape of `clientStats`: how clients, versions, and connectable status are
  reported (A5, A6).
- Prices in the bonus store, and whether purchases report the new balance.
- How many wedges the account holds; Load User Data does not document it.
- Whether VIP torrents are freeleech for VIP members, as the API's warning about
  wedges on VIP torrents suggests. Until confirmed, VIP torrents wait.
- Whether API use counts as activity for rule 1.6.
- Whether Load User Data reports a disabled-download or seed-only state (A10).
