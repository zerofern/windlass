//! Disk-pressure machine (`DiskMachine`).
//!
//! Observes free disk space and publishes whether the system is below or above
//! the configured hard floor (`DiskConfig::hard_floor_bytes`).
//!
//! The actual disk read is deferred to the shell (story 32 cutover); this core
//! only holds the decision logic.  The shell feeds `DiskEvent::DiskSpaceObserved`
//! and the core publishes `DiskPublish::BelowFloor` or `DiskPublish::AboveFloor`.
//!
//! # Deferred shell wiring
//!
//! No shell action for reading disk space is wired up yet.  Story 32 will add
//! the live disk-read loop.  For now, the core compiles and all tests exercise it
//! via direct event injection.
//!
//! # Rank classes (deferred)
//!
//! The four real deletion-value rank classes — (1) completed + low rating (≤2★),
//! (2) DNF, (3) completed + high rating but long since listened, (4) unstarted +
//! low AI score — require librarian data outside operator scope.  They are
//! documented here as the eventual target but not implemented.  The current
//! placeholder rank (longest `seed_time` first among HnR-satisfied torrents) is
//! implemented in `windlass-qbit-core::QbitCommand::EvictOneForDiskPressure` and
//! holds the spot until librarian integration lands.
#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::time::Instant;

use serde::{Deserialize, Serialize};
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome, Timed};

/// Configuration for the disk-pressure machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskConfig {
    /// Free-space threshold in bytes.  When `free_bytes < hard_floor_bytes` the
    /// machine publishes `BelowFloor`; at or above, it publishes `AboveFloor`.
    pub hard_floor_bytes: u64,
}

/// Commands accepted by `DiskMachine`.
///
/// `RefreshDisk` is included for symmetry with the other service cores; it is
/// optional and the shell may omit it until story 32.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskCommand {
    /// Request a fresh disk-space observation.
    RefreshDisk,
}

/// Events produced by the disk shell and consumed by `DiskMachine`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskEvent {
    /// Emitted once when the machine is first created, triggering any init work.
    Init,
    /// A fresh disk-space reading from the shell.
    DiskSpaceObserved {
        /// Current free bytes on the monitored volume.
        free_bytes: u64,
    },
}

/// Actions that `DiskMachine` asks the shell to perform.
///
/// Currently empty: the machine has no immediate I/O need of its own.
/// Story 32 may add a `ReadDiskSpace` action when the live poll loop is wired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskAction {}

/// Facts published by `DiskMachine` to topic subscribers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskPublish {
    /// Free space dropped below the configured hard floor.
    /// `free_bytes < config.hard_floor_bytes` — DISK-1.
    BelowFloor { free_bytes: u64 },
    /// Free space is at or above the hard floor.
    /// `free_bytes >= config.hard_floor_bytes` — DISK-1.
    AboveFloor { free_bytes: u64 },
}

/// Topics for `DiskPublish` routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskTopic {
    /// Disk-pressure signals (`BelowFloor`, `AboveFloor`).
    Pressure,
}

impl HasTopic<DiskTopic> for DiskPublish {
    fn topic(&self) -> DiskTopic {
        match self {
            Self::BelowFloor { .. } | Self::AboveFloor { .. } => DiskTopic::Pressure,
        }
    }
}

/// Response returned to callers of `DiskMachine::handle_command`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskResponse {
    Accepted,
}

/// Disk-pressure sans-I/O machine.
///
/// Tracks the last observed free-space value and publishes `BelowFloor` /
/// `AboveFloor` on every observation according to DISK-1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiskMachine {
    config: DiskConfig,
    /// Last known free-space reading, or `None` before the first observation.
    free_bytes: Option<u64>,
}

impl DiskMachine {
    /// Returns the last observed free-byte count, or `None` if no observation
    /// has arrived yet.
    #[must_use]
    pub const fn free_bytes(&self) -> Option<u64> {
        self.free_bytes
    }

    /// Core decision — DISK-1: `BelowFloor` iff `free < floor`.
    const fn pressure_publish(&self, free_bytes: u64) -> DiskPublish {
        if free_bytes < self.config.hard_floor_bytes {
            DiskPublish::BelowFloor { free_bytes }
        } else {
            DiskPublish::AboveFloor { free_bytes }
        }
    }
}

impl Machine for DiskMachine {
    type Config = DiskConfig;
    type Event = DiskEvent;
    type Action = DiskAction;
    type Publish = DiskPublish;
    type Topic = DiskTopic;
    type Command = DiskCommand;
    type Response = DiskResponse;
    type StateSnapshot = Self;

    fn new(config: Self::Config, _now: Instant) -> Self {
        Self {
            config,
            free_bytes: None,
        }
    }

    fn handle(
        &mut self,
        _now: Instant,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event.inner {
            DiskEvent::Init => Outcome {
                actions: Vec::new(),
                publishes: Vec::new(),
            },
            DiskEvent::DiskSpaceObserved { free_bytes } => {
                self.free_bytes = Some(free_bytes);
                Outcome {
                    actions: Vec::new(),
                    publishes: vec![self.pressure_publish(free_bytes)],
                }
            }
        }
    }

    fn handle_command(
        &mut self,
        _now: Instant,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        match cmd {
            DiskCommand::RefreshDisk => Self::outcome(Vec::new(), DiskResponse::Accepted),
        }
    }

