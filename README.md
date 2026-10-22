# Windlass

Windlass is being redesigned around one pure Rust core, an asynchronous shell,
SQLite persistence, and native systemd services on NixOS.

- [Direction](.workflow/direction.md) describes the intended product.
- [Backlog](.workflow/backlog.md) retains future work.
- [Application foundation](.workflow/features/B-0008/brief.md) is ready for Design.
- [Feature Briefs](.workflow/features/) contain the agreed and proposed flows.
- [Project instructions](AGENTS.md) define coding and testing conventions.

## Proof of concept

The previous implementation and its documentation are archived in [POC](POC/README.md).
That directory contains the old Rust workspace, frontend, Docker setup, database
metadata, tests, research material, and local build artifacts. It is reference
material, not the specification for the new implementation.

The root Rust package contains a small executable core/shell slice. Run
`just check` for compilation, formatting, strict Clippy, tests, and documentation
checks. Run `just coverage` for LLVM coverage and `just integration` for real
SQLite integration tests. The foundation feature is still incomplete.

## Inspect the core and shell

```sh
just run initialise /tmp/windlass.sqlite
```

The console accepts `pause`, `enable`, `forget`, `status`, and `quit`. Startup
and every storage operation are driven by core actions. `forget` deletes the
saved MAM request preference and applies the paused default. It does not delete
books or download history. No network requests are implemented.

To restore the same instance:

```sh
just run resume /tmp/windlass.sqlite
```

Initialisation refuses to overwrite an existing file; resume refuses to create
missing storage. The console shows the actual core state, including startup
phase, saved, requested, and effective preferences, and the exact pending update.
`Core::state()` returns a read-only reference to that state. Enabling waits for
a successful save, while pause applies immediately. Outstanding writes finish
before normal console shutdown. Failed startup or a failed final preference save
produces a nonzero exit status.

Slow console output keeps the latest status and storage error while the core
continues processing commands and storage results. Intermediate status updates
can be replaced. Shutdown waits for the remaining console output to drain.

The [runner](src/runner.rs) owns the [core](src/core/machine.rs), consumes one
timestamped event stream, and dispatches actions through the [shell](src/shell/mod.rs).
The shell executes effects and returns results through that same stream. The
[console](src/shell/console.rs) supplies requests and observes core state. The
[SQLite boundary](src/shell/storage.rs) executes typed storage operations on
blocking workers, outside the event loop. There are no operation identifiers,
generic machine traits, or database connection pools.

`Runner::new` returns the runner, an `EventSender`, and an observation subscription.
Clone the sender for another input source, such as the planned HTTP API. Producers
record occurrence time before enqueueing; `send_timed` preserves a timestamp
already recorded by its producer. The queue holds 64 messages and applies
backpressure. Events follow queue order even when their timestamps differ.
Observers receive the actual core state, the last event, and the latest storage
error. Slow observers retain the newest observation without blocking the runner.

`EventSender::shutdown` rejects new submissions and drains accepted inputs and
all resulting storage work. Sender clones can remain alive during shutdown.
HTTP routes and the webpage remain to be implemented against this ingress.

Timers, task cancellation, shutdown signals, deployment units, and actual MAM
and qBittorrent clients remain to be built. This console is for reviewing the
operating model; it does not yet implement the full service.

## Development

Clippy enables `all`, `pedantic`, `nursery`, and selected restriction lints.
Warnings fail the build; unsafe code is forbidden. No lint suppressions are
configured. Lint settings live in `Cargo.toml`.

Development requires Rust with rustfmt, Clippy, and llvm-tools, plus just,
cargo-llvm-cov, Python, ripgrep, and prose-check. The workflow checker defaults to
the installed workflow-document-check skill under `~/.agents/skills/`; set
`WORKFLOW_DOC_CHECKER` to use another installation.

The archived build and test recipes remain in `POC/justfile`; run those from
`POC/`. They still depend on the old Docker/PostgreSQL setup.

Official MAM documentation remains in `docs/MAM/` as locally supplied references.
Windlass-authored API flows live in Feature Briefs under `.workflow/features/`.
