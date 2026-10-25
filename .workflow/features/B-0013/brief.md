# Download queue, torrent limits, and what Windlass did

Status: Draft
Feature ID: B-0013

## Goal

The owner bookmarks a torrent and it ends up downloaded and seeding, without the
connection becoming unusable and without breaking any MAM rule. The owner can
see and reorder what is coming, and when something unexpected happens, Windlass
says what happened and what it did about it.

## Stories and acceptance

### S1: Limit active and downloading torrents

Story: The owner sets how many torrents may download at once, and how many may
be active at once, so the connection stays usable.

Acceptance:

- Two settings on the dashboard, saved like the others: most torrents
  downloading (default 5) and most torrents active (default 10). Active means
  downloading, or seeding while peers download from it; a seed nobody is
  downloading from does not count, so it keeps seeding and earning points.
- Windlass starts no new download while its unfinished downloads reach the
  downloading limit.
- Windlass sets qBittorrent's queue to match: queueing on, the same downloading
  and active limits, and idle torrents not counted. qBittorrent then pauses
  (queues) torrents above the limits, and resumes them as others go idle.
- A torrent still short of MAM's 72 hours of seeding is never held in the queue:
  Windlass force-starts it, so a limit can never make it a hit-and-run.
- Windlass sets every other qBittorrent setting it depends on (listen port,
  interface, queue), and puts back any that drift.

### S2: The download queue

Story: The owner sees the bookmarks waiting to download, in the order Windlass
will download them, and can change that order.

Acceptance:

- A queue page lists queued torrents in planned order, with why the first one
  waits, if it does. The default order is the bookmark date, oldest first.
- The owner can move a torrent up or down, or to the top. The order is saved.
- The owner can mark one torrent "download now": it may go above the
  downloading and active limits, once. It never goes above MAM's limits: the
  unsatisfied limit and its reserve of 2, the ratio floor, the client and seedbox
  gates, and the wedge rules all still apply.
- Snatched bookmarks are listed separately with "Download again", as now.

### S3: Keep enough unsatisfied room

Story: Unsatisfied torrents never exceed what the account's class allows, also
when VIP ends or the account drops a class.

Acceptance:

- Windlass works out the class the account would fall to when VIP ends: Power
  User (100 unsatisfied, rule 2.8) while the ratio keeps that class, User (50)
  otherwise. It follows the ratio in every reading, because a falling ratio
  takes Power User away too.
- When VIP would lapse and MAM's unsatisfied count is above that class's limit,
  Windlass renews VIP ahead of other purchases.
- When the ratio comes close to losing a class, the owner is told before it
  happens, with the unsatisfied count and the limit it would fall to.
- Downloads already hold when a download would leave fewer than 2 slots free
  (B-0011 A2).

### S4: Say what happened and what Windlass did

Story: When something unexpected happens, the owner hears about it, names and
all, together with what Windlass did to resolve it.

Acceptance:

- Notices that concern a torrent tell the story, for example: "*Title* stopped
  being free while downloading. Windlass paused it, spent a wedge, and resumed
  it." When freeleech ends mid-download, Windlass pauses the torrent first, then
  wedges it, then resumes it once MAM's bonus history shows the wedge.
- Every notice is kept on the dashboard's notifications page, newest first, with
  its full text and its own address.
- A push carries no content: it says only that Windlass has a notice, and links
  to that notice's entry on the notifications page. Nothing about the account or
  its torrents leaves the LAN.
- Each notice is either urgent or quiet. Urgent ones are pushed; quiet ones
  (a torrent finished, a purchase settled, a repair that worked) are only on the
  notifications page.

### S5: Never a hit-and-run

Story: A torrent short of MAM's seeding requirement keeps seeding, whatever
happens, and when Windlass cannot keep it seeding the owner knows at once.

Acceptance:

- Windlass treats every unsatisfied torrent as one it must seed: it is never
  paused, queued, or removed by Windlass, and is force-started if qBittorrent
  stops it.
- When an unsatisfied torrent is not seeding (the VPN is down, qBittorrent is
  unreachable, the torrent is stopped or in error, MAM reports the client not
  connectable), Windlass tries what it can (restart the tunnel check, renew the
  port lease, register the seedbox, start the torrent) and sends an urgent
  notice saying what is wrong and what it tried.
- While it stays unresolved and needs the owner, the notice is repeated, at
  most once an hour. When seeding resumes, a quiet notice says so.

### S6: Millionaire's Vault reminder

Story: When the vault pays out, the owner is reminded to donate again, so the
next payout includes them.

Acceptance:

- When the account's wedges rise by 8 or more without a purchase by Windlass,
  Windlass takes it as a vault payout and sends a notice to donate again, with a
  link to the vault page.
- Windlass never donates; the API does not offer it.

## Feature-wide constraints and acceptance

- MAM's rules and B-0011's invariants come first; no setting or override relaxes
  them.
- The request budget is 10 at once, then one every 3 seconds (was 5 and one
  every 10 seconds). It guards against a bug sending many requests, not against
  MAM's limits, which each request type keeps on its own (IP lookup once a
  minute, seedbox registration once an hour).

## Scope

Download and active limits, qBittorrent's queue settings, the queue page,
reordering and "download now", VIP renewal for the unsatisfied limit, ratio and
class watching, narrated notices, the notifications page, content-free pushes,
quiet notices, hit-and-run prevention, and the vault reminder.

## Non-goals

Downloading torrents the owner did not bookmark; speed limits; disk space
(B-0001); cleanup (B-0006); donating to the Millionaire's Vault, which the API
does not offer.

## Related records

- [MAM account safety](../B-0011/brief.md)
- [MAM acquisition](../B-0005/brief.md)
- [Notifications](../B-0002/brief.md)
- [qBittorrent integration](../B-0007/brief.md)

## Open questions and assumptions

- Decided: pushes carry only a link to the notice's entry, so titles stay on the
  LAN (B-0002's rule holds).
- Decided: when VIP ends, the account falls to Power User while its ratio keeps
  that class. The ratio Power User needs is not in the captured rules; assumed
  2.0 until confirmed, and the fallback is User (50) below it.
- VIP renewal for the unsatisfied limit: may it dip into the points reserve when
  nothing else keeps the count within the limit? Proposed: yes, with an urgent
  notice; otherwise downloads hold and the owner is told.
- "Download now" lasts until that torrent finishes downloading.
- Millionaire's Vault: bonus history shows donations (`type` `millionaires`,
  `amount` 2000) but no payout entry has been seen yet, so the payout is taken
  from a jump in wedges. When a payout shows in the history, Windlass should use
  that instead. The vault page's address is still needed for the link.
