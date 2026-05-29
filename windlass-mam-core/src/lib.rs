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

/// Default keep-alive interval (§27).  Matches Mousehole's default check
/// cadence (5 minutes / 300 seconds).
pub const DEFAULT_KEEP_ALIVE_INTERVAL: Duration = Duration::from_mins(5);

/// Default consecutive-failure threshold for `KeepAliveDegraded` (§27).
pub const DEFAULT_KEEP_ALIVE_FAILURE_THRESHOLD: u32 = 3;

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
    /// Recurring `FetchStatus` cadence that keeps the MAM account alive
    /// (§27, MAM Rule 1.6).  Default: 300 s
    /// (`DEFAULT_KEEP_ALIVE_INTERVAL`, matches Mousehole).
    pub keep_alive_interval: Duration,
    /// Consecutive retryable failures required to publish
    /// `KeepAliveDegraded` (§27).  `0` disables the alert.
    /// Default: `3` (`DEFAULT_KEEP_ALIVE_FAILURE_THRESHOLD`).
    pub keep_alive_failure_threshold: u32,
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
    /// §27: self-perpetuating heartbeat that drives recurring `FetchStatus`.
    KeepAlive,
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
    /// §28: MAM could not be reached at all (DNS/TCP/TLS/timeout).  Distinct
    /// from `StatusFailed` (MAM responded but the response was wrong) and
    /// from `StatusFetched { connectable: false }` (MAM responded and
    /// reports we are unconnectable).  Routed by the shell from
    /// `MamFetchError::Unreachable` or `Event::MamUnreachable`.
    Unreachable {
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
    /// §28: MAM could not be reached at all.  Distinct from `NotConnectable`,
    /// which means MAM responded and reports our client is not connectable.
    /// `Unreachable` is transient; the operator alert path lives in the
    /// keep-alive degraded publish (§27) rather than a Critical/Warning here.
    Unreachable {
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
    /// §27: published exactly once on the rising edge when
    /// `consecutive_status_failures` crosses `keep_alive_failure_threshold`.
    /// Carries the last retryable-failure reason for the alert body.
    KeepAliveDegraded {
        consecutive_failures: u32,
        last_reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MamTopic {
    Availability,
    Connectability,
    Seedbox,
    /// Upload-health alerts (§26).
    UploadHealth,
    /// Keep-alive heartbeat degradation alerts (§27).
    KeepAlive,
}

impl HasTopic<MamTopic> for MamPublish {
    fn topic(&self) -> MamTopic {
        match self {
            Self::Ready | Self::Unavailable { .. } | Self::RateLimited { .. } => {
                MamTopic::Availability
            }
            Self::Connectable { .. } | Self::NotConnectable { .. } | Self::Unreachable { .. } => {
                MamTopic::Connectability
            }
            Self::SeedboxPortReady { .. } => MamTopic::Seedbox,
            Self::UploadHealthDegraded { .. } => MamTopic::UploadHealth,
            Self::KeepAliveDegraded { .. } => MamTopic::KeepAlive,
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
    /// §27: `true` once the `KeepAlive` self-perpetuating chain has been
    /// started.  Guards against a duplicate chain being launched by a second
    /// `AuthSucceeded` (MAM-8).
    keep_alive_scheduled: bool,
    /// §27: consecutive retryable-failure count.  Incremented by every
    /// `AuthFailed`/`StatusFailed`/`SeedboxUpdateFailed`; reset by
    /// `StatusFetched`.
    consecutive_status_failures: u32,
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

    /// Returns the current consecutive retryable-failure count (§27).
    #[must_use]
    pub const fn consecutive_status_failures(&self) -> u32 {
        self.consecutive_status_failures
    }

    /// Returns `true` once the `KeepAlive` chain has been started (§27).
    #[must_use]
    pub const fn keep_alive_scheduled(&self) -> bool {
        self.keep_alive_scheduled
    }

    /// Increments the consecutive-failure count, returning `true` iff this
    /// bump crossed `keep_alive_failure_threshold` from below — the
    /// rising-edge predicate behind MAM-10.  When the threshold is `0` the
    /// gate is disabled and this always returns `false`.
    const fn bump_keep_alive_failures(&mut self) -> bool {
        let threshold = self.config.keep_alive_failure_threshold;
        if threshold == 0 {
            self.consecutive_status_failures = self.consecutive_status_failures.saturating_add(1);
            return false;
        }
        let before = self.consecutive_status_failures;
        let after = before.saturating_add(1);
        self.consecutive_status_failures = after;
        before < threshold && after >= threshold
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
            keep_alive_scheduled: false,
            consecutive_status_failures: 0,
        }
    }

    // Each event arm is a small, self-contained decision; the function is long
    // because the event set is large, not because any single arm is complex.
    #[allow(clippy::too_many_lines)]
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
            // §27: the keep-alive timer always re-schedules itself before
            // emitting the FetchStatus action, so a dropped result or shell
            // error cannot kill the chain (MAM-9).
            MamEvent::TimerFired(MamTimer::KeepAlive) => Outcome {
                actions: vec![
                    MamAction::FetchStatus,
                    MamAction::ScheduleTimer {
                        timer: MamTimer::KeepAlive,
                        after: self.config.keep_alive_interval,
                    },
                ],
                publish: Vec::new(),
            },
            MamEvent::AuthSucceeded => {
                self.authenticated = true;
                let mut actions = vec![MamAction::FetchStatus];
                // §27 / MAM-8: start the keep-alive chain at most once per
                // machine lifetime, on the first AuthSucceeded.
                if !self.keep_alive_scheduled {
                    self.keep_alive_scheduled = true;
                    actions.push(MamAction::ScheduleTimer {
                        timer: MamTimer::KeepAlive,
                        after: self.config.keep_alive_interval,
                    });
                }
                Outcome {
                    actions,
                    publish: vec![MamPublish::Ready],
                }
            }
            MamEvent::AuthFailed { reason }
            | MamEvent::StatusFailed { reason }
            | MamEvent::SeedboxUpdateFailed { reason } => {
                let mut publish = vec![MamPublish::Unavailable {
                    reason: reason.clone(),
                }];
                // §27 / MAM-10: increment the consecutive-failure count, and
                // publish KeepAliveDegraded exactly once on the rising edge
                // when the count crosses the configured threshold.
                let crossed = self.bump_keep_alive_failures();
                if crossed {
                    publish.push(MamPublish::KeepAliveDegraded {
                        consecutive_failures: self.consecutive_status_failures,
                        last_reason: reason,
                    });
                }
                Outcome {
                    actions: vec![MamAction::ScheduleTimer {
                        timer: MamTimer::StatusRetry,
                        after: self.config.status_retry,
                    }],
                    publish,
                }
            }
            // §28 / MAM-11: a transport-level failure publishes
            // `Unreachable` on the Connectability topic — distinct from
            // `Unavailable` (which means "MAM responded but is broken for me
            // right now") and from `NotConnectable` (which means "MAM
            // responded and reports my client is unreachable from their
            // side").  Same StatusRetry + keep-alive-counter handling as the
            // other retryable failures.
            MamEvent::Unreachable { reason } => {
                let mut publish = vec![MamPublish::Unreachable {
                    reason: reason.clone(),
                }];
                let crossed = self.bump_keep_alive_failures();
                if crossed {
                    publish.push(MamPublish::KeepAliveDegraded {
                        consecutive_failures: self.consecutive_status_failures,
                        last_reason: reason,
                    });
                }
                Outcome {
                    actions: vec![MamAction::ScheduleTimer {
                        timer: MamTimer::StatusRetry,
                        after: self.config.status_retry,
                    }],
                    publish,
                }
            }
            MamEvent::StatusFetched {
                connectable,
                seedbox_port,
                ratio,
                upload_credit_bytes,
            } => {
                self.seedbox_port = seedbox_port;
                self.ratio = ratio;
                self.upload_credit_bytes = upload_credit_bytes;
                // §27: a successful status read resets the consecutive-
                // failure count.  After a future failure burst, the rising
                // edge over the threshold republishes KeepAliveDegraded.
                self.consecutive_status_failures = 0;
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
                keep_alive_interval: Duration::from_secs(300),
                keep_alive_failure_threshold: 3,
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
        // §27: AuthSucceeded triggers a status fetch *and* arms the
        // self-perpetuating KeepAlive timer.
        assert_eq!(
            out.actions,
            vec![
                MamAction::FetchStatus,
                MamAction::ScheduleTimer {
                    timer: MamTimer::KeepAlive,
                    after: Duration::from_secs(300),
                },
            ]
        );
        assert_eq!(out.publish, vec![MamPublish::Ready]);
        assert!(machine.keep_alive_scheduled());
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

    // ── KeepAlive heartbeat tests (§27) ───────────────────────────────────────

    #[test]
    fn keep_alive_timer_emits_fetch_and_reschedules() {
        let mut machine = machine();
        // Arm the chain via AuthSucceeded; consume its actions.
        let _ = handle(&mut machine, MamEvent::AuthSucceeded);

        let out = handle(&mut machine, MamEvent::TimerFired(MamTimer::KeepAlive));

        assert_eq!(
            out.actions,
            vec![
                MamAction::FetchStatus,
                MamAction::ScheduleTimer {
                    timer: MamTimer::KeepAlive,
                    after: Duration::from_secs(300),
                },
            ]
        );
        assert!(out.publish.is_empty());
    }

    #[test]
    fn second_auth_success_does_not_arm_second_keep_alive_chain() {
        let mut machine = machine();

        let first = handle(&mut machine, MamEvent::AuthSucceeded);
        assert_eq!(
            first
                .actions
                .iter()
                .filter(|a| matches!(
                    a,
                    MamAction::ScheduleTimer {
                        timer: MamTimer::KeepAlive,
                        ..
                    }
                ))
                .count(),
            1,
            "first AuthSucceeded must arm KeepAlive"
        );

        let second = handle(&mut machine, MamEvent::AuthSucceeded);
        assert_eq!(
            second
                .actions
                .iter()
                .filter(|a| matches!(
                    a,
                    MamAction::ScheduleTimer {
                        timer: MamTimer::KeepAlive,
                        ..
                    }
                ))
                .count(),
            0,
            "second AuthSucceeded must NOT arm a second KeepAlive chain"
        );
    }

    #[test]
    fn keep_alive_degraded_fires_on_third_consecutive_failure_only() {
        let mut machine = machine();

        let first = handle(
            &mut machine,
            MamEvent::StatusFailed {
                reason: "boom".to_string(),
            },
        );
        let second = handle(
            &mut machine,
            MamEvent::StatusFailed {
                reason: "boom".to_string(),
            },
        );
        let third = handle(
            &mut machine,
            MamEvent::StatusFailed {
                reason: "third".to_string(),
            },
        );
        let fourth = handle(
            &mut machine,
            MamEvent::StatusFailed {
                reason: "fourth".to_string(),
            },
        );

        let degraded_count = |out: &Outcome<MamAction, MamPublish>| {
            out.publish
                .iter()
                .filter(|p| matches!(p, MamPublish::KeepAliveDegraded { .. }))
                .count()
        };

        assert_eq!(degraded_count(&first), 0);
        assert_eq!(degraded_count(&second), 0);
        assert_eq!(
            degraded_count(&third),
            1,
            "rising edge fires exactly once on threshold-crossing failure"
        );
        assert_eq!(
            degraded_count(&fourth),
            0,
            "no re-publish while still over threshold"
        );

        if let Some(MamPublish::KeepAliveDegraded {
            consecutive_failures,
            last_reason,
        }) = third
            .publish
            .iter()
            .find(|p| matches!(p, MamPublish::KeepAliveDegraded { .. }))
        {
            assert_eq!(*consecutive_failures, 3);
            assert_eq!(last_reason, "third");
        } else {
            panic!("third failure must publish KeepAliveDegraded");
        }
    }

    #[test]
    fn status_fetched_resets_failure_counter_and_rearms_rising_edge() {
        let mut machine = machine();
        for _ in 0..3 {
            let _ = handle(
                &mut machine,
                MamEvent::StatusFailed {
                    reason: "x".to_string(),
                },
            );
        }
        assert!(machine.consecutive_status_failures() >= 3);

        let _ = handle(
            &mut machine,
            MamEvent::StatusFetched {
                connectable: true,
                seedbox_port: None,
                ratio: 3.0,
                upload_credit_bytes: 50 * 1024 * 1024 * 1024,
            },
        );
        assert_eq!(machine.consecutive_status_failures(), 0);

        // Burn down to the threshold again; rising edge must fire a second time.
        let _ = handle(
            &mut machine,
            MamEvent::StatusFailed {
                reason: "y".to_string(),
            },
        );
        let _ = handle(
            &mut machine,
            MamEvent::StatusFailed {
                reason: "y".to_string(),
            },
        );
        let third = handle(
            &mut machine,
            MamEvent::StatusFailed {
                reason: "y".to_string(),
            },
        );
        let degraded_count = third
            .publish
            .iter()
            .filter(|p| matches!(p, MamPublish::KeepAliveDegraded { .. }))
            .count();
        assert_eq!(
            degraded_count, 1,
            "rising edge must fire again after a reset"
        );
    }

    #[test]
    fn all_three_failure_kinds_count_toward_keep_alive_threshold() {
        let mut machine = machine();
        let _ = handle(
            &mut machine,
            MamEvent::AuthFailed {
                reason: "auth".to_string(),
            },
        );
        let _ = handle(
            &mut machine,
            MamEvent::SeedboxUpdateFailed {
                reason: "seedbox".to_string(),
            },
        );
        let third = handle(
            &mut machine,
            MamEvent::StatusFailed {
                reason: "status".to_string(),
            },
        );

        let degraded = third
            .publish
            .iter()
            .find(|p| matches!(p, MamPublish::KeepAliveDegraded { .. }));
        assert!(
            degraded.is_some(),
            "mixed failures must accumulate toward threshold"
        );
        if let Some(MamPublish::KeepAliveDegraded {
            consecutive_failures,
            last_reason,
        }) = degraded
        {
            assert_eq!(*consecutive_failures, 3);
            assert_eq!(last_reason, "status");
        }
    }

    #[test]
    fn keep_alive_threshold_zero_disables_degraded_publish() {
        let mut machine = MamMachine::new(
            MamConfig {
                status_retry: Duration::from_secs(5),
                min_global_ratio: 2.0,
                min_upload_buffer_bytes: 25 * 1024 * 1024 * 1024,
                keep_alive_interval: Duration::from_secs(300),
                keep_alive_failure_threshold: 0,
            },
            Instant::now(),
        );
        for _ in 0..10 {
            let out = handle(
                &mut machine,
                MamEvent::StatusFailed {
                    reason: "x".to_string(),
                },
            );
            assert!(
                !out.publish
                    .iter()
                    .any(|p| matches!(p, MamPublish::KeepAliveDegraded { .. })),
                "threshold=0 must never publish KeepAliveDegraded"
            );
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
        (
            any_ratio(),
            any_buffer(),
            // 1..=600s keep-alive cadence covers the realistic range without
            // making the timer constant explode in failure-burst proptests.
            1u64..=600u64,
            0u32..=10u32,
        )
            .prop_map(
                |(
                    min_global_ratio,
                    min_upload_buffer_bytes,
                    keep_alive_secs,
                    keep_alive_failure_threshold,
                )| MamConfig {
                    status_retry: Duration::from_secs(5),
                    min_global_ratio,
                    min_upload_buffer_bytes,
                    keep_alive_interval: Duration::from_secs(keep_alive_secs),
                    keep_alive_failure_threshold,
                },
            )
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
            any::<bool>(),
            0u32..=20u32,
        )
            .prop_map(
                |(
                    config,
                    authenticated,
                    seedbox_port,
                    desired_seedbox_port,
                    ratio,
                    upload_credit_bytes,
                    keep_alive_scheduled,
                    consecutive_status_failures,
                )| {
                    let mut machine = MamMachine::new(config, Instant::now());
                    machine.authenticated = authenticated;
                    machine.seedbox_port = seedbox_port;
                    machine.desired_seedbox_port = desired_seedbox_port;
                    machine.ratio = ratio;
                    machine.upload_credit_bytes = upload_credit_bytes;
                    machine.keep_alive_scheduled = keep_alive_scheduled;
                    machine.consecutive_status_failures = consecutive_status_failures;
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
            any::<String>().prop_map(|reason| MamEvent::Unreachable { reason }),
            Just(MamEvent::SeedboxUpdated),
            any::<String>().prop_map(|reason| MamEvent::SeedboxUpdateFailed { reason }),
            (0u64..=3600).prop_map(|s| MamEvent::RateLimited {
                retry_after: Duration::from_secs(s)
            }),
            Just(MamEvent::TimerFired(MamTimer::StatusRetry)),
            Just(MamEvent::TimerFired(MamTimer::RateLimitExpired)),
            Just(MamEvent::TimerFired(MamTimer::KeepAlive)),
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
        // StatusRetry and publishes its kind-specific publish — never an
        // immediate retry action.  §27 adds: failures may also publish
        // KeepAliveDegraded on the rising edge.  §28 generalises this from
        // "always Unavailable" to "Unavailable for Auth/Status/Seedbox
        // failures, Unreachable for transport-level Unreachable".
        #[test]
        fn failures_schedule_one_status_retry(
            mut machine in any_mam_machine(),
            reason in any::<String>(),
        ) {
            // Auth/Status/Seedbox failures publish Unavailable.
            for event in [
                MamEvent::AuthFailed { reason: reason.clone() },
                MamEvent::StatusFailed { reason: reason.clone() },
                MamEvent::SeedboxUpdateFailed { reason: reason.clone() },
            ] {
                let out = machine.handle(Instant::now(), Timed::now(event));
                prop_assert_eq!(out.actions.len(), 1);
                let is_status_retry = matches!(
                    out.actions[0],
                    MamAction::ScheduleTimer { timer: MamTimer::StatusRetry, .. }
                );
                prop_assert!(is_status_retry);
                let unavailable_count = out
                    .publish
                    .iter()
                    .filter(|p| matches!(p, MamPublish::Unavailable { .. }))
                    .count();
                prop_assert_eq!(unavailable_count, 1);
            }
            // §28 / MAM-11: Unreachable publishes Unreachable, not Unavailable,
            // but still schedules exactly one StatusRetry.
            let out = machine.handle(
                Instant::now(),
                Timed::now(MamEvent::Unreachable { reason }),
            );
            prop_assert_eq!(out.actions.len(), 1);
            let is_status_retry = matches!(
                out.actions[0],
                MamAction::ScheduleTimer { timer: MamTimer::StatusRetry, .. }
            );
            prop_assert!(is_status_retry);
            let unreachable_count = out
                .publish
                .iter()
                .filter(|p| matches!(p, MamPublish::Unreachable { .. }))
                .count();
            prop_assert_eq!(unreachable_count, 1);
            let unavailable_count = out
                .publish
                .iter()
                .filter(|p| matches!(p, MamPublish::Unavailable { .. }))
                .count();
            prop_assert_eq!(
                unavailable_count, 0,
                "Unreachable must not also publish Unavailable"
            );
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

        // MAM-8 [safety] (§27): AuthSucceeded schedules KeepAlive at most once
        // per machine lifetime.  Repeated AuthSucceeded events never produce a
        // second KeepAlive ScheduleTimer.  Tested against a machine whose
        // keep_alive_scheduled flag is randomly true or false.
        #[test]
        fn keep_alive_chain_starts_at_most_once(mut machine in any_mam_machine()) {
            let was_scheduled = machine.keep_alive_scheduled();
            let out = machine.handle(Instant::now(), Timed::now(MamEvent::AuthSucceeded));
            let scheduled = out
                .actions
                .iter()
                .filter(|a| matches!(
                    a,
                    MamAction::ScheduleTimer { timer: MamTimer::KeepAlive, .. }
                ))
                .count();
            if was_scheduled {
                prop_assert_eq!(scheduled, 0,
                    "no second KeepAlive ScheduleTimer when chain already armed");
            } else {
                prop_assert_eq!(scheduled, 1,
                    "AuthSucceeded must arm KeepAlive exactly once");
            }
            // After handling AuthSucceeded the chain is always considered armed.
            prop_assert!(machine.keep_alive_scheduled());
        }

        // MAM-9 [liveness] (§27): TimerFired(KeepAlive) always emits exactly
        // one FetchStatus action and exactly one KeepAlive re-schedule action,
        // for any machine state.  The chain cannot die from a single handler
        // step.
        #[test]
        fn keep_alive_timer_always_reschedules(mut machine in any_mam_machine()) {
            let out = machine.handle(
                Instant::now(),
                Timed::now(MamEvent::TimerFired(MamTimer::KeepAlive)),
            );
            let fetch_count = out
                .actions
                .iter()
                .filter(|a| matches!(a, MamAction::FetchStatus))
                .count();
            let reschedule_count = out
                .actions
                .iter()
                .filter(|a| matches!(
                    a,
                    MamAction::ScheduleTimer { timer: MamTimer::KeepAlive, .. }
                ))
                .count();
            prop_assert_eq!(fetch_count, 1, "must emit exactly one FetchStatus");
            prop_assert_eq!(reschedule_count, 1, "must re-arm KeepAlive exactly once");
            prop_assert!(out.publish.is_empty(),
                "KeepAlive timer is side-effect-free on publishes");
        }

        // MAM-10 [safety] (§27): a retryable failure publishes
        // KeepAliveDegraded iff this event's bump crosses the configured
        // threshold from below.  Total invariant — holds for any starting
        // counter, including ones already over the threshold.
        #[test]
        fn keep_alive_degraded_publishes_iff_rising_edge(
            mut machine in any_mam_machine(),
            reason in any::<String>(),
            which in 0u8..3,
        ) {
            let before = machine.consecutive_status_failures();
            let threshold = machine.config.keep_alive_failure_threshold;

            let event = match which {
                0 => MamEvent::AuthFailed { reason: reason.clone() },
                1 => MamEvent::StatusFailed { reason: reason.clone() },
                _ => MamEvent::SeedboxUpdateFailed { reason: reason.clone() },
            };
            let out = machine.handle(Instant::now(), Timed::now(event));

            let after = machine.consecutive_status_failures();
            // The counter advances by exactly 1 unless saturated at u32::MAX.
            prop_assert!(after == before.saturating_add(1));

            let degraded_count = out
                .publish
                .iter()
                .filter(|p| matches!(p, MamPublish::KeepAliveDegraded { .. }))
                .count();

            let expected_publish = threshold > 0 && before < threshold && after >= threshold;
            if expected_publish {
                prop_assert_eq!(degraded_count, 1,
                    "rising edge must publish exactly one KeepAliveDegraded");
                if let Some(MamPublish::KeepAliveDegraded {
                    consecutive_failures, last_reason,
                }) = out.publish.iter().find(|p| matches!(p, MamPublish::KeepAliveDegraded { .. })) {
                    prop_assert_eq!(*consecutive_failures, after);
                    prop_assert_eq!(last_reason, &reason);
                }
            } else {
                prop_assert_eq!(degraded_count, 0,
                    "no KeepAliveDegraded outside the rising-edge transition");
            }
        }

        // MAM-11 [safety] (§28): the `Unreachable` event publishes exactly
        // one `MamPublish::Unreachable { reason }` and zero
        // `MamPublish::NotConnectable`.  Total invariant — `NotConnectable`
        // belongs strictly to `StatusFetched { connectable: false }`.
        #[test]
        fn unreachable_event_publishes_unreachable_not_notconnectable(
            mut machine in any_mam_machine(),
            reason in any::<String>(),
        ) {
            let out = machine.handle(
                Instant::now(),
                Timed::now(MamEvent::Unreachable { reason: reason.clone() }),
            );
            let unreachable_count = out
                .publish
                .iter()
                .filter(|p| matches!(p, MamPublish::Unreachable { .. }))
                .count();
            let notconnectable_count = out
                .publish
                .iter()
                .filter(|p| matches!(p, MamPublish::NotConnectable { .. }))
                .count();
            prop_assert_eq!(unreachable_count, 1);
            prop_assert_eq!(notconnectable_count, 0);
            if let Some(MamPublish::Unreachable { reason: r }) = out
                .publish
                .iter()
                .find(|p| matches!(p, MamPublish::Unreachable { .. }))
            {
                prop_assert_eq!(r, &reason);
            }
        }

        // MAM-12 [safety] (§28): `StatusFetched { connectable: false }`
        // publishes exactly one `NotConnectable` and zero `Unreachable`.
        // Total invariant — `Unreachable` belongs strictly to the
        // `Unreachable` event.
        #[test]
        fn status_fetched_not_connectable_publishes_notconnectable_not_unreachable(
            mut machine in any_mam_machine(),
            seedbox_port in proptest::option::of(any_vpn_port()),
            ratio in any_ratio(),
            upload_credit_bytes in any_buffer(),
        ) {
            let out = machine.handle(
                Instant::now(),
                Timed::now(MamEvent::StatusFetched {
                    connectable: false,
                    seedbox_port,
                    ratio,
                    upload_credit_bytes,
                }),
            );
            let notconnectable_count = out
                .publish
                .iter()
                .filter(|p| matches!(p, MamPublish::NotConnectable { .. }))
                .count();
            let unreachable_count = out
                .publish
                .iter()
                .filter(|p| matches!(p, MamPublish::Unreachable { .. }))
                .count();
            prop_assert_eq!(notconnectable_count, 1);
            prop_assert_eq!(unreachable_count, 0);
        }
    }
}
