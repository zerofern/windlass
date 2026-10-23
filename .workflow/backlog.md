# Backlog

## B-0001: Disk-space monitoring and download holds

- Status: Needs Direction
- Value: Show storage pressure and hold new downloads when disk space is
  insufficient, while allowing existing torrents to keep seeding.
- Direction: [Disk space](direction.md#disk-space)

## B-0002: Important-event notifications

- Status: Needs Direction
- Value: Notify the user through hosted ntfy.sh about important events without requiring them to
  continually check Windlass.
- Direction: [Notifications](direction.md#notifications)

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

## B-0009: Audiobookshelf integration

- Status: Needs Direction
- Value: Connect Windlass's acquisition and library experience with Audiobookshelf.
- Direction: [Audiobookshelf integration](direction.md#audiobookshelf-integration)
