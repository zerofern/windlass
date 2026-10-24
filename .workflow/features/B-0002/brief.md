# Important-event notifications

Status: Draft
Feature ID: B-0002

## Goal

Tell the owner about important events through hosted ntfy.sh, so they stay
informed without checking the dashboard, and let each notification lead straight
to the dashboard page that explains it.

## Stories and acceptance

### S1: Hear about lasting problems, and when they end

Story: The owner learns when something stays wrong long enough to matter, and
again when it is resolved, without being told about short blips.

Acceptance:

- Each condition notifies once it has held for its grace period, and again when
  it clears. A condition that clears within its grace period sends nothing.

  | Condition | Notify after |
  | --- | --- |
  | VPN protection lost | 3 failed tunnel checks, when failover is considered |
  | VPN switch blocked (hourly limit, last switch unknown, no other server) | Immediately |
  | No forwarded port | 5 minutes; not counted while a server switch drops the lease |
  | qBittorrent not responding | 5 minutes |
  | qBittorrent version not allowed by MAM | Immediately |
  | Torrents in an error state | 5 minutes |
  | Files missing on disk | 10 minutes |
  | Active torrents missing from qBittorrent | 10 minutes |
  | Torrent records not saved | 2 minutes |

- VPN protection clears only after a successful tunnel check. A blocked switch
  sends no notice of its own when it ends; the switch or restored protection
  that ends it is announced instead.

### S2: Hear about events as they happen

Story: The owner learns at once about events that do not last.

Acceptance:

- Windlass started (each start, with whether storage was new or resumed) and
  startup failed.
- A VPN server switch started, succeeded, or failed.
- A setting change that could not be saved, history that could not be opened,
  history entries that could not be saved, and torrent records that could not
  be read.
- A test notification requested from the settings page.

### S3: Deliver reliably and privately

Story: Notifications arrive even while the VPN is down, and say nothing that
identifies books or the account.

Acceptance:

- The ntfy topic is set on the settings page, stored like the API cookie, and
  recorded in history only as `[redacted]`. Without a topic, nothing is sent.
- Windlass reaches ntfy through the host relay of
  [ADR-0003](../../decisions/adr/0003-vpn-namespace-confinement.md); the server's
  name resolves to the relay, so HTTPS verifies ntfy's certificate end to end.
- Each notice has a title, a priority, tags, and a link to the dashboard page
  that explains it (`services.windlass.dashboard.url`).
- Notices carry conditions and counts only, never book titles or account
  details.
- One notice is delivered at a time. A failed delivery is retried after 30
  seconds, then 1, 2, 4, and 8 minutes, then every 10 minutes. At most 50 notices
  wait; beyond that the oldest are dropped and counted.
- The status page shows the last delivery result, failed attempts, notices
  waiting, notices dropped, and conditions reported but not yet resolved.

## Feature-wide constraints and acceptance

- The core decides what to tell the user and when, with supplied time; the shell
  only formats and publishes. Core tests drive grace periods, one-off notices,
  and retries with events.
- Shell tests publish to a local fake ntfy through a stand-in relay. Tests never
  contact ntfy.sh.
- The NixOS VM test sets a topic from the dashboard, sends a test notification,
  and checks that failover notices arrive through the relay while the VPN is
  down.

## Scope

ntfy delivery, the conditions and events above, the topic setting, and the
dashboard's notification panel.

## Non-goals

Other channels, per-condition muting, and notifications for features not yet
built, such as disk space (B-0001) and MAM registration (B-0004); those features
add their own conditions.

## Related records

- [Direction](../../direction.md#notifications)
- [ntfy reference](../../references.md#ntfy)
- [VPN confinement](../B-0010/brief.md)
- [Web dashboard](../B-0003/brief.md)

## Open questions and assumptions

Agreed with the owner: the conditions, events, and grace periods above; the topic
on the settings page; a notice at every start; a dashboard link in every notice.
