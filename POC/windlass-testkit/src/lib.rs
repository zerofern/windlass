//! Library surface for `windlass-testkit`.
//!
//! The binary in `main.rs` is the operational entrypoint; this `lib.rs`
//! re-exports the fake-service modules so they can be mounted in-process
//! from integration tests (e.g. the MAM contract-drift smoke test in
//! `tests/mam_drift.rs`).

#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

pub mod mam;
