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
when VIP ends.

Acceptance:

- When VIP would lapse and MAM's unsatisfied count is above the limit of the
  class the account would fall to (100 for Power User, by rule 2.8), Windlass
  renews VIP ahead of other purchases.
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
  its full text, and every push links to its entry there.

## Feature-wide constraints and acceptance

- MAM's rules and B-0011's invariants come first; no setting or override relaxes
  them.
- The request budget may be loosened: it guards against a bug sending many
  requests, not against MAM's limits, which each request type keeps on its own.

## Scope

Download and active limits, qBittorrent's queue settings, the queue page,
reordering and "download now", VIP renewal for the unsatisfied limit, narrated
notices, and the notifications page.

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

- Book titles in push notifications: B-0002 keeps titles out of notices because
  ntfy.sh is a hosted service outside the VPN. Proposed: the push says what
  happened without the title ("A download stopped being free…") and links to the
  notifications page, which shows the title on the LAN.
- VIP renewal for the unsatisfied limit: may it dip into the points reserve when
  nothing else keeps the count within the limit? Proposed: yes, with an urgent
  notice; otherwise downloads hold and the owner is told.
- The class the account falls to when VIP ends is assumed to be Power User
  (limit 100); MAM reports only the current class's limit.
- New request budget, proposed: 10 at once, then one every 3 seconds (was 5 and
  one every 10 seconds). MAM's own limits (IP lookup once a minute, seedbox
  registration once an hour) stay.
- "Download now" lasts until that torrent finishes downloading.
- Millionaire's Vault reminder: waits for a capture of
  `userBonusHistory.php?type[]=millionaires` and the vault page's address.
