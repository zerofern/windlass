#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome, Timed};
use windlass_types::VpnPort;

/// 25 GiB in bytes (binary GiB: 1024³ = 1 073 741 824).
///
/// This is the default upload-credit-buffer threshold per §26.  The binary GiB
/// choice mirrors how storage capacities are measured on the tracker and is
/// the conventionally understood meaning of "25 GB" in torrent-tracker contexts.
pub const DEFAULT_MIN_UPLOAD_BUFFER_BYTES: u64 = 25 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MamConfig {
    pub status_retry: Duration,
    /// Minimum global ratio required for non-freeleech downloads (§26).
    /// Default: `2.0`.
    pub min_global_ratio: f64,
    /// Minimum upload-credit buffer (bytes-equivalent) for all downloads (§26).
    /// Freeleech grabs also require the buffer even though they bypass the ratio
    /// (§7.4 spec: freeleech does not spend ratio, but upload health still matters).
    /// Default: 25 GiB (`DEFAULT_MIN_UPLOAD_BUFFER_BYTES`).
    pub min_upload_buffer_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamCommand {
    EnsureAuthenticated,
    EnsureSeedboxPort { port: VpnPort },
    RefreshStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamTimer {
    StatusRetry,
    RateLimitExpired,
}

// `MamEvent` cannot derive `Eq` because `StatusFetched` carries `ratio: f64`,
// and `f64` only implements `PartialEq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MamEvent {
    Init,
    AuthSucceeded,
    AuthFailed {
        reason: String,
    },
    StatusFetched {
        connectable: bool,
        seedbox_port: Option<VpnPort>,
        /// Global upload ratio from MAM (§26).  `0.0` when the field is absent
        /// (fail-closed: the upload-health gate fires on a missing ratio).
        ratio: f64,
        /// Upload-credit proxy in bytes-equivalent (§26).  `0` when absent
        /// (fail-closed).
        upload_credit_bytes: u64,
    },
    StatusFailed {
        reason: String,
    },
    SeedboxUpdated,
    SeedboxUpdateFailed {
        reason: String,
    },
    RateLimited {
        retry_after: Duration,
    },
    TimerFired(MamTimer),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamAction {
    FetchStatus,
    UpdateSeedbox,
    ScheduleTimer { timer: MamTimer, after: Duration },
}

// `MamPublish` cannot derive `Eq` because `UploadHealthDegraded` carries `f64`
// fields, and `f64` only implements `PartialEq`, not `Eq` (NaN ≠ NaN).
// The other variants are logically equatable; this is an acceptable trade-off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MamPublish {
    Ready,
    Unavailable {
        reason: String,
    },
    RateLimited {
        retry_after: Duration,
    },
    Connectable {
        seedbox_port: Option<VpnPort>,
    },
    NotConnectable {
        reason: String,
    },
    SeedboxPortReady {
        port: VpnPort,
    },
    /// Published when the upload-health gate would block a non-freeleech download
    /// (§26).  Published on every `StatusFetched` where `!upload_health_ok(false)`.
    UploadHealthDegraded {
        ratio: f64,
        upload_credit_bytes: u64,
        /// `true` iff `ratio >= config.min_global_ratio`.
        ratio_ok: bool,
        /// `true` iff `upload_credit_bytes >= config.min_upload_buffer_bytes`.
        buffer_ok: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamTopic {
    Availability,
    Connectability,
    Seedbox,
    /// Upload-health alerts (§26).
    UploadHealth,
}

impl HasTopic<MamTopic> for MamPublish {
    fn topic(&self) -> MamTopic {
        match self {
            Self::Ready | Self::Unavailable { .. } | Self::RateLimited { .. } => {
                MamTopic::Availability
            }
            Self::Connectable { .. } | Self::NotConnectable { .. } => MamTopic::Connectability,
            Self::SeedboxPortReady { .. } => MamTopic::Seedbox,
            Self::UploadHealthDegraded { .. } => MamTopic::UploadHealth,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamResponse {
    Accepted,
}

// `MamMachine` cannot derive `Eq` because `MamConfig.min_global_ratio` and the
// `ratio` field are `f64`, which only implements `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub struct MamMachine {
    config: MamConfig,
    authenticated: bool,
    seedbox_port: Option<VpnPort>,
    desired_seedbox_port: Option<VpnPort>,
    /// Last observed global upload ratio (§26).  Initialised to `0.0`
    /// (fail-closed: the gate fires until a real value is observed).
    ratio: f64,
    /// Last observed upload-credit proxy in bytes-equivalent (§26).
    /// Initialised to `0` (fail-closed).
    upload_credit_bytes: u64,
}

impl MamMachine {
    #[must_use]
    pub const fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    #[must_use]
    pub const fn seedbox_port(&self) -> Option<VpnPort> {
        self.seedbox_port
    }

    /// Returns the last observed global upload ratio (§26).
    #[must_use]
    pub const fn ratio(&self) -> f64 {
        self.ratio
    }

    /// Returns the last observed upload-credit proxy in bytes-equivalent (§26).
    #[must_use]
    pub const fn upload_credit_bytes(&self) -> u64 {
        self.upload_credit_bytes
    }

    /// Returns `true` when the upload-health gate would allow a new download.
    ///
    /// - When `freeleech == false`: both `ratio >= min_global_ratio` **and**
    ///   `upload_credit_bytes >= min_upload_buffer_bytes` must hold.
    /// - When `freeleech == true`: freeleech bypasses the ratio requirement
    ///   (§7.4 spec — freeleech does not spend ratio) but the buffer requirement
    ///   still applies.
    #[must_use]
    pub fn upload_health_ok(&self, freeleech: bool) -> bool {
        let buffer_ok = self.upload_credit_bytes >= self.config.min_upload_buffer_bytes;
        if freeleech {
            buffer_ok
        } else {
            self.ratio >= self.config.min_global_ratio && buffer_ok
        }
    }

    fn refresh_or_update_seedbox(&self) -> Vec<MamAction> {
        if self.desired_seedbox_port.is_some() {
            vec![MamAction::UpdateSeedbox]
        } else {
            vec![MamAction::FetchStatus]
        }
    }

    fn converge_seedbox(&self) -> Vec<MamAction> {
        let Some(desired) = self.desired_seedbox_port else {
            return Vec::new();
        };
        if self.seedbox_port == Some(desired) {
            Vec::new()
        } else {
            vec![MamAction::UpdateSeedbox]
        }
    }

    fn seedbox_publish(&self, seedbox_port: Option<VpnPort>) -> Vec<MamPublish> {
        seedbox_port
            .filter(|port| {
                self.desired_seedbox_port
                    .is_none_or(|desired_port| desired_port == *port)
            })
            .map(|port| MamPublish::SeedboxPortReady { port })
            .into_iter()
            .collect()
    }
}

impl Machine for MamMachine {
    type Config = MamConfig;
    type Event = MamEvent;
    type Action = MamAction;
    type Publish = MamPublish;
    type Topic = MamTopic;
    type Command = MamCommand;
    type Response = MamResponse;

    fn new(config: Self::Config, _now: Instant) -> Self {
        Self {
            config,
            authenticated: false,
            seedbox_port: None,
            desired_seedbox_port: None,
            // Start at 0.0 / 0 so the upload-health gate fires until real
            // values are observed (fail-closed per §26).
            ratio: 0.0,
            upload_credit_bytes: 0,
        }
    }

    fn handle(
        &mut self,
        _now: Instant,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event.inner {
            MamEvent::Init => Outcome {
                actions: vec![MamAction::FetchStatus],
                publish: Vec::new(),
            },
            MamEvent::TimerFired(MamTimer::StatusRetry | MamTimer::RateLimitExpired) => Outcome {
                actions: self.refresh_or_update_seedbox(),
                publish: Vec::new(),
            },
            MamEvent::AuthSucceeded => {
                self.authenticated = true;
                Outcome {
                    actions: vec![MamAction::FetchStatus],
                    publish: vec![MamPublish::Ready],
                }
            }
            MamEvent::AuthFailed { reason }
            | MamEvent::StatusFailed { reason }
            | MamEvent::SeedboxUpdateFailed { reason } => Outcome {
                actions: vec![MamAction::ScheduleTimer {
                    timer: MamTimer::StatusRetry,
                    after: self.config.status_retry,
                }],
                publish: vec![MamPublish::Unavailable { reason }],
            },
            MamEvent::StatusFetched {
                connectable,
                seedbox_port,
                ratio,
                upload_credit_bytes,
            } => {
                self.seedbox_port = seedbox_port;
                self.ratio = ratio;
                self.upload_credit_bytes = upload_credit_bytes;
                let mut publish = vec![if connectable {
                    MamPublish::Connectable { seedbox_port }
                } else {
                    MamPublish::NotConnectable {
                        reason: "MAM reports not connectable".to_string(),
                    }
                }];
                if connectable {
                    publish.extend(self.seedbox_publish(seedbox_port));
                }
                // §26: publish UploadHealthDegraded when the strictest
                // (non-freeleech) gate would block.  This is checked
                // regardless of connectability so the alert fires even
                // during a not-connectable state.
                if !self.upload_health_ok(false) {
                    let ratio_ok = ratio >= self.config.min_global_ratio;
                    let buffer_ok = upload_credit_bytes >= self.config.min_upload_buffer_bytes;
                    publish.push(MamPublish::UploadHealthDegraded {
                        ratio,
                        upload_credit_bytes,
                        ratio_ok,
                        buffer_ok,
                    });
                }
                Outcome {
                    actions: self.converge_seedbox(),
                    publish,
                }
            }
            MamEvent::SeedboxUpdated => {
                let port = self.desired_seedbox_port;
                if let Some(p) = port {
                    self.seedbox_port = Some(p);
                }
                Outcome {
                    actions: Vec::new(),
                    publish: port
                        .map(|p| MamPublish::SeedboxPortReady { port: p })
                        .into_iter()
                        .collect(),
                }
            }
            MamEvent::RateLimited { retry_after } => Outcome {
                actions: vec![MamAction::ScheduleTimer {
                    timer: MamTimer::RateLimitExpired,
                    after: retry_after,
                }],
                publish: vec![MamPublish::RateLimited { retry_after }],
            },
        }
    }

    fn handle_command(
        &mut self,
        _now: Instant,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        let actions = match cmd {
            MamCommand::EnsureAuthenticated | MamCommand::RefreshStatus => {
                vec![MamAction::FetchStatus]
            }
            MamCommand::EnsureSeedboxPort { port } => {
                self.desired_seedbox_port = Some(port);
                if self.seedbox_port == Some(port) {
                    return Self::outcome_with_publish(
                        Vec::new(),
                        vec![MamPublish::SeedboxPortReady { port }],
                        MamResponse::Accepted,
                    );
                }
                vec![MamAction::UpdateSeedbox]
            }
        };
        Self::outcome(actions, MamResponse::Accepted)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use windlass_machine::{Machine, Outcome, Timed};
    use windlass_types::VpnPort;

    use crate::{MamAction, MamCommand, MamConfig, MamEvent, MamMachine, MamPublish, MamTimer};

    fn machine() -> MamMachine {
        MamMachine::new(
            MamConfig {
                status_retry: Duration::from_secs(5),
                min_global_ratio: 2.0,
                min_upload_buffer_bytes: 25 * 1024 * 1024 * 1024,
            },
            Instant::now(),
        )
    }

    fn handle(machine: &mut MamMachine, event: MamEvent) -> Outcome<MamAction, MamPublish> {
        machine.handle(Instant::now(), Timed::now(event))
    }

    #[test]
    fn auth_success_publishes_ready_and_fetches_status() {
        let mut machine = machine();

        let out = handle(&mut machine, MamEvent::AuthSucceeded);

        assert!(machine.is_authenticated());
        assert_eq!(out.actions, vec![MamAction::FetchStatus]);
        assert_eq!(out.publish, vec![MamPublish::Ready]);
    }

    #[test]
    fn ensure_authenticated_command_fetches_status() {
        let mut machine = machine();

        let out = machine.handle_command(Instant::now(), MamCommand::EnsureAuthenticated);

        assert_eq!(out.actions, vec![MamAction::FetchStatus]);
    }

    #[test]
    fn rate_limit_schedules_expiry_timer() {
        let mut machine = machine();
        let retry_after = Duration::from_secs(30);

        let out = handle(&mut machine, MamEvent::RateLimited { retry_after });

        assert_eq!(
            out.actions,
            vec![MamAction::ScheduleTimer {
                timer: MamTimer::RateLimitExpired,
                after: retry_after,
            }]
        );
        assert_eq!(out.publish, vec![MamPublish::RateLimited { retry_after }]);
    }

    #[test]
    fn seedbox_update_publishes_ready_port() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();
        // Set a desired port so the machine knows which port was converged.
        let _ = machine.handle_command(Instant::now(), MamCommand::EnsureSeedboxPort { port });

        let out = handle(&mut machine, MamEvent::SeedboxUpdated);

        assert_eq!(machine.seedbox_port(), Some(port));
        assert_eq!(out.publish, vec![MamPublish::SeedboxPortReady { port }]);
    }

    #[test]
    fn status_mismatch_updates_desired_seedbox_without_publishing_ready() {
        let mut machine = machine();
        let desired = VpnPort::try_new(51_820).unwrap();
        let observed = VpnPort::try_new(42_000).unwrap();
        let _ = machine.handle_command(
            Instant::now(),
            MamCommand::EnsureSeedboxPort { port: desired },
        );

        let out = handle(
            &mut machine,
            MamEvent::StatusFetched {
                connectable: true,
                seedbox_port: Some(observed),
                // Healthy ratio/buffer so no UploadHealthDegraded publish.
                ratio: 3.0,
                upload_credit_bytes: 50 * 1024 * 1024 * 1024,
            },
        );

        assert_eq!(out.actions, vec![MamAction::UpdateSeedbox]);
        assert_eq!(
            out.publish,
            vec![MamPublish::Connectable {
                seedbox_port: Some(observed),
            }]
        );
    }

    #[test]
    fn seedbox_update_failure_retries_desired_port_without_ready_publish() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = machine.handle_command(Instant::now(), MamCommand::EnsureSeedboxPort { port });

        let failed = handle(
            &mut machine,
            MamEvent::SeedboxUpdateFailed {
                reason: "rate limited".to_string(),
            },
        );

        assert_eq!(
            failed.actions,
            vec![MamAction::ScheduleTimer {
                timer: MamTimer::StatusRetry,
                after: Duration::from_secs(5),
            }]
        );
        assert_eq!(
            failed.publish,
            vec![MamPublish::Unavailable {
                reason: "rate limited".to_string(),
            }]
        );

        let retry = handle(&mut machine, MamEvent::TimerFired(MamTimer::StatusRetry));

        assert_eq!(retry.actions, vec![MamAction::UpdateSeedbox]);
    }

    #[test]
    fn ensure_seedbox_port_publishes_when_already_converged() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = machine.handle_command(Instant::now(), MamCommand::EnsureSeedboxPort { port });
        let _ = handle(&mut machine, MamEvent::SeedboxUpdated);

        let out = machine.handle_command(Instant::now(), MamCommand::EnsureSeedboxPort { port });

        assert!(out.actions.is_empty());
        assert_eq!(out.publish, vec![MamPublish::SeedboxPortReady { port }]);
    }

    // ── upload_health_ok predicate tests (§26) ────────────────────────────────

    #[test]
    fn upload_health_ok_false_when_ratio_and_buffer_both_good() {
        let mut m = machine();
        m.ratio = 3.0;
        m.upload_credit_bytes = 30 * 1024 * 1024 * 1024;
        assert!(m.upload_health_ok(false));
    }

    #[test]
    fn upload_health_ok_false_when_ratio_bad() {
        let mut m = machine();
        m.ratio = 1.5;
        m.upload_credit_bytes = 30 * 1024 * 1024 * 1024;
        assert!(!m.upload_health_ok(false));
    }

    #[test]
    fn upload_health_ok_false_when_buffer_bad() {
        let mut m = machine();
        m.ratio = 3.0;
        m.upload_credit_bytes = 0;
        assert!(!m.upload_health_ok(false));
    }

    #[test]
    fn upload_health_ok_false_when_both_bad() {
        let mut m = machine();
        m.ratio = 0.5;
        m.upload_credit_bytes = 0;
        assert!(!m.upload_health_ok(false));
    }

    #[test]
    fn upload_health_ok_freeleech_true_ignores_ratio_when_buffer_ok() {
        let mut m = machine();
        m.ratio = 0.5; // below min_global_ratio
        m.upload_credit_bytes = 30 * 1024 * 1024 * 1024;
        // freeleech bypasses ratio requirement
        assert!(m.upload_health_ok(true));
    }

    #[test]
    fn upload_health_ok_freeleech_false_when_buffer_bad_even_with_good_ratio() {
        let mut m = machine();
        m.ratio = 5.0;
        m.upload_credit_bytes = 0; // below min_upload_buffer_bytes
        assert!(!m.upload_health_ok(true));
    }

    // ── StatusFetched upload-health publish tests (§26) ───────────────────────

    #[test]
    fn status_fetched_bad_ratio_emits_upload_health_degraded_with_ratio_ok_false() {
        let mut m = machine();
        let out = handle(
            &mut m,
            MamEvent::StatusFetched {
                connectable: true,
                seedbox_port: None,
                ratio: 1.5,
                upload_credit_bytes: 30 * 1024 * 1024 * 1024,
            },
        );
        let degraded = out
            .publish
            .iter()
            .find(|p| matches!(p, MamPublish::UploadHealthDegraded { .. }));
        assert!(
            degraded.is_some(),
            "must emit UploadHealthDegraded when ratio is bad"
        );
        if let Some(MamPublish::UploadHealthDegraded {
            ratio_ok,
            buffer_ok,
            ..
        }) = degraded
        {
            assert!(!ratio_ok, "ratio_ok must be false when ratio < min");
            assert!(*buffer_ok, "buffer_ok must be true when buffer >= min");
        }
    }

    #[test]
    fn status_fetched_bad_buffer_emits_upload_health_degraded_with_buffer_ok_false() {
        let mut m = machine();
        let out = handle(
            &mut m,
            MamEvent::StatusFetched {
                connectable: true,
                seedbox_port: None,
                ratio: 3.0,
                upload_credit_bytes: 0,
            },
        );
        let degraded = out
            .publish
            .iter()
            .find(|p| matches!(p, MamPublish::UploadHealthDegraded { .. }));
        assert!(
            degraded.is_some(),
            "must emit UploadHealthDegraded when buffer is bad"
        );
        if let Some(MamPublish::UploadHealthDegraded {
            ratio_ok,
            buffer_ok,
            ..
        }) = degraded
        {
            assert!(*ratio_ok, "ratio_ok must be true when ratio >= min");
            assert!(!buffer_ok, "buffer_ok must be false when buffer < min");
        }
    }

    #[test]
    fn status_fetched_good_health_emits_no_upload_health_degraded() {
        let mut m = machine();
        let out = handle(
            &mut m,
            MamEvent::StatusFetched {
                connectable: true,
                seedbox_port: None,
                ratio: 3.0,
                upload_credit_bytes: 50 * 1024 * 1024 * 1024,
            },
        );
        let degraded_count = out
            .publish
            .iter()
            .filter(|p| matches!(p, MamPublish::UploadHealthDegraded { .. }))
            .count();
        assert_eq!(
            degraded_count, 0,
            "must not emit UploadHealthDegraded when health is ok"
        );
    }

    #[test]
    fn status_fetched_both_bad_emits_one_upload_health_degraded_with_both_flags_false() {
        let mut m = machine();
        let out = handle(
            &mut m,
            MamEvent::StatusFetched {
                connectable: true,
                seedbox_port: None,
                ratio: 0.5,
                upload_credit_bytes: 0,
            },
        );
        let degraded_count = out
            .publish
            .iter()
            .filter(|p| matches!(p, MamPublish::UploadHealthDegraded { .. }))
            .count();
        assert_eq!(
            degraded_count, 1,
            "must emit exactly one UploadHealthDegraded when both bad"
        );
        if let Some(MamPublish::UploadHealthDegraded {
            ratio_ok,
            buffer_ok,
            ..
        }) = out
            .publish
            .iter()
            .find(|p| matches!(p, MamPublish::UploadHealthDegraded { .. }))
        {
            assert!(!ratio_ok, "ratio_ok must be false");
            assert!(!buffer_ok, "buffer_ok must be false");
        }
    }
}

#[cfg(test)]
mod prop_tests {
    use std::time::{Duration, Instant};

    use proptest::prelude::*;
    use windlass_machine::{Machine, Timed};
    use windlass_types::VpnPort;

    use crate::{MamAction, MamConfig, MamEvent, MamMachine, MamPublish, MamTimer};

    fn any_vpn_port() -> impl Strategy<Value = VpnPort> {
        (1u16..=u16::MAX).prop_map(|p| VpnPort::try_new(p).unwrap())
    }

    /// Ratio constrained to `0.0..=10.0` to avoid NaN/Infinity, which are
    /// pathological inputs the parse boundary already rejects.
    fn any_ratio() -> impl Strategy<Value = f64> {
        (0u32..=1000u32).prop_map(|n| f64::from(n) / 100.0)
    }

    /// Buffer constrained to `0..=(100 GiB)`.
    fn any_buffer() -> impl Strategy<Value = u64> {
        0u64..=(100 * 1024 * 1024 * 1024u64)
    }

    fn any_mam_config() -> impl Strategy<Value = MamConfig> {
        (any_ratio(), any_buffer()).prop_map(|(min_global_ratio, min_upload_buffer_bytes)| {
            MamConfig {
                status_retry: Duration::from_secs(5),
                min_global_ratio,
                min_upload_buffer_bytes,
            }
        })
    }

    // Fully-arbitrary state, including unreachable field combinations: the tested
    // invariants are total.
    fn any_mam_machine() -> impl Strategy<Value = MamMachine> {
        (
            any_mam_config(),
            any::<bool>(),
            proptest::option::of(any_vpn_port()),
            proptest::option::of(any_vpn_port()),
            any_ratio(),
            any_buffer(),
        )
            .prop_map(
                |(
                    config,
                    authenticated,
                    seedbox_port,
                    desired_seedbox_port,
                    ratio,
                    upload_credit_bytes,
                )| {
                    let mut machine = MamMachine::new(config, Instant::now());
                    machine.authenticated = authenticated;
                    machine.seedbox_port = seedbox_port;
                    machine.desired_seedbox_port = desired_seedbox_port;
                    machine.ratio = ratio;
                    machine.upload_credit_bytes = upload_credit_bytes;
                    machine
                },
            )
    }

    fn any_mam_event() -> impl Strategy<Value = MamEvent> {
        prop_oneof![
            Just(MamEvent::Init),
            Just(MamEvent::AuthSucceeded),
            any::<String>().prop_map(|reason| MamEvent::AuthFailed { reason }),
            (
                any::<bool>(),
                proptest::option::of(any_vpn_port()),
                any_ratio(),
                any_buffer(),
            )
                .prop_map(|(connectable, seedbox_port, ratio, upload_credit_bytes)| {
                    MamEvent::StatusFetched {
                        connectable,
                        seedbox_port,
                        ratio,
                        upload_credit_bytes,
                    }
                }),
            any::<String>().prop_map(|reason| MamEvent::StatusFailed { reason }),
            Just(MamEvent::SeedboxUpdated),
            any::<String>().prop_map(|reason| MamEvent::SeedboxUpdateFailed { reason }),
            (0u64..=3600).prop_map(|s| MamEvent::RateLimited {
                retry_after: Duration::from_secs(s)
            }),
            Just(MamEvent::TimerFired(MamTimer::StatusRetry)),
            Just(MamEvent::TimerFired(MamTimer::RateLimitExpired)),
        ]
    }

    proptest! {
        // GLOBAL-1 (no panic).
        #[test]
        fn handle_never_panics(mut machine in any_mam_machine(), event in any_mam_event()) {
            let _ = machine.handle(Instant::now(), Timed::now(event));
        }

        // MAM-1 (Guarantee C): every published SeedboxPortReady carries a port
        // that agrees with the desired target (or there is no desired target).
        #[test]
        fn seedbox_port_ready_matches_desired(
            mut machine in any_mam_machine(),
            event in any_mam_event(),
        ) {
            let out = machine.handle(Instant::now(), Timed::now(event));
            for publish in &out.publish {
                if let MamPublish::SeedboxPortReady { port } = publish {
                    prop_assert!(
                        machine.desired_seedbox_port.is_none()
                            || machine.desired_seedbox_port == Some(*port)
                    );
                }
            }
        }

        // MAM-2 (Guarantee F): a retryable failure schedules exactly one backed-off
        // StatusRetry and publishes Unavailable — never an immediate retry action.
        #[test]
        fn failures_schedule_one_status_retry(
            mut machine in any_mam_machine(),
            reason in any::<String>(),
        ) {
            for event in [
                MamEvent::AuthFailed { reason: reason.clone() },
                MamEvent::StatusFailed { reason: reason.clone() },
                MamEvent::SeedboxUpdateFailed { reason },
            ] {
                let out = machine.handle(Instant::now(), Timed::now(event));
                prop_assert_eq!(out.actions.len(), 1);
                let is_status_retry = matches!(
                    out.actions[0],
                    MamAction::ScheduleTimer { timer: MamTimer::StatusRetry, .. }
                );
                prop_assert!(is_status_retry);
                prop_assert_eq!(out.publish.len(), 1);
                let is_unavailable = matches!(out.publish[0], MamPublish::Unavailable { .. });
                prop_assert!(is_unavailable);
            }
        }

        // MAM-7 [safety] (upload-health alert — §26):
        // `StatusFetched` publishes `UploadHealthDegraded` iff
        // `!upload_health_ok(freeleech=false)`.  The published `ratio_ok` and
        // `buffer_ok` flags are consistent with the configured thresholds.
        // Total invariant.
        #[test]
        fn upload_health_degraded_iff_not_upload_health_ok(
            mut machine in any_mam_machine(),
            connectable in any::<bool>(),
            seedbox_port in proptest::option::of(any_vpn_port()),
            ratio in any_ratio(),
            upload_credit_bytes in any_buffer(),
        ) {
            let out = machine.handle(
                Instant::now(),
                Timed::now(MamEvent::StatusFetched {
                    connectable,
                    seedbox_port,
                    ratio,
                    upload_credit_bytes,
                }),
            );

            // After handle, self.ratio and self.upload_credit_bytes are updated.
            let expected_health_ok = machine.upload_health_ok(false);
            let degraded_publishes: Vec<_> = out
                .publish
                .iter()
                .filter(|p| matches!(p, MamPublish::UploadHealthDegraded { .. }))
                .collect();

            if expected_health_ok {
                prop_assert!(
                    degraded_publishes.is_empty(),
                    "upload_health_ok(false)=true must produce no UploadHealthDegraded"
                );
            } else {
                prop_assert_eq!(
                    degraded_publishes.len(),
                    1,
                    "upload_health_ok(false)=false must produce exactly one UploadHealthDegraded"
                );
                // Check flag consistency.
                if let MamPublish::UploadHealthDegraded {
                    ratio: r,
                    upload_credit_bytes: b,
                    ratio_ok,
                    buffer_ok,
                } = degraded_publishes[0]
                {
                    prop_assert_eq!(
                        *ratio_ok,
                        *r >= machine.config.min_global_ratio,
                        "ratio_ok must be consistent with the threshold"
                    );
                    prop_assert_eq!(
                        *buffer_ok,
                        *b >= machine.config.min_upload_buffer_bytes,
                        "buffer_ok must be consistent with the threshold"
                    );
                }
            }
        }
    }
}
