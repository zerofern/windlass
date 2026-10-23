# MAM initialization loop

Status: Draft
Feature ID: B-0004

## Goal

Establish API access and maintain seedbox IP registration. Explain outcomes
while enforcing pause, privacy, and rate-limit policies.

## Stories and acceptance

### S1: Enable MAM contact

Story: The user configures an API cookie and enables MAM contact. Windlass
establishes whether it can register the seedbox through the protected connection.

Acceptance:

- A paused instance or unavailable VPN protection produces no new MAM requests.
- The API cookie is a user-supplied credential, used like an API key and sent
  as `mam_id`. Windlass changes it only through user configuration.
- No email/password login or undocumented session-check endpoint is used.
- The dashboard distinguishes unverified authentication, a rejected session,
  confirmed registration, and failed or deferred registration.

Load saved pause state and endpoint cooldowns before requests. Register when
contact is enabled, VPN protection is available, a cookie is configured, and
shared and endpoint limits permit. Only documented successful `Completed` and
`No Change` outcomes confirm registration; an error's returned IP does not.
Without a cookie, show setup required and make no MAM requests.

### S2: Keep registration current

Story: While MAM contact is enabled, the system detects a changed exit IP and
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
retries require contact to remain enabled and VPN protection to be available.
Show the failure and next eligible attempt time without requiring user intervention.

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

## Feature-wide constraints and acceptance

- Apply [Direction](../../direction.md), including manual requests and retries.
- Exercise HTTP behaviour against the local fake MAM server using synthetic data.

### External API calls

Both requests use HTTPS on `t.myanonamouse.net`, with no query parameters or body:

| Purpose | Request | Authentication | Source |
| --- | --- | --- | --- |
| Register seedbox | `GET /json/dynamicSeedbox.php` | `Cookie: mam_id=<API cookie>` | [Official documentation](../../../docs/MAM/dynamicSeedbox.txt) |
| Observe IP | `GET /json/jsonIp.php` | No cookie attached | [Official documentation](../../../docs/MAM/jsonIp.txt) |

Registration requires an IP- or ASN-locked API cookie with update permission.
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
  remains in storage?
- Which changes should allow another attempt after each documented 403 rejection?
- How should server-directed delays without a usable deadline be handled?
- What should happen if a cooldown cannot be saved or restored, or another tool
  updates registration using the account?
- Account-data loading belongs to the [acquisition brief](../B-0005/brief.md).
