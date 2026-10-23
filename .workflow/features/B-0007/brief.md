# qBittorrent integration

Status: Draft
Feature ID: B-0007

## Goal

Draft for discussion. Native qbittorrent-nox on NixOS is the intended client.
The exact MAM-approved version must be selected before this becomes an executable
contract. Windlass communicates through a configured private HTTP endpoint.

## Feature-wide constraints and acceptance

The MAM pause control does not stop existing qBittorrent downloads or tracker
communication. A new acquisition still requires the agreed admission checks.
Client API requests do not consume MAM bucket tokens. NixOS enforces VPN isolation:
qBittorrent runs in the VPN namespace of
[ADR-0003](../../decisions/adr/0003-vpn-namespace-confinement.md), is bound to the
WireGuard interface, and has its WebUI on namespace-local loopback only. Its own
UPnP and NAT-PMP are disabled; Windlass sets the listen port to the port forwarded
by ProtonVPN ([B-0010](../B-0010/brief.md)) through `app/setPreferences`.

Core tests exercise admission and reconciliation with events and actions. Shell
integration tests use the pinned real qBittorrent binary and disposable local
torrents, without contacting MAM. Include lost add/start responses, expired
sessions, restart during submission, incomplete content, and deletion failures.

### External API surface

Source: [official API wiki](https://github.com/qbittorrent/qBittorrent/wiki/WebUI-API-%28qBittorrent-5.0%29).
Paths below are relative to that endpoint.

| Purpose | Request | Data |
| --- | --- | --- |
| Authenticate | `POST /api/v2/auth/login` | Form: `username`, `password`; retain `SID` cookie. |
| Check versions | `GET /api/v2/app/version`, `GET /api/v2/app/webapiVersion` | No body. |
| List or reconcile | `GET /api/v2/torrents/info` | Optional `hashes` filter. |
| Submit metadata | `POST /api/v2/torrents/add` | Multipart `torrents` file, `savepath`; optional `category`, `tags`. |
| Start accepted work | `POST /api/v2/torrents/start` | Form: `hashes`. |
| Read details | `GET /api/v2/torrents/properties?hash=<hash>` | No body. |
| Inspect content | `GET /api/v2/torrents/files?hash=<hash>` | No body. |
| Remove torrent and data | `POST /api/v2/torrents/delete` | Form: `hashes`, `deleteFiles=true`. |

Authenticated calls carry `SID`. Set Origin or Referer consistently with the
configured endpoint. HTTP 200 alone is insufficient to establish login success.
Progress observations include `amount_left`, `completion_on`, `state`, and
`seeding_time`.

For stopped submission, the wiki documents `paused`, but
[current source](https://github.com/qbittorrent/qBittorrent/blob/master/src/webui/api/torrentscontroller.cpp)
reads `stopped`. Select the correct parameter for the pinned release; do not
copy a mixed-version request into implementation.

## Stories and acceptance

### S1: Submit and observe an acquisition

Story: Windlass hands acquired metadata to qBittorrent and can distinguish
accepted, uncertain, active, and completed work.

Acceptance:

- A timeout must not be treated as proof of failed submission.
- Preserve acquisition history and observe actual content completion separately.

Proposed acquisition handoff, pending agreement:

1. Establish the qBittorrent session and verify the expected client/API version.
   This login is local client authentication, separate from MAM API-cookie access.
2. Obtain a current inventory. Preserve the mapping between MAM torrent ID and
   torrent hash from the validated metadata. Check the whole client for a matching
   torrent, not only a Windlass category. Never identify a torrent by title alone.
3. Save acquisition intent before submission. A failed save prevents submission.
4. Upload the bytes Windlass fetched from MAM. Do not give qBittorrent a MAM
   download URL or the MAM API cookie; that would bypass Windlass's request gates.
5. Add in a stopped state with normal verification enabled and the whole torrent
   selected. Path and category policy remain to be agreed.
6. Query by hash to confirm presence and inspect the accepted content and path.
   If add times out, reconcile first. Do not repeat the upload while its outcome
   is uncertain. Absence from one immediate query need not establish failure.
7. Recheck admission, including available/reserved disk space and VPN protection.
   Save the accepted mapping before requesting start. If blocked, show the reason
   rather than start the torrent.
8. Observe subsequent client state to confirm that the start took effect. Continue
   monitoring content completion, errors, missing data, and seeding separately.
   Record completion durably before using its date for cleanup ordering.

An explicitly requested re-download overrides historical suppression, not current
presence in the client. Existing stopped or incomplete work needs reconciliation
rather than another copy. The response to externally modified torrents remains
to be agreed.

### S2: Execute permitted cleanup

Story: When space is needed, Windlass removes eligible user-approved content
without losing acquisition history.

Acceptance:


- Deletion follows [Direction](../../direction.md#disk-space). Never use
an unrestricted deletion selector. Reconcile uncertain deletion outcomes and
measure actual free space before deleting another book or admitting a download.
Preserve acquisition history throughout cleanup.

The delete API does not define Windlass's policy for library copies, hardlinks,
or multiple books in one torrent. That policy and evidence of satisfied MAM
seeding obligations must be settled before enabling automatic cleanup. Client
seeding time alone is not established as equivalent to tracker-confirmed credit.

## Scope

qBittorrent authentication, submission, reconciliation, progress, and permitted
cleanup. Shared acquisition policy belongs to the [MAM brief](../B-0005/brief.md).

## Non-goals

MAM API calls, WireGuard implementation, port forwarding (B-0010), and library-copy
deletion policy.

## Related records

- [Direction](../../direction.md#deployment)
- [Single core](../../decisions/adr/0001-single-core.md)
- [External references](../../references.md#qbittorrent-integration)

## Open questions and assumptions

- Exact client version, stopped-submission parameter, and supported torrent-hash
  formats.
- Whether to adopt stopped submission followed by explicit start.
- Polling cadence, session recovery, reconciliation deadlines, and retry timing.
- Save paths, ownership of existing torrents, category/tags, concurrent downloads,
  and disk-space reservation.
- Whether staged downloads may start while MAM contact is explicitly paused.
