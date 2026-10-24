# ADR-0004: A torrent registry as the desired state

Status: Accepted

## Context

Windlass manages every torrent in qBittorrent; the owner does not add or remove
torrents by hand. Direction requires durable acquisition history that survives
deleted files and removed client entries, so that a torrent already downloaded
is not downloaded again automatically. MAM limits how many unsatisfied torrents
an account may hold, so selected torrents can wait for a free slot. MAM does not
allow partial sharing: a torrent is shared whole or not at all.

qBittorrent's own list is not a sufficient record. It forgets a torrent when the
torrent is removed, holds no MAM torrent IDs, and can change outside Windlass's
control after a crash, restore, or disk failure.

## Decision

Windlass keeps a torrent registry in SQLite. Each row records one torrent Windlass
knows about, and rows are never deleted. The registry is the desired state:
Windlass compares it with what qBittorrent reports and acts to make reality match.

Each row separates intent from observation.

**Intent**, changed only by Windlass's decisions or the user's commands:

- The MAM torrent ID, the stable link to the torrent on MAM. Windlass does not
  store `.torrent` files; when it needs the metadata again, it downloads it from
  MAM, subject to pause, VPN protection, and MAM's request limits.
- The info hash, once known.
- How the torrent arrived: a MAM bookmark, an explicit re-download, or adoption
  of a torrent found in qBittorrent without a row.
- The desired state:
  - *Queued*: selected, but waiting for a free MAM slot or another admission check.
  - *Active*: present in qBittorrent, whole, and running; downloading until
    complete, then seeding.
  - *Removed*: absent from qBittorrent. The row stays as download history.
- The save path, and whether the user allows the content to be deleted for space.

**Observation**, written as qBittorrent reports changes:

- The qBittorrent state and sharing status, progress, and when the torrent was
  added, started, and completed.
- The content path, the total size, and the size on disk.
- Upload totals, ratio, and seeding time, and when the torrent was last seen.

Only intent drives actions. The core reconciles each row with qBittorrent's
updates:

| Registry | qBittorrent | Action |
| --- | --- | --- |
| Active | Stopped | Start it. |
| Active | Some files not selected | Select every file; MAM does not allow partial sharing. |
| Active | Missing | Download the metadata from MAM and add it again; qBittorrent checks files already on disk. |
| Active | Content missing on disk | Notify the user and wait; do not download the content again without them. |
| Removed | Present | Remove it and its files. |
| No row | Present | Adopt it as Active, with its MAM torrent ID unknown until established. |
| Queued | Absent | Admit it when a MAM slot is free and the admission checks pass. |

Observations are saved when they change meaningfully: a state change, completion,
a new content path, or a size change. Upload totals and seeding time are saved
about every 15 minutes rather than on every poll. The completion time is saved
durably because cleanup deletes the oldest completed content first.

## Rationale

A desired-state registry gives one durable answer to "what should exist", so
recovery after a crash, a restore, or a lost qBittorrent session is the same
reconciliation that runs every poll. Keeping history in the registry, rather than
in qBittorrent, satisfies Direction's rule that removing files must not erase the
fact of a download. The MAM torrent ID is a smaller and more durable link than a
stored `.torrent` file, and re-downloading metadata uses the same gated path as
any other MAM request.

## Scope

This decides the registry's role, its rows' meaning, and the reconciliation rules.
It does not decide the table schema, admission checks, MAM slot accounting, or
how an adopted torrent's MAM torrent ID is established.

## Consequences

- Re-adding a missing torrent needs MAM access; while paused or without VPN
  protection, the row waits.
- Missing content is never downloaded again silently; the user decides.
- The core holds the registry in memory and requests registry writes as actions,
  following [ADR-0002](0002-sqlite-persistence.md).
- Torrents found without a row are adopted, not removed.
- Schema version 4 stores the registry in the `torrents` table: one row per
  torrent with its intent columns, then the latest observation's columns, which
  are present when `last_seen` is set. Changed rows are saved in one transaction
  per batch, with one batch outstanding at a time.

## Related records

- [Direction](../../direction.md#cohesive-mam-experience)
- [SQLite persistence](0002-sqlite-persistence.md)
- [qBittorrent integration](../../features/B-0007/brief.md)
- [MAM acquisition](../../features/B-0005/brief.md)
