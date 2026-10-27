# AGENTS.md

## Project style

This is a Rust project using Functional Core / Imperative Shell.

- `core/` is pure, synchronous, and contains all business logic.
- `shell/` is async, performs I/O, and only translates between the outside world and core events/actions.
- If code makes a decision, it belongs in the core.
- If code talks to the OS, network, Docker, files, or time, it belongs in the shell.

Stateful components should follow sans-IO:

```rust
Event -> Core::handle(now, event) -> Vec<Action> -> Shell executes actions
```

The core never calls `Instant::now()`, never performs I/O, never uses `async`, and never uses `Mutex`.

## Type rules

Prefer deep, domain-specific types.

- Wrap domain values in newtypes; avoid raw primitives across boundaries.
- No bare `f64`, `u64`, `u32`, `usize`, `String`, or `bool` in domain types.
  Library types that already say what they are, such as `SystemTime`,
  `Duration`, and `IpAddr`, are fine. A `bool` is usually clearer as a
  two-variant enum.
- Give each kind of id and count its own type, so values of different things
  cannot be mixed.
- Parse, don't validate: the shell parses everything entering Windlass (MAM,
  qBittorrent, storage, forms, the command line) once, into types known to be
  valid. A value Windlass relies on that is missing or out of range fails the
  whole input; it is never replaced by a default.
- Use existing Rust/library types before creating custom ones.
- Use `nutype` for validated newtypes, and `derive_more` for `Display` and
  similar derives on other newtypes and enums.
- Use `secrecy::SecretString` for secrets.
- Make invalid states unrepresentable with enums.
- Do not use sentinel values like `0`, `-1`, or empty strings to mean “none”.
- Avoid `Deref`, `AsRef`, or `Into` on domain newtypes unless crossing an outer boundary.
- Prefer inherent methods over traits unless the trait is for:
  - standard library integration,
  - a real shell boundary,
  - or genuine open-ended polymorphism.

## Testing rules

No mocks, no adapters, no dependency injection for core logic.

- Test the core by passing inputs and asserting returned state/actions.
- External services, time, and I/O become input events, function arguments, or returned actions.
- The shell should stay logic-free; cover it with coarse integration tests when needed.

## Development workflow

Use `just`, not bare `cargo`, unless there is no recipe.

Before considering work done:

```sh
just check
just coverage
```

When shell, Docker, event-loop, or external API behavior changes, also run:

```sh
just integration
```

Clippy must pass with zero warnings. Do not suppress lints unless the suppression has an inline justification and, for global allow-list changes, explicit approval.

### Before every commit

Run `just check && just coverage`. Both must pass with zero warnings, zero test
failures, and no coverage regression.

## Codebase architecture checks

When improving existing architecture, look for:

1. Logic leaking into async shell code.
2. I/O leaking into the pure core.
3. Shallow or unvalidated types.
4. Premature single-implementer traits.
5. Fragmented or overloaded state machines.

Present candidates first. Do not jump straight to signatures or implementation.