    fn state_snapshot(&self) -> Self::StateSnapshot {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use windlass_machine::{ExternalCause, Machine, Timed};

    use crate::{DiskConfig, DiskEvent, DiskMachine, DiskPublish};

    fn machine(hard_floor_bytes: u64) -> DiskMachine {
        DiskMachine::new(DiskConfig { hard_floor_bytes }, Instant::now())
    }

    fn observe(m: &mut DiskMachine, free_bytes: u64) -> Vec<DiskPublish> {
        m.handle(
            Instant::now(),
            Timed::external(
                Instant::now(),
                ExternalCause::Unknown,
                DiskEvent::DiskSpaceObserved { free_bytes },
            ),
        )
        .publishes
    }

    // ── DISK-1 unit tests ────────────────────────────────────────────────────

    #[test]
    fn below_floor_publishes_below_floor() {
        let mut m = machine(1_000_000);
        let publishes = observe(&mut m, 999_999);
        assert_eq!(
            publishes,
            vec![DiskPublish::BelowFloor {
                free_bytes: 999_999
            }]
        );
    }

    #[test]
    fn at_floor_publishes_above_floor() {
        // `free == floor` is NOT below floor — DISK-1 threshold edge.
        let mut m = machine(1_000_000);
        let publishes = observe(&mut m, 1_000_000);
        assert_eq!(
            publishes,
            vec![DiskPublish::AboveFloor {
                free_bytes: 1_000_000
            }]
        );
    }

    #[test]
    fn above_floor_publishes_above_floor() {
        let mut m = machine(1_000_000);
        let publishes = observe(&mut m, 2_000_000);
        assert_eq!(
            publishes,
            vec![DiskPublish::AboveFloor {
                free_bytes: 2_000_000
            }]
        );
    }

    #[test]
    fn init_produces_no_publish() {
        let mut m = machine(1_000_000);
        let out = m.handle(
            Instant::now(),
            Timed::external(Instant::now(), ExternalCause::Unknown, DiskEvent::Init),
        );
        assert!(out.publishes.is_empty());
        assert!(out.actions.is_empty());
    }

    #[test]
    fn free_bytes_updated_after_observation() {
        let mut m = machine(1_000_000);
        assert_eq!(m.free_bytes(), None);
        let _ = observe(&mut m, 500_000);
        assert_eq!(m.free_bytes(), Some(500_000));
    }

    #[test]
    fn state_snapshot_reflects_last_observation() {
        // §37b: after a fresh observation the snapshot's free_bytes
        // matches what we just fed in.
        let mut m = machine(1_000_000);
        let _ = observe(&mut m, 750_000);
        let value = serde_json::to_value(m.state_snapshot()).expect("snapshot should serialize");
        assert_eq!(value["free_bytes"], 750_000);
        assert_eq!(value["config"]["hard_floor_bytes"], 1_000_000);
    }
}

#[cfg(test)]
mod prop_tests {
    use std::time::Instant;

    use proptest::prelude::*;
    use windlass_machine::{ExternalCause, Machine, Timed};

    use crate::{DiskConfig, DiskEvent, DiskMachine, DiskPublish};

    fn any_disk_machine() -> impl Strategy<Value = DiskMachine> {
        (any::<u64>(), proptest::option::of(any::<u64>())).prop_map(
            |(hard_floor_bytes, free_bytes)| {
                let mut machine = DiskMachine::new(DiskConfig { hard_floor_bytes }, Instant::now());
                machine.free_bytes = free_bytes;
                machine
            },
        )
    }

    fn any_disk_event() -> impl Strategy<Value = DiskEvent> {
        prop_oneof![
            Just(DiskEvent::Init),
            any::<u64>().prop_map(|free_bytes| DiskEvent::DiskSpaceObserved { free_bytes }),
        ]
    }

    proptest! {
        // GLOBAL-1 (no panic).
        #[test]
        fn handle_never_panics(mut machine in any_disk_machine(), event in any_disk_event()) {
            let _ = machine.handle(Instant::now(), Timed::external(Instant::now(), ExternalCause::Unknown, event));
        }

        // DISK-1 [safety] (Guarantee A): BelowFloor is published iff
        // free_bytes < hard_floor_bytes; otherwise AboveFloor.
        // Total invariant — holds for any machine state and any observation.
        #[test]
        fn disk_floor_invariant(
            mut machine in any_disk_machine(),
            free_bytes in any::<u64>(),
        ) {
            let out = machine.handle(
                Instant::now(),
                Timed::external(Instant::now(), ExternalCause::Unknown, DiskEvent::DiskSpaceObserved { free_bytes }),
            );
            // Exactly one publish must be emitted.
            prop_assert_eq!(out.publishes.len(), 1);
            match out.publishes[0] {
                DiskPublish::BelowFloor { free_bytes: fb } => {
                    prop_assert_eq!(fb, free_bytes);
                    prop_assert!(
                        fb < machine.config.hard_floor_bytes,
                        "BelowFloor published but free_bytes ({fb}) >= hard_floor_bytes ({})",
                        machine.config.hard_floor_bytes
                    );
                }
                DiskPublish::AboveFloor { free_bytes: fb } => {
                    prop_assert_eq!(fb, free_bytes);
                    prop_assert!(
                        fb >= machine.config.hard_floor_bytes,
                        "AboveFloor published but free_bytes ({fb}) < hard_floor_bytes ({})",
                        machine.config.hard_floor_bytes
                    );
                }
            }
        }

        // DISK-1 corollary: no actions are ever emitted.
        #[test]
        fn no_actions_ever_emitted(
            mut machine in any_disk_machine(),
            event in any_disk_event(),
        ) {
            let out = machine.handle(Instant::now(), Timed::external(Instant::now(), ExternalCause::Unknown, event));
            prop_assert!(out.actions.is_empty());
        }
    }
}
