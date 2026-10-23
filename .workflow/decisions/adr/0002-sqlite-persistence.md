# ADR-0002: SQLite for durable application data

Status: Accepted

## Context

Windlass is moving from a container-oriented proof of concept to native services
on NixOS. The owner wants fewer dependencies and has chosen SQLite instead of
PostgreSQL. Durable pause state and acquisition history remain essential.

## Decision

Use SQLite for Windlass's durable application data. Database I/O belongs to the
imperative shell. The core requests persistence through actions and receives
success or failure as events; it never accesses a database connection.
Reads also require core actions. SQLite stores specific application records,
not a serialised snapshot of the core's in-memory state.

The core directs reads, updates, and deletions through actions naming domain
records, such as MAM configuration. Configuration can be split into separate
records rather than loaded or rewritten as one global object. The SQLite shell
owns the physical tables, SQL, and transaction execution. Results return as typed
events distinguishing absence from failure and confirmed writes from pending work.
The core groups changes that must succeed atomically into a transaction action.
The shell commits or rolls back that group and reports the result.

## Rationale

An embedded database removes the separate PostgreSQL service from the intended
deployment. Persistence still has an explicit failure boundary, allowing the
core to distinguish requested changes from confirmed durable changes.

## Scope

This selects the persistence engine, not a Rust database library, schema,
journal mode, durability configuration, or migration strategy. Design must
specify those details to meet the agreed restart and acknowledgement behaviour.
The current proof of concept does not change merely because this record exists.

## Consequences

SQLite writes must not block event handling. Storage failures must return typed
outcomes rather than silently reset control state. PostgreSQL and Docker are
not prerequisites for the new foundation's tests or deployment. Existing data
import and legacy retirement require separate scope decisions.

## Related records

- [Application foundation](../../features/B-0008/brief.md)
- [Single core](0001-single-core.md)
- [Direction](../../direction.md#local-persistence)
