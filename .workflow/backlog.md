# Backlog

## B-0001: Disk-space monitoring and download holds

- Status: Needs Direction
- Value: Show storage pressure and hold new downloads when disk space is
  insufficient, while allowing existing torrents to keep seeding.
- Direction: [Disk space](direction.md#disk-space)

## B-0002: Important-event notifications

- Status: Ready for Shape
- Value: Notify the user through hosted ntfy.sh about important events without requiring them to
  continually check Windlass.
- Direction: [Notifications](direction.md#notifications)
- Feature Brief: [Important-event notifications](features/B-0002/brief.md)

## B-0003: Web dashboard and MAM pause control

- Status: Ready for Shape
- Value: Show current system status and history, let the user configure Windlass,
  and provide a control to pause all of Windlass's own requests to MAM.
- Direction: [Web dashboard](direction.md#web-dashboard)
- Feature Brief: [Web dashboard and MAM pause control](features/B-0003/brief.md)
- Relationships: Hosts status panels and settings owned by other feature briefs.

## B-0004: MAM initialization loop

- Status: Ready for Shape
- Value: Establish MAM access and keep seedbox IP registration understandable while
  enforcing privacy, pause, and API restrictions.
- Direction: [MAM API compliance](direction.md#mam-api-compliance)
- Feature Brief: [MAM initialization loop](features/B-0004/brief.md)

## B-0005: MAM search and torrent queueing loop

- Status: Needs Direction
- Value: Discover torrents selected through MAM bookmarks and queue them for
  download as a separate workflow from initialization.
- Direction: [Cohesive MAM experience](direction.md#cohesive-mam-experience)
- Feature Brief: [MAM acquisition](features/B-0005/brief.md)
- Relationships: Hands acquired
  torrent metadata to [qBittorrent integration](#b-0007-qbittorrent-integration).

## B-0006: Permission-based disk cleanup

- Status: Needs Direction
- Value: Free space for the next download by deleting user-approved books oldest
  first after seeding obligations are satisfied, retaining other books and
  acquisition history.
- Direction: [Disk space](direction.md#disk-space)

## B-0007: qBittorrent integration

- Status: Needs Direction
- Value: Submit acquired torrents, reconcile uncertain submissions, observe
  download completion and seeding, and execute permitted torrent cleanup.
- Direction: [Deployment](direction.md#deployment)
- Feature Brief: [qBittorrent integration](features/B-0007/brief.md)
- Relationships: Supports [MAM acquisition](#b-0005-mam-search-and-torrent-queueing-loop)
  and [cleanup](#b-0006-permission-based-disk-cleanup).

## B-0008: Application foundation

- Status: Ready for Shape
- Value: Provide predictable startup, persistent control state, responsive command
  handling, asynchronous results, and reliable timers without PostgreSQL or Docker.
- Direction: [Deployment](direction.md#deployment), [persistence](direction.md#local-persistence)
- Feature Brief: [Application foundation](features/B-0008/brief.md)
- Relationships: Foundation for MAM initialization, acquisition, and qBittorrent integration.

## B-0010: VPN confinement, port forwarding, and failover

- Status: Ready for Shape
- Value: Keep all BitTorrent and MAM traffic inside a ProtonVPN tunnel that fails
  closed, keep the forwarded port current, verify the tunnel works, and fail over
  between servers, while notifications and the dashboard stay available.
- Direction: [VPN privacy](direction.md#vpn-privacy), [Deployment](direction.md#deployment)
- Feature Brief: [VPN confinement, port forwarding, and failover](features/B-0010/brief.md)
- Relationships: Supplies VPN protection and exit-IP facts to
  [MAM initialization](#b-0004-mam-initialization-loop) and the forwarded port to
  [qBittorrent integration](#b-0007-qbittorrent-integration).

## B-0011: MAM account safety and bonus points

- Status: Needs Direction
- Value: Keep the account inside every MAM rule on ratio, unsatisfied torrents,
  seeding, clients, IPs, and automation, as MAM itself reports them, while
  Windlass acquires bookmarked torrents unattended without spending ratio; keep
  qBittorrent connectable; and spend bonus points on wedges, VIP, and upload
  credit when needed.
- Direction: [MAM API compliance](direction.md#mam-api-compliance)
- Feature Brief: [MAM account safety and bonus points](features/B-0011/brief.md)
- Relationships: Gates [MAM acquisition](#b-0005-mam-search-and-torrent-queueing-loop)
  and [cleanup](#b-0006-permission-based-disk-cleanup); adds conditions to
  [notifications](#b-0002-important-event-notifications).

## B-0012: Fake MAM server

- Status: Needs Direction
- Value: Let integration tests prove Windlass keeps MAM's rules against a local
  stand-in that answers as MAM was observed to, without contacting MAM.
- Direction: [MAM API compliance](direction.md#mam-api-compliance)
- Feature Brief: [Fake MAM server](features/B-0012/brief.md)
- Relationships: Supports [MAM initialization](#b-0004-mam-initialization-loop),
  [MAM acquisition](#b-0005-mam-search-and-torrent-queueing-loop), and
  [account safety](#b-0011-mam-account-safety-and-bonus-points).

## B-0013: Download queue, torrent limits, and narrated notices

- Status: Needs Direction
- Value: Bookmarked torrents download in an order the owner can see and change,
  within configurable downloading and active limits that keep the connection
  usable, and every unexpected event is reported with what Windlass did.
- Direction: [MAM API compliance](direction.md#mam-api-compliance),
  [Notifications](direction.md#notifications)
- Feature Brief: [Download queue, torrent limits, and what Windlass did](features/B-0013/brief.md)
- Relationships: Extends [MAM acquisition](#b-0005-mam-search-and-torrent-queueing-loop),
  [account safety](#b-0011-mam-account-safety-and-bonus-points), and
  [notifications](#b-0002-important-event-notifications).

## B-0009: Audiobookshelf integration

- Status: Needs Direction
- Value: Connect Windlass's acquisition and library experience with Audiobookshelf.
- Direction: [Audiobookshelf integration](direction.md#audiobookshelf-integration)
