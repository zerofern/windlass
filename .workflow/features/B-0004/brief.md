# MAM initialization loop

Status: Draft
Feature ID: B-0004

## Goal

Establish API access and maintain seedbox IP registration. Explain outcomes
while enforcing pause, privacy, and rate-limit policies.

## Stories and acceptance

### S1: Enable MAM contact

Story: The user configures an API cookie and resumes MAM requests. Windlass
establishes whether it can register the seedbox through the protected connection.

Acceptance:

- A paused instance or unavailable VPN protection produces no new MAM requests.
- The API cookie is a user-supplied credential, used like an API key and sent
  as `mam_id`. Windlass changes it only through user configuration, or by saving
  a replacement MAM sends in a response ([B-0011 A18](../B-0011/brief.md#automation-boundaries)).
- No email/password login or undocumented session-check endpoint is used.
- The dashboard distinguishes unverified authentication, a rejected session,
  confirmed registration, and failed or deferred registration.

Load saved pause state and endpoint cooldowns before requests. Register when
MAM requests are not paused, VPN protection is available, a cookie is
configured, and shared and endpoint limits permit. Only documented successful `Completed` and
`No Change` outcomes confirm registration; an error's returned IP does not.
Without a cookie, show setup required and make no MAM requests.

- Implemented: requests leave only while MAM requests are resumed, VPN protection
  is available, and the core's shared token bucket (10 requests, one more every 3
  seconds) has a token; one request is outstanding at a time. The IP lookup needs
  no cookie; every other request needs the saved cookie and a session not
  refused. The dashboard's MAM access panel shows the session as unverified,
  accepted, or refused, and registration as confirmed, failed, or not yet allowed.

### S2: Keep registration current

Story: While MAM requests are not paused, the system detects a changed exit IP and
updates registration without exceeding the documented limits.

Acceptance:

- Registration calls are limited to one per rolling hour, including `No Change`.
- Pausing or losing VPN protection discards queued requests and invalidates
  confidence in authentication and IP reporting, as defined in Direction.
- Observe the exit IP and ASN every five minutes, including registration cooldowns and
  IP/ASN rejections. Pause or unavailable VPN protection stops these requests.
  Never exceed the documented once-per-minute limit.
- If that IP differs from the confirmed registration, request a seedbox update
  when allowed. An unchanged observed IP alone does not require another update.
- After resume or VPN recovery, restart observation and obtain fresh registration
  confirmation when allowed. Do not replay discarded work.
- During registration cooldown, show unconfirmed status and the next eligible time.
- Reconsider queued registration using the latest observation. IP changes must
  not accumulate updates.
- Implemented: IP lookups are sent at most once a minute, and the first after a
  start waits a minute, since a previous run may have sent one moments before.
  Requests that need the cookie wait until MAM has shown which IP and ASN it
  sees, after a start or a server switch, since the session is locked to an ASN.
- Implemented: the time of each registration attempt is saved before the request
  is sent, so restarts keep the hour; a failed save sends nothing. Pause, VPN loss,
  a server switch, or a cookie change discards the outstanding request, ignores
  its late answer, and makes the session and registration unconfirmed.

### S3: Explain failures

Story: When MAM contact fails, the user can distinguish a configuration problem,
a cooldown, and a transport failure without Windlass repeatedly sending requests.

Acceptance:

- A documented 403 session or permission rejection is distinct from a transport
  failure and from unverified authentication.
- A 429 response defers work; available bucket tokens do not override it.
- A timeout or malformed response does not confirm registration.
- A failed IP lookup holds new downloads and marks the current exit IP unverified.
  Retain the last known IP and observation time; do not label the cookie rejected
  or request registration solely because lookup failed.
- Retry failed IP lookups after five minutes or any longer server delay.
  A successful lookup matching the confirmed registration clears this hold.
  It cannot establish registration or replace fresh confirmation after pause or VPN loss.

Agreed failure handling:

| Outcome | Status and response |
| --- | --- |
| HTTP 403, IP/ASN mismatch | Show the rejection. A changed observed IP or ASN permits another attempt after cooldown. |
| HTTP 403, invalid cookie or permission | Show the rejection; wait for configuration changes. Other 403 recovery triggers remain open. |
| HTTP 429 | Registration remains unconfirmed; retry automatically after the endpoint cooldown and any longer server-directed delay. |
| Timeout, transport failure, malformed response, or unexpected status | Outcome is uncertain; keep registration unconfirmed and retry automatically at the next permitted time. |

Every dispatched registration retains its one-hour cooldown, including failed
or cancelled requests. Restart, cookie replacement, and pause/resume cannot shorten it. Automatic
retries require MAM requests to remain unpaused and VPN protection to be available.
Show the failure and next eligible attempt time without requiring user intervention.

- Implemented as in the table. A refused session stops cookie requests until MAM
  sees another IP or ASN, for a mismatch, or the cookie changes. A permission
  refusal (`session type`) blocks registration only. 429 honours `Retry-After`.

### S4: Replace the API cookie

Story: The user replaces a nonworking API cookie and can correct a failed update.

Acceptance:

- Submitting a replacement stops use of the old cookie and invalidates its
  authentication and registration confirmation. Late results cannot restore them.
- Save the replacement before using it. A failed save blocks authenticated
  requests and reports an error so the user can try again; never fall back.
- A saved replacement requires fresh registration confirmation. Rejection reports
  an error so the user can correct or retry it, without using the old cookie.
- Cookie updates and user retries preserve pause and all request limits.
- Implemented. A replacement MAM sends is saved through the same setting; if that
  save fails, Windlass keeps using the saved cookie and reports the failure.

## Feature-wide constraints and acceptance

- Apply [Direction](../../direction.md), including manual requests and retries.
- Exercise HTTP behaviour against the local fake MAM server using synthetic data
  ([B-0012](../B-0012/brief.md)); the NixOS VM test also checks that registration
  leaves through the tunnel.

### External API calls

Both requests use HTTPS on `t.myanonamouse.net`, with no query parameters or body:

| Purpose | Request | Authentication | Source |
| --- | --- | --- | --- |
| Register seedbox | `GET /json/dynamicSeedbox.php` | `Cookie: mam_id=<API cookie>` | [Official documentation](../../../docs/MAM/dynamicSeedbox.txt) |
| Observe IP | `GET /json/jsonIp.php` | No cookie attached | [Official documentation](../../../docs/MAM/jsonIp.txt) |

Registration requires an IP- or ASN-locked API cookie with update permission.
The owner uses an ASN-locked session. ProtonVPN failover ([B-0010](../B-0010/brief.md))
can move to a server in an ASN not yet added to the session; the ASN reported by
`jsonIp.php` is compared with the user's list, and an unknown ASN is reported
before any registration attempt.
MAM observes the source IP; no IP or port is submitted. Rule 1.2 requires VPN
IP changes less than once an hour. Staff notification is complete.

## Scope

API-cookie access, seedbox registration, exit-IP observation, and their status.

## Non-goals

Search, acquisition, account polling, bonus purchases, library organisation,
and VPN implementation.

## Related records

- [Single core](../../decisions/adr/0001-single-core.md)
- [Privacy boundary](../../decisions/pdr/0001-vpn-privacy-boundary.md)
- [Supplied MAM API sources](../../references.md#mam-rules-and-api-documentation)
- [Mousehole request implementation](https://github.com/t-mart/mousehole/blob/master/src/backend/external-api/mam.ts)

## Open questions and assumptions

- How should failed cookie replacement recover across restart when the old value
  remains in storage? Until decided, the stored value is used after a restart.
- Which changes should allow another attempt after each documented 403 rejection?
- How should server-directed delays without a usable deadline be handled?
- What should happen if a cooldown cannot be saved or restored, or another tool
  updates registration using the account?
- Account-data loading belongs to the [acquisition brief](../B-0005/brief.md).
