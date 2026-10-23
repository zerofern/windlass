# MAM initialization loop

Status: Draft
Feature ID: B-0004

## Goal

Define the actual MAM requests needed to establish API access and maintain
seedbox IP registration. Keep request outcomes visible without bypassing the
agreed pause, privacy, and rate-limit policies.

## Stories and acceptance

### S1: Enable MAM contact

Story: The user configures an API cookie and resumes MAM requests. Windlass
establishes whether it can register the seedbox through the protected connection.

Acceptance:

- A paused instance or unavailable VPN protection produces no new MAM requests.
- No email/password login or undocumented session-check endpoint is used.
- The dashboard distinguishes unverified authentication, a rejected session,
  confirmed registration, and failed or deferred registration.

Agreed request sequence:

1. Load saved pause state and endpoint cooldowns before making requests.
2. If MAM requests are not paused, VPN protection is available, and an API
   cookie is configured, request seedbox registration when the shared bucket
   and endpoint cooldown permit it.
3. Send `GET https://t.myanonamouse.net/json/dynamicSeedbox.php` with the
   `mam_id` API cookie. No IP parameter is sent: MAM observes the source IP.
4. Treat documented successful `Completed` and `No Change` outcomes as confirmed
   registration. Preserve a replacement API cookie if the response supplies one.
5. Do not interpret an IP returned with an error as proof of registration.

If no API cookie is configured, show setup required and make no MAM requests.

### S2: Keep registration current

Story: While MAM requests are not paused, the system detects a changed exit IP and
updates registration without exceeding the documented limits.

Acceptance:

- Requests use the common admission policy, including manual requests and retries.
- Calls to dynamicSeedbox are limited to one request per rolling hour, including
  calls that might return `No Change`. Mousehole's more frequent contact schedule
  does not override the supplied MAM API documentation.
- Pausing or losing VPN protection discards queued requests and invalidates
  confidence in authentication and IP reporting, as defined in Direction.

Agreed monitoring and recovery sequence:

- Periodically send `GET https://t.myanonamouse.net/json/jsonIp.php` to observe
  the exit IP every five minutes while MAM requests are not paused.
  Never exceed its documented once-per-minute limit.
- If that IP differs from the confirmed registration, request a seedbox update
  when allowed. An unchanged observed IP alone does not require another update.
- Establish fresh confirmation after resume or VPN recovery rather than replaying
  requests discarded during the interruption. Request
  registration again when allowed, then resume periodic IP observation.
- If the cooldown delays registration, keep its status unconfirmed and show the
  next eligible update time. Do not treat an IP lookup as registration success.
- Before sending a queued registration update, reconsider whether it is still
  needed using the latest observation. IP changes must not accumulate updates.

### S3: Explain failures

Story: When MAM contact fails, the user can distinguish a configuration problem,
a cooldown, and a transport failure without Windlass repeatedly sending requests.

Acceptance:

- A documented 403 session or permission rejection is distinct from a transport
  failure and from unverified authentication.
- A 429 response defers work; available bucket tokens do not override it.
- A timeout or malformed response does not confirm registration.
- Failure handling and retry timing remain to be agreed before this brief is Ready.

Proposed failure handling, pending agreement:

| Outcome | Status and response |
| --- | --- |
| HTTP 200 and documented success | Confirm registration for the returned IP. |
| HTTP 403 | Show the specific session or permission rejection; stop automatic registration retries until relevant configuration or network conditions change. |
| HTTP 429 | Registration remains unconfirmed; wait for the endpoint cooldown and any longer server-directed delay. |
| Timeout, transport failure, malformed response, or unexpected status | Outcome is uncertain; do not confirm registration or immediately repeat the request. |

A failed or cancelled request may have reached MAM. Proposed policy: preserve
the seedbox cooldown after dispatch even if its outcome is uncertain. Exact
retry timing for other endpoints remains open.

## Feature-wide constraints and acceptance

- Apply the shared policies in [Direction](../../direction.md).
- Exercise HTTP behaviour against the local fake MAM server using synthetic data.

### External API calls

Both requests use HTTPS on `t.myanonamouse.net`, with no query parameters or body:

| Purpose | Request | Authentication | Source |
| --- | --- | --- | --- |
| Register seedbox | `GET /json/dynamicSeedbox.php` | `Cookie: mam_id=<API cookie>` | [Official documentation](../../../docs/MAM/dynamicSeedbox.txt) |
| Observe IP | `GET /json/jsonIp.php` | No cookie attached | [Official documentation](../../../docs/MAM/jsonIp.txt) |

Registration requires an IP- or ASN-locked API session with seedbox-update
permission. No IP or port is submitted; MAM observes the source address.
Registration responses contain `Success`, `msg`, `ip`, `ASN`, and `AS`.
The field table spells `asn` differently from the examples; examples also vary
`No Change` capitalisation. Preserve these discrepancies in contract fixtures.
IP observations contain `ip`, `ASN`, and `AS`, optionally `time`; they establish
neither authentication nor registration. Keep fixtures for both source examples.
Cookie rotation uses `Set-Cookie`; handling a failed cookie save remains open.

## Scope

API-cookie access, seedbox registration, exit-IP observation, and their status.

## Non-goals

Torrent search, downloading torrent files, account polling, bonus purchases,
library organisation, and VPN implementation are separate flows to discuss.

## Related records

- [Single core](../../decisions/adr/0001-single-core.md)
- [Privacy boundary](../../decisions/pdr/0001-vpn-privacy-boundary.md)
- [Supplied MAM API sources](../../references.md#mam-rules-and-api-documentation)
- [Mousehole request implementation](https://github.com/t-mart/mousehole/blob/master/src/backend/external-api/mam.ts)

## Open questions and assumptions

- What fresh requests should a changed cookie trigger?
- How should retries, server-directed cooldowns, and uncertain outcomes recover?
- How should endpoint cooldowns survive restarts and requests shared with other
  tools using the account?
- Account-data loading belongs to the [acquisition brief](../B-0005/brief.md).
