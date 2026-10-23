# Web dashboard and MAM pause control

Status: Draft
Feature ID: B-0003

## Goal

Give the user one local web page to understand what Windlass is doing, why work
is blocked, and what has happened, and to configure Windlass and pause its own
MAM requests. This brief owns the dashboard framework, pause control, and history.
Each feature brief owns the status panel and settings for its own workflow.

## Stories and acceptance

### S1: See current status

Story: The user opens the dashboard and sees the current state of Windlass,
including where intervention is needed.

Acceptance:

- Status updates live, without reloading the page.
- The status shown is a view produced by the core. Handlers and templates do not
  derive status or blocking reasons themselves.
- If the live connection to Windlass is lost, the page shows it is disconnected
  rather than presenting the last status as current.
- The dashboard remains available while VPN protection is unavailable, so the
  user can diagnose the problem.
- Each feature contributes its own panel. Initially:
  - MAM initialization ([B-0004](../B-0004/brief.md)): setup required,
    unverified authentication, rejected session, confirmed registration, and
    failed or deferred registration with the next eligible update time.
  - Acquisition ([B-0005](../B-0005/brief.md)): why new downloads are blocked.
  - Pause and VPN protection state (this brief).

### S2: Pause and resume MAM requests

Story: The user pauses all of Windlass's own MAM requests and knows the pause
will survive a restart.

Pause is the only control for disabling MAM contact; there is no separate
"contact enabled" setting.

Acceptance:

- Pause behaves as defined in [Direction](../../direction.md#web-dashboard): it
  discards queued requests, ignores new ones, cancels outstanding requests where
  possible, and does not replay dropped work on resume.
- Pause does not affect qBittorrent tracker communication.
- Blocking takes effect immediately; the dashboard distinguishes "pause pending
  save" from "paused".
- The dashboard confirms the pause only after it has been durably saved. A
  failed save is shown as a failure and never as confirmation.
- Resume requires an explicit user action. VPN recovery does not clear a pause.
- While paused, authentication is shown as unverified and IP reporting as
  unconfirmed, not as rejected.

### S3: Configure Windlass

Story: The user changes operational settings from the dashboard and knows
whether each change was saved.

Acceptance:

- Configuration is split by owner:
  - NixOS module: deployment facts such as bind address, filesystem paths, and
    the qBittorrent endpoint. The dashboard may display these but cannot change them.
  - SQLite, edited from the dashboard: operational settings such as the MAM API
    cookie, the ntfy topic, and disk thresholds. Later
    features add their own settings here.
- Secrets, including the MAM API cookie, are write-only. The dashboard can set,
  replace, or clear them and show whether one is configured, but never shows
  their value.
- Each change is acknowledged as saved only after the save succeeds, following
  [Application foundation S4](../B-0008/brief.md#s4-acknowledge-durable-changes-truthfully).
- Invalid values are rejected with a reason and do not replace the saved value.

### S4: Review history

Story: The user reviews what Windlass received and did, in order, with when each
thing happened and when Windlass processed it.

Acceptance:

- Record every event the core handles and every action it returns, in core
  processing order.
- Each entry is structured, not free text: a kind, typed fields, and identifiers
  that link related entries.
- Event entries record when the event happened and when the core processed it.
  For example, a timer expiry records its deadline, its delivery, and its
  processing time; an HTTP result records when the response arrived.
- Action entries record which processing step produced them. When an operation
  later completes, fails, or is cancelled, its result event links back to the
  action.
- Entries show when the core ignored an event as late, obsolete, or superseded,
  so ignored results are visible rather than silently missing.
- Secrets never appear in the log. A cookie change is recorded as "replaced",
  without the value.
- The dashboard can filter history by kind and time range.
- History is stored in SQLite and kept within a size bound. The core decides
  when the bound is exceeded and which oldest entries to remove; the shell only
  carries out the removal. Trimming is itself recorded in history.

## Feature-wide constraints and acceptance

- Follow the [single-core operating model](../../decisions/adr/0001-single-core.md).
  The dashboard is a shell adapter: HTTP requests become core events, and replies
  and status views are core actions. The shell does not borrow the core.
- Command replies are correlated by request identity, so a slow save delays only
  that reply and does not block other inputs.
- Render HTML on the server. The build must not require Node or a JavaScript
  bundler. Small progressive enhancement for live updates is acceptable.
- Access is through the LAN and Tailscale only; Windlass has no login of its own.
  The listen address comes from the NixOS module.
- Because there is no login, state-changing requests must not be possible from
  other websites the user visits. Reject cross-origin requests and unexpected
  `Host` headers (cross-site request forgery and DNS rebinding).
- Test status views, command acknowledgements, and history entries in the core
  with concrete events and supplied time. Cover the HTTP adapter and the live
  status stream with coarse integration tests.

## Scope

The web server, page layout, live status channel, command and reply plumbing,
pause and resume, the configuration page framework, secret handling, and the
history log and its viewer.

## Non-goals

Feature-specific status panels and settings beyond those listed in S1 and S3
belong to their own briefs. Remote access other than the LAN and Tailscale, user
accounts, and multi-user access are excluded. Notifications belong to
[B-0002](../../backlog.md#b-0002-important-event-notifications).

## Related records

- [Direction](../../direction.md#web-dashboard)
- [Single core](../../decisions/adr/0001-single-core.md)
- [SQLite persistence](../../decisions/adr/0002-sqlite-persistence.md)
- [Privacy boundary](../../decisions/pdr/0001-vpn-privacy-boundary.md)
- [Application foundation](../B-0008/brief.md)
- [MAM initialization](../B-0004/brief.md)

## Open questions and assumptions

Agreed with the owner: server-rendered HTML without Node, the configuration
split above, LAN and Tailscale access without a login, framework ownership in
this brief with panels in feature briefs, a structured history of all events
and actions stored in SQLite and trimmed by the core, and pause as the only way
to disable MAM contact.

Proposed, pending agreement:

- History is a record for the user, not the source of truth. Restart restores
  saved state and does not replay the log.
- History entries carry wall-clock timestamps for display. Processing order is
  recorded separately, so clock changes cannot reorder entries.
- The size bound is a maximum entry count set in the dashboard. Trimming removes
  enough oldest entries to stay comfortably under it, rather than deleting one
  entry per new write.
- A fresh installation starts paused. The user configures the API cookie and
  then resumes to begin MAM contact.
- A failed history write does not stop the core. The gap is recorded as an
  explicit entry once writing recovers, so the log never looks complete when it
  is not.
- Routine status-publishing actions are recorded but hidden by default in the
  viewer, so that periodic work does not crowd out meaningful entries.

Still open:

- What is the default history size bound?
- Should history be exportable, for example as JSON lines?
