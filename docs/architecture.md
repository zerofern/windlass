# Windlass Architecture Direction

Windlass is moving toward small sans-I/O state machines connected by typed
messages. The runtime shell owns I/O; cores own decisions.

## Design Rules

- Cores are pure state machines. They do not own Tokio tasks, HTTP clients,
  Docker clients, Postgres pools, files, sockets, or timers.
- Shells execute actions returned by cores. Results are reported back as timed
  events.
- Service-specific edge cases stay in service cores. The main Windlass domain
  core should express system policy, not qBittorrent cookies, MAM rate limits,
  Gluetun health polling, or SQL details.
- Database schema is defined in SQL migrations. Rust code uses SQLx
  compile-time checked queries against that schema.
- Published messages are normalized facts for subscribers. Low-level transport
  details stay inside the service core or shell that understands them.

## Crate Boundaries

```text
windlass-machine
  Generic Machine, Shell, Timed, Outcome, CommandOutcome, HasTopic, TopicFanout.

windlass-domain-core
  Main policy machine. Coordinates service facts and emits service commands,
  persistence commands, and user-visible state.

windlass-qbit-core
  qBittorrent state machine. Owns auth/session state, listen-port convergence,
  torrent refresh commands, retries, timeouts, and qBit-specific failures.

windlass-mam-core
  MAM state machine. Owns auth/session state, seedbox convergence, status
  refreshes, rate limits, retries, and MAM-specific failures.

windlass-vpn-core
  VPN/Gluetun state machine. Owns container health, forwarded-port observation,
  file-watch events, health-poll backstops, retries, and VPN-specific failures.

windlass-db-core
  Sans-I/O persistence protocol: DbCommand, DbEvent, durable record types, ids,
  and failure classification.

windlass-db
  Postgres adapter. Owns PgPool, migrations, and SQLx checked queries.

windlass runtime crates
  Tokio tasks, HTTP clients, Docker clients, Postgres pool, web routes, socket
  subscriptions, timers, and channel delivery.
```

## Message Flow

```text
External I/O
    |
    v
Service shells
    |
    v
qBit / MAM / VPN service cores
    |
    v
Normalized service publish messages
    |
    v
Windlass domain core
    |
    v
Service commands + DbCommand + WindlassPublish
```

The main domain core should mostly read like policy:

- when the VPN forwarded port is ready, ensure qBit and MAM converge on it
- when a service degrades, publish degraded system state and record activity
- when durable state changes, emit database commands
- when user commands arrive, return typed responses and emit service commands

## Generic Machine API

The shared `windlass-machine` crate provides the contracts used by each core:

```rust
pub trait Machine {
    type Config;
    type Event;
    type Action;
    type Publish;
    type Topic;
    type Command;
    type Response;

    fn new(config: Self::Config, now: Instant) -> Self;
    fn handle(&mut self, now: Instant, event: Self::Event) -> Outcome<Self::Action, Self::Publish>;
    fn handle_command(
        &mut self,
        now: Instant,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response>;
}
```

`Timed<E>` carries logical event time. Timer events should use the scheduled fire
time, not the runtime wake-up time.

## Database Direction

Postgres will replace the current embedded database layer. SQL files are the
source of truth:

```text
windlass-db/migrations/
  0001_initial.sql
```

`windlass-db-core` defines the persistence protocol:

```rust
pub enum DbCommand {
    RecordActivity(ActivityRecord),
    RecordAlert(AlertRecord),
    SaveSystemSnapshot(SystemSnapshotRecord),
    UpsertTorrent(TorrentRecord),
    UpsertBook(BookRecord),
    EnqueueDownload(DownloadQueueRecord),
    MarkDownloadState(DownloadStateChange),
}

pub enum DbEvent {
    ActivityRecorded { id: ActivityId },
    AlertRecorded { id: AlertId },
    SystemSnapshotSaved { id: SnapshotId },
    TorrentUpserted { id: TorrentId },
    BookUpserted { id: BookId },
    DownloadQueueUpdated { id: DownloadId },
    Failed(DbFailure),
}
```

`windlass-db` consumes `DbCommand`, runs SQLx compile-time checked Postgres
queries, and emits `DbEvent`.

The intended SQLx workflow is:

- local live checking with `DATABASE_URL` when a dev Postgres is running
- committed `.sqlx/` metadata so normal checks can run deterministically
- integration tests run migrations against real Postgres

## Migration Milestones

1. Add `windlass-machine` with generic machine/pubsub/shell primitives.
2. Add `windlass-db-core` with DB command/event and record types.
3. Add Postgres migrations and switch `windlass-db` to `PgPool`.
4. Enable SQLx compile-time checked queries and `.sqlx/` metadata.
5. Move activity log and alerts through `DbCommand`.
6. Extract `windlass-vpn-core`.
7. Extract `windlass-qbit-core`.
8. Extract `windlass-mam-core`.
9. Extract `windlass-domain-core`.
10. Remove old direct orchestration and embedded database paths.

Each milestone should leave `just check` and the relevant integration tests
green before moving to the next one.
