# Application foundation

Status: Ready
Feature ID: B-0008

## Goal

Establish predictable application operation and restart behaviour for Windlass's
native NixOS service. Provide a testable foundation for subsequent MAM and
qBittorrent features without PostgreSQL or Docker dependencies.

## Stories and acceptance

### S1: Start from known control state

Story: When Windlass starts, the operator can trust that saved controls are
restored before external operations become eligible.

Acceptance:

- Load configuration and durable state before enabling external work.
- Restore a saved pause without making MAM requests first.
- Missing or unreadable saved state cannot silently become an unpaused instance.
- Report startup/storage failure rather than claim successful restoration.
- Fresh setup is explicit and cannot be confused with failed restoration.

### S2: Respond while operations are outstanding

Story: While an external operation is slow, the operator can issue another command
and receive a result consistent with the application's current state.

Acceptance:

- Process accepted inputs sequentially in one consistent order.
- Outstanding I/O does not block processing subsequent inputs.
- Report operation success, failure, and cancellation distinctly.
- Late results from cancelled or superseded work cannot undo newer controls.
- Effects requiring a previous operation's completion cannot run merely because
  both were requested in the same processing step.
- Cancellation does not claim that an already-sent external request was recalled.

### S3: Run scheduled work once when eligible

Story: The system waits for a deadline and performs only the currently applicable
work, despite replacement, cancellation, or delayed delivery.

Acceptance:

- Replacing a named timer makes the previous instance ineffective.
- Cancelling a timer makes its eventual obsolete expiry ineffective.
- Duplicate expiry delivery cannot run the same scheduled work twice.
- Recheck current eligibility before acting on an expiry; an expiry alone is
  not permission for an external operation.
- Periodic work has one explicitly scheduled next occurrence.
- Timing behaviour is reproducible with supplied time and events, without sleeping.

### S4: Acknowledge durable changes truthfully

Story: The operator requests a persistent control change and knows whether it
will survive restart.

Acceptance:

- Use SQLite rather than a separate database service.
- Distinguish changes awaiting persistence from confirmed durable changes.
- Confirm durable success only after the associated save succeeds.
- A failed save returns failure and never a durable-success acknowledgement.
- Restrictive controls can take effect immediately while persistence is pending.
- Restart restores confirmed saved controls; persistence does not block unrelated
  event handling while I/O is outstanding.

## Feature-wide constraints and acceptance

- Follow the accepted [single-core operating model](../../decisions/adr/0001-single-core.md)
  and [SQLite decision](../../decisions/adr/0002-sqlite-persistence.md).
- Preserve the [VPN privacy boundary](../../decisions/pdr/0001-vpn-privacy-boundary.md).
- Run as a native systemd service on NixOS without requiring Docker or PostgreSQL.
- Keep policy in the pure synchronous core and I/O in the asynchronous shell.
- Test core decisions using concrete events, supplied time, and state/actions;
  no core mocks or dependency injection.
- Verify SQLite restart behaviour and shell timer/result delivery through focused
  integration tests. No live MAM access is required.
- Verification covers failures and ordering boundaries, not only successful paths.

## Scope

Startup, durable control-state round trips, responsive event processing,
operation completion/cancellation, timer lifecycle, and native service operation.
Detailed interfaces, clock representation, queue bounds, SQLite configuration,
and systemd unit design belong to Design.

## Non-goals

Implementing MAM initialization, token-bucket policy, acquisition, qBittorrent
integration, cleanup, notifications, or the full dashboard. Those features use
this foundation. VPN implementation and importing the proof of concept's
PostgreSQL data are excluded. Automatic legacy-code retirement is not authorised
by this brief.

## Related records

- [Direction](../../direction.md)
- [MAM initialization](../B-0004/brief.md)
- [MAM acquisition](../B-0005/brief.md)
- [qBittorrent integration](../B-0007/brief.md)

## Open questions and assumptions

The owner accepted the operating model and selected this feature for Design.
Feature-specific cooldown recovery and request retry rules remain in their
own briefs. This foundation defines reliable execution and persistence rather
than those product policies.
