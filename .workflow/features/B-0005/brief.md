# MAM search and torrent queueing loop

Status: Draft
Feature ID: B-0005

## Goal

Acquire specific torrents selected through MAM bookmarks, preserving download
history and handing metadata to [qBittorrent](../B-0007/brief.md).

## Stories and acceptance

### S1: Discover selected torrents

Story: The user bookmarks torrents on MAM. Windlass discovers these selections
without choosing a different torrent through title matching.

Acceptance:

- Leave bookmarks untouched.
- Deduplicate by MAM torrent ID and check current client presence and active work.
- Suppress historical downloads and positive `my_snatched` results, including
  acquisitions predating Windlass. Retain this evidence without claiming completion.
- Deleted files, removed client entries, and restart do not erase suppression.
- A false or missing `my_snatched` flag cannot override local history.
- Explicit re-download overrides historical suppression, not current active work.
- A selected torrent that cannot start yet, for example because the account has
  no free unsatisfied-torrent slot, is recorded as *Queued* in the
  [torrent registry](../../decisions/adr/0004-torrent-registry.md) and admitted
  when a slot frees.

### S2: Admit and acquire a torrent

Story: Windlass checks a selected torrent and either acquires its metadata for
download or shows why acquisition is blocked.

Acceptance:

- Require confirmed MAM access and seedbox registration, client availability,
  account eligibility, sufficient space, and the duplicate/history checks.
- Insufficient space holds new downloads while existing torrents keep seeding
  under the VPN boundary. Follow the [cleanup policy](../../direction.md#disk-space).
- Fetching torrent metadata is distinct from client acceptance and book completion.

Proposed ordering, pending agreement:

1. Wait for [initialization](../B-0004/brief.md), then read bookmarked torrents.
2. Apply duplicate/history checks. If candidates remain, load account data once
   for the batch rather than separately per torrent; required freshness is open.
3. Select the next candidate and check admission for its complete content size.
4. If necessary, perform eligible cleanup only until sufficient space exists.
   If cleanup cannot provide space, leave acquisition blocked.
5. Fetch and validate the .torrent file, including identity and complete size.
   Reassess space if metadata differs from the search result.
6. Save acquisition intent before client submission. A failed save prevents
   submission. Recheck admission before starting content transfer.
7. Reconcile uncertain client submission by hash before retrying. Record client
   acknowledgement and later completion separately, preserving completion date.
8. Organise completed content and continue seeding under their separate policies.

### S3: Preserve history through cleanup and recovery

Story: A book is removed for space, or acquisition is interrupted. Windlass
retains enough history to avoid unintended repeat downloads.

Acceptance:

- Cleanup retains acquisition history and requires explicit user permission.
- Repeated polls cannot duplicate waiting, submitted, or active acquisitions.
- Explicit repeat acquisition preserves the earlier history.
- Interrupted downloads and uncertain persistence need recovery rules before Ready.

## Feature-wide constraints and acceptance

Apply [Direction's request controls](../../direction.md#mam-api-compliance) and
pause/VPN behaviour. Shared request deduplication differs from acquisition history.
Pending API requests are discarded during pause; retention of user intent is open.
Use the fake MAM server for HTTP integration tests.

### External API calls

All calls use HTTPS on `www.myanonamouse.net` and `Cookie: mam_id=<API cookie>`.

| Purpose | Call | Request data | Official source |
| --- | --- | --- | --- |
| Bookmarks | `POST /tor/js/loadSearchJSONbasic.php` | Proposed JSON body; `Content-Type: application/json` | [Search](../../../docs/MAM/loadSearchJSONbasic.txt) |
| Account data | `GET /jsonLoad.php?snatch_summary` | No body or user `id` | [User data](../../../docs/MAM/jsonLoad.txt) |
| Torrent metadata | `GET /tor/download.php?tid=<id>` | No body | [Download](../../../docs/MAM/download.txt) |

Search requires `tor`; `perpage` accepts 5–1000 and `startNumber` is the offset.
The bookmark-filtering `searchIn` value is undocumented. A `bookmarks` flag or
bookmark sort alone does not establish that filter. Do not invent the body.
Useful results include ID, title, size, format, seeders, freeleech/VIP flags,
and `my_snatched`. Author, narrator, and series fields contain JSON strings;
some numeric examples use strings. Snatched/personal-freeleech flags lag 5–20 minutes.

Account data lists class, ratio, VIP expiry, and formatted transfer totals.
The `snatch_summary` shape lacks an example. Do not infer exact byte balances
from formatted strings. Proposed policy: missing, stale, or unrecognised eligibility
blocks acquisition. `clientStats` has a 30-minute cache and cannot prove current
client availability. No bonus purchases are implied.

Torrent fetch must return valid metadata, not HTML/JSON with HTTP success.
Proposed default: omit `fl`, which can spend a wedge even on VIP torrents.
Prefer authenticated download by ID without requesting credential-bearing `dlLink`.
Endpoint-specific limits and error contracts are incomplete in the supplied docs.

## Scope

Bookmarks, admission, metadata acquisition, durable history, and client handoff.

## Non-goals

General title searches, automatic content recommendations, spending policy,
client API implementation, library organisation, and cleanup implementation.

## Related records

- [Direction](../../direction.md#cohesive-mam-experience)
- [Single core](../../decisions/adr/0001-single-core.md)
- [Official-source register](../../references.md#mam-rules-and-api-documentation)

## Open questions and assumptions

- Bookmark filter, cadence, pagination bounds, media, and processing order.
- Account thresholds, response fixtures, freshness, and disk reservations/margin.
- Whether to adopt the proposed ordering and wedge policy.
- Interrupted acquisition, persistence failures, retries, and retention of user
  intent during MAM pause.
- Whether polling requires completed initialization or only content acquisition does.
- Explicit repeat-download controls and reconciliation of externally modified work.
