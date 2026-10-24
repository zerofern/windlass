# Fake MAM server

Status: Draft
Feature ID: B-0012

## Goal

Give every MAM feature a local stand-in for MAM that behaves as the real site was
observed to behave, so integration tests can prove Windlass keeps every rule in
[B-0011](../B-0011/brief.md) without ever contacting MAM.

## Stories and acceptance

### S1: Answer like MAM

Story: Windlass's shell talks to the fake exactly as it would to MAM, and gets
the responses the owner captured, with synthetic data.

Acceptance:

- The fake serves the endpoints Windlass uses, on a `www` and a `t` host:
  Load User Data, Torrent Search, Download Torrent, Bonus Points Store, User Bonus
  History, IP lookup, and Dynamic Seedbox. Any other path answers 404 and is
  recorded, so an unsupported call fails the test visibly.
- Responses follow the shapes recorded in B-0011's observed responses, including
  their quirks:
  - flag parameters are honoured only with a value (`clientStats=true`), and a
    bare flag is ignored;
  - Torrent Search honours `searchIn` only in a JSON POST, while `tor[id]` works
    by GET too; `bookmarked` is a Unix time;
  - `ratio` is display text (`"∞"`, `"inf."`) and sizes in search are display
    text; byte counts are integers and the points balance in a purchase
    response has a fraction;
  - a refused download is HTTP 200 with an empty HTML body;
  - session errors are text, not JSON: `Invalid/missing cookie` and
    `Session ASN mismatch`.
- The fake holds an account: uploaded and downloaded bytes, points, wedges,
  class, VIP expiry, the snatch summary's categories, connectable status,
  clients, notification counts, and wedge history. A catalogue of synthetic
  torrents holds each torrent's flags, bookmark time, snatched state, and a
  small bencoded torrent file.
- Actions change that state as MAM does:
  - `download.php?tid=<id>&fl=1` spends one wedge at once and records a
    `wedgePF` history entry, while search keeps reporting the old
    `personal_freeleech` until the test lets the lag end;
  - upload credit costs 500 points per GiB, in whole GiB of at least 50 or
    `Max Affordable `, and adds exact bytes;
  - VIP `max` fills to 90 days from the purchase at 1,250 points a week, at
    least 7 days, and reports the weeks bought;
  - wedges are not for sale; `gift` and `sendWedge` are refused and recorded as
    a violation;
  - an unaffordable or invalid purchase answers `success: false`.
- Every response sets a new `mam_id` value with a 15-day lifetime. Earlier
  values keep working until the test expires them.
- Sessions are bound to an ASN chosen by the test, and the IP lookup reports the
  IP and ASN the test sets, so tests can cover an ASN mismatch after failover.

### S2: Let tests steer and inspect

Story: A test sets up a situation, drives Windlass through it, and checks what
Windlass asked MAM to do.

Acceptance:

- A control interface, separate from the MAM routes, sets the account,
  catalogue, clients, sessions, IP and ASN; ends the search flag lag; and
  injects faults: an HTTP error, malformed JSON, a delay, a rate-limit answer,
  or a dropped connection, for one call or until cleared.
- A request journal records each request's method, host, path, query, body,
  `User-Agent`, and which session cookie it used. Tests assert on it, for
  example that every request names `Windlass/<version>`, that no purchase
  followed an uncertain one, that the IP lookup and seedbox registration kept
  their spacing, and that no forbidden call was made.
- The fake enforces the documented rate limits (IP lookup once a minute,
  seedbox registration once an hour) and records any breach as a violation,
  using time the test controls rather than real waiting.
- A reset returns the fake to its initial state between tests.
- Implemented in process rather than as HTTP routes: a test changes the fake's
  state through a closure and reads its journal and violations directly. Each
  test starts its own fake, so no reset is needed. A dropped connection is not
  yet offered as a fault.

### S3: Run where the tests run

Story: The same fake serves Windlass's in-process integration tests and the
NixOS VM test.

Acceptance:

- Integration tests start the fake in process on a random local port, as the
  existing tunnel and notification tests do for their stand-ins.
- The VM test runs the fake as a service in the VM, reachable through the VPN
  path, so Windlass's MAM traffic is proven to leave through the tunnel.
- Windlass takes MAM's base URLs from configuration, defaulting to the real
  hosts, so tests can point it at the fake. No test build changes Windlass's
  code paths.
- Implemented for integration tests (`tests/support/fake_mam.rs`) and through
  `--mam-site` and `--mam-tracker`, which the NixOS module sets to MAM's hosts by
  default. The VM test stands in for the IP lookup and seedbox registration with
  nginx rather than the Rust fake, which is test code; that is enough to show
  registration leaves through the tunnel with the VPN's exit IP.

## Feature-wide constraints and acceptance

- The fake is test code: it is not part of the Windlass package or the NixOS
  module's service.
- Fixtures are synthetic. They keep the shapes the owner captured but never the
  owner's username, uid, cookie values, IPs, or real torrent details.
- The fake's own tests pin each response shape against B-0011's observed
  responses, so a change to the fake that drifts from MAM fails.
- Passing against the fake shows agreement with the recorded responses, not
  with the live site. A shape MAM changes later is caught by the owner's
  captures, not by the fake.

## Scope

The endpoints, behaviours, control interface, and journal above, used by B-0004,
B-0005, and B-0011.

## Non-goals

Anything MAM offers that Windlass does not call; HTML pages; reproducing MAM's
rules beyond what Windlass's tests need.

## Related records

- [Single core, fake MAM for shell tests](../../decisions/adr/0001-single-core.md)
- [MAM account safety, observed responses](../B-0011/brief.md#observed-responses)
- [MAM initialization](../B-0004/brief.md)
- [MAM acquisition](../B-0005/brief.md)

## Open questions and assumptions

- The Dynamic Seedbox responses and a client list entry are not yet captured
  (B-0011 calls 3 and 12). Until they are, the fake follows the documentation
  and these responses are marked as unconfirmed.
- The archived POC fake (`POC/windlass-testkit`) is audited before any of it is
  reused, as ADR-0001 requires.
