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

No new Rust workspace exists at the repository root yet. The old build and test
recipes remain in `POC/justfile`; run them from `POC/`. They still depend on the
old Docker/PostgreSQL setup. New build and test tooling belongs to the application
foundation work.

Official MAM documentation remains in `docs/MAM/` as locally supplied references.
Windlass-authored API flows live in Feature Briefs under `.workflow/features/`.
