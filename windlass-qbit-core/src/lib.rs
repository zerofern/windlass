#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome, Timed};
use windlass_types::{AuthCookie, TorrentHash, VpnPort};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QbitConfig {
    pub auth_retry: Duration,
    pub sync_retry: Duration,
    pub torrent_refresh: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QbitCommand {
    EnsureAuthenticated,
    EnsureListenPort { port: VpnPort },
    RefreshTorrents,
    PauseTorrent { hash: TorrentHash },
    ResumeTorrent { hash: TorrentHash },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QbitTimer {
    AuthRetry,
    SyncRetry,
    TorrentRefresh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QbitEvent {
    Init,
    AuthSucceeded { cookie: AuthCookie },
    AuthFailed { reason: String },
    PreferencesRead { listen_port: Option<VpnPort> },
    PreferencesFailed { reason: String },
    ListenPortSet { port: VpnPort },
    ListenPortSetFailed { port: VpnPort, reason: String },
    TorrentsListed { hashes: Vec<TorrentHash> },
    TimerFired(QbitTimer),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QbitAction {
    Login,
    ReadPreferences {
        cookie: AuthCookie,
    },
    SetListenPort {
        cookie: AuthCookie,
        port: VpnPort,
    },
    ListTorrents {
        cookie: AuthCookie,
    },
    PauseTorrent {
        cookie: AuthCookie,
        hash: TorrentHash,
    },
    ResumeTorrent {
        cookie: AuthCookie,
        hash: TorrentHash,
    },
    ScheduleTimer {
        timer: QbitTimer,
        after: Duration,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QbitPublish {
    Ready,
    Unavailable { reason: String },
    ListenPortReady { port: VpnPort },
    TorrentsUpdated { hashes: Vec<TorrentHash> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QbitTopic {
    Availability,
    ListenPort,
    Torrents,
}

impl HasTopic<QbitTopic> for QbitPublish {
    fn topic(&self) -> QbitTopic {
        match self {
            Self::Ready | Self::Unavailable { .. } => QbitTopic::Availability,
            Self::ListenPortReady { .. } => QbitTopic::ListenPort,
            Self::TorrentsUpdated { .. } => QbitTopic::Torrents,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QbitResponse {
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QbitMachine {
    config: QbitConfig,
    cookie: Option<AuthCookie>,
    listen_port: Option<VpnPort>,
    desired_listen_port: Option<VpnPort>,
    /// True once the self-perpetuating `TorrentRefresh` timer chain has been started.
    /// Prevents a second independent chain from being spawned on re-authentication.
    refresh_scheduled: bool,
}

impl QbitMachine {
    #[must_use]
    pub const fn is_authenticated(&self) -> bool {
        self.cookie.is_some()
    }

    #[must_use]
    pub const fn listen_port(&self) -> Option<VpnPort> {
        self.listen_port
    }

    fn retry_listen_port_or_read_preferences(&self) -> Vec<QbitAction> {
        let Some(cookie) = self.cookie.clone() else {
            return vec![QbitAction::Login];
        };
        match self.desired_listen_port {
            None => vec![QbitAction::ReadPreferences { cookie }],
            Some(port) => vec![QbitAction::SetListenPort { cookie, port }],
        }
    }

    fn converge_listen_port(&self) -> Vec<QbitAction> {
        let Some(port) = self.desired_listen_port else {
            return Vec::new();
        };
        if self.listen_port == Some(port) {
            return Vec::new();
        }
        self.cookie.clone().map_or_else(
            || vec![QbitAction::Login],
            |cookie| vec![QbitAction::SetListenPort { cookie, port }],
        )
    }

    fn listen_port_publish(&self, listen_port: Option<VpnPort>) -> Vec<QbitPublish> {
        listen_port
            .filter(|port| {
                self.desired_listen_port
                    .is_none_or(|desired_port| desired_port == *port)
            })
            .map(|port| QbitPublish::ListenPortReady { port })
            .into_iter()
            .collect()
    }
}

impl Machine for QbitMachine {
    type Config = QbitConfig;
    type Event = QbitEvent;
    type Action = QbitAction;
    type Publish = QbitPublish;
    type Topic = QbitTopic;
    type Command = QbitCommand;
    type Response = QbitResponse;

    fn new(config: Self::Config, _now: Instant) -> Self {
        Self {
            config,
            cookie: None,
            listen_port: None,
            desired_listen_port: None,
            refresh_scheduled: false,
        }
    }

    fn handle(
        &mut self,
        _now: Instant,
        event: Timed<Self::Event>,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event.inner {
            QbitEvent::Init | QbitEvent::TimerFired(QbitTimer::AuthRetry) => Outcome {
                actions: vec![QbitAction::Login],
                publish: Vec::new(),
            },
            QbitEvent::AuthSucceeded { cookie } => {
                self.cookie = Some(cookie.clone());
                let mut actions = self.desired_listen_port.map_or_else(
                    || {
                        vec![QbitAction::ReadPreferences {
                            cookie: cookie.clone(),
                        }]
                    },
                    |port| {
                        vec![QbitAction::SetListenPort {
                            cookie: cookie.clone(),
                            port,
                        }]
                    },
                );
                if !self.refresh_scheduled {
                    self.refresh_scheduled = true;
                    actions.push(QbitAction::ScheduleTimer {
                        timer: QbitTimer::TorrentRefresh,
                        after: self.config.torrent_refresh,
                    });
                }
                Outcome {
                    actions,
                    publish: vec![QbitPublish::Ready],
                }
            }
            QbitEvent::AuthFailed { reason } => Outcome {
                actions: vec![QbitAction::ScheduleTimer {
                    timer: QbitTimer::AuthRetry,
                    after: self.config.auth_retry,
                }],
                publish: vec![QbitPublish::Unavailable { reason }],
            },
            QbitEvent::PreferencesRead { listen_port } => {
                self.listen_port = listen_port;
                Outcome {
                    actions: self.converge_listen_port(),
                    publish: self.listen_port_publish(listen_port),
                }
            }
            QbitEvent::PreferencesFailed { reason }
            | QbitEvent::ListenPortSetFailed { reason, .. } => Outcome {
                actions: vec![QbitAction::ScheduleTimer {
                    timer: QbitTimer::SyncRetry,
                    after: self.config.sync_retry,
                }],
                publish: vec![QbitPublish::Unavailable { reason }],
            },
            QbitEvent::ListenPortSet { port } => {
                self.listen_port = Some(port);
                Outcome {
                    actions: Vec::new(),
                    publish: vec![QbitPublish::ListenPortReady { port }],
                }
            }
            QbitEvent::TorrentsListed { hashes } => Outcome {
                actions: Vec::new(),
                publish: vec![QbitPublish::TorrentsUpdated { hashes }],
            },
            QbitEvent::TimerFired(QbitTimer::SyncRetry) => Outcome {
                actions: self.retry_listen_port_or_read_preferences(),
                publish: Vec::new(),
            },
            QbitEvent::TimerFired(QbitTimer::TorrentRefresh) => {
                let mut actions = self
                    .cookie
                    .clone()
                    .map_or_else(Vec::new, |cookie| vec![QbitAction::ListTorrents { cookie }]);
                actions.push(QbitAction::ScheduleTimer {
                    timer: QbitTimer::TorrentRefresh,
                    after: self.config.torrent_refresh,
                });
                Outcome {
                    actions,
                    publish: Vec::new(),
                }
            }
        }
    }

    fn handle_command(
        &mut self,
        _now: Instant,
        cmd: Self::Command,
    ) -> CommandOutcome<Self::Action, Self::Publish, Self::Response> {
        let actions = match cmd {
            QbitCommand::EnsureAuthenticated => vec![QbitAction::Login],
            QbitCommand::EnsureListenPort { port } => {
                self.desired_listen_port = Some(port);
                if self.listen_port == Some(port) {
                    return Self::outcome_with_publish(
                        Vec::new(),
                        vec![QbitPublish::ListenPortReady { port }],
                        QbitResponse::Accepted,
                    );
                }
                self.cookie.clone().map_or_else(
                    || vec![QbitAction::Login],
                    |cookie| vec![QbitAction::SetListenPort { cookie, port }],
                )
            }
            QbitCommand::RefreshTorrents => self
                .cookie
                .clone()
                .map_or_else(Vec::new, |cookie| vec![QbitAction::ListTorrents { cookie }]),
            QbitCommand::PauseTorrent { hash } => {
                self.cookie.clone().map_or_else(Vec::new, |cookie| {
                    vec![QbitAction::PauseTorrent { cookie, hash }]
                })
            }
            QbitCommand::ResumeTorrent { hash } => {
                self.cookie.clone().map_or_else(Vec::new, |cookie| {
                    vec![QbitAction::ResumeTorrent { cookie, hash }]
                })
            }
        };
        Self::outcome(actions, QbitResponse::Accepted)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use windlass_machine::{Machine, Outcome, Timed};
    use windlass_types::{AuthCookie, VpnPort};

    use crate::{
        QbitAction, QbitCommand, QbitConfig, QbitEvent, QbitMachine, QbitPublish, QbitTimer,
    };

    fn machine() -> QbitMachine {
        QbitMachine::new(
            QbitConfig {
                auth_retry: Duration::from_secs(1),
                sync_retry: Duration::from_secs(2),
                torrent_refresh: Duration::from_secs(30),
            },
            Instant::now(),
        )
    }

    fn handle(machine: &mut QbitMachine, event: QbitEvent) -> Outcome<QbitAction, QbitPublish> {
        machine.handle(Instant::now(), Timed::now(event))
    }

    #[test]
    fn init_logs_in() {
        let mut machine = machine();

        let out = handle(&mut machine, QbitEvent::Init);

        assert_eq!(out.actions, vec![QbitAction::Login]);
    }

    #[test]
    fn auth_success_publishes_ready_and_reads_preferences() {
        let mut machine = machine();

        let cookie = AuthCookie::new("sid".to_string());
        let out = handle(
            &mut machine,
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );

        assert!(machine.is_authenticated());
        assert_eq!(
            out.actions,
            vec![
                QbitAction::ReadPreferences { cookie },
                QbitAction::ScheduleTimer {
                    timer: QbitTimer::TorrentRefresh,
                    after: Duration::from_secs(30),
                },
            ]
        );
        assert_eq!(out.publish, vec![QbitPublish::Ready]);
    }

    #[test]
    fn ensure_listen_port_requires_authentication() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = machine.handle_command(Instant::now(), QbitCommand::EnsureListenPort { port });

        assert_eq!(out.actions, vec![QbitAction::Login]);
    }

    #[test]
    fn auth_success_sets_desired_port_after_pre_auth_request() {
        let mut machine = machine();
        let cookie = AuthCookie::new("sid".to_string());
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = machine.handle_command(Instant::now(), QbitCommand::EnsureListenPort { port });

        let out = handle(
            &mut machine,
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );

        assert_eq!(
            out.actions,
            vec![
                QbitAction::SetListenPort { cookie, port },
                QbitAction::ScheduleTimer {
                    timer: QbitTimer::TorrentRefresh,
                    after: Duration::from_secs(30),
                },
            ]
        );
        assert_eq!(out.publish, vec![QbitPublish::Ready]);
    }

    #[test]
    fn ensure_listen_port_carries_cookie_when_authenticated() {
        let mut machine = machine();
        let cookie = AuthCookie::new("sid".to_string());
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = handle(
            &mut machine,
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );

        let out = machine.handle_command(Instant::now(), QbitCommand::EnsureListenPort { port });

        assert_eq!(
            out.actions,
            vec![QbitAction::SetListenPort { cookie, port }]
        );
    }

    #[test]
    fn preference_mismatch_sets_desired_port_without_publishing_ready() {
        let mut machine = machine();
        let cookie = AuthCookie::new("sid".to_string());
        let desired = VpnPort::try_new(51_820).unwrap();
        let observed = VpnPort::try_new(42_000).unwrap();
        let _ = handle(
            &mut machine,
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );
        let _ = machine.handle_command(
            Instant::now(),
            QbitCommand::EnsureListenPort { port: desired },
        );

        let out = handle(
            &mut machine,
            QbitEvent::PreferencesRead {
                listen_port: Some(observed),
            },
        );

        assert_eq!(
            out.actions,
            vec![QbitAction::SetListenPort {
                cookie,
                port: desired,
            }]
        );
        assert!(out.publish.is_empty());
    }

    #[test]
    fn set_failure_publishes_unavailable_and_retries_desired_port() {
        let mut machine = machine();
        let cookie = AuthCookie::new("sid".to_string());
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = handle(
            &mut machine,
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );
        let _ = machine.handle_command(Instant::now(), QbitCommand::EnsureListenPort { port });

        let failed = handle(
            &mut machine,
            QbitEvent::ListenPortSetFailed {
                port,
                reason: "forbidden".to_string(),
            },
        );

        assert_eq!(
            failed.actions,
            vec![QbitAction::ScheduleTimer {
                timer: QbitTimer::SyncRetry,
                after: Duration::from_secs(2),
            }]
        );
        assert_eq!(
            failed.publish,
            vec![QbitPublish::Unavailable {
                reason: "forbidden".to_string(),
            }]
        );

        let retry = handle(&mut machine, QbitEvent::TimerFired(QbitTimer::SyncRetry));

        assert_eq!(
            retry.actions,
            vec![QbitAction::SetListenPort { cookie, port }]
        );
    }

    #[test]
    fn ensure_listen_port_publishes_when_already_converged() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = handle(&mut machine, QbitEvent::ListenPortSet { port });

        let out = machine.handle_command(Instant::now(), QbitCommand::EnsureListenPort { port });

        assert!(out.actions.is_empty());
        assert_eq!(out.publish, vec![QbitPublish::ListenPortReady { port }]);
    }

    #[test]
    fn listen_port_set_publishes_ready_port() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = handle(&mut machine, QbitEvent::ListenPortSet { port });

        assert_eq!(machine.listen_port(), Some(port));
        assert_eq!(out.publish, vec![QbitPublish::ListenPortReady { port }]);
    }

    #[test]
    fn torrent_refresh_timer_round_trips() {
        // Phase 1: AuthSucceeded schedules the TorrentRefresh timer.
        let mut machine = machine();
        let cookie = AuthCookie::new("sid".to_string());
        let auth_out = handle(
            &mut machine,
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );
        assert!(
            auth_out.actions.contains(&QbitAction::ScheduleTimer {
                timer: QbitTimer::TorrentRefresh,
                after: Duration::from_secs(30),
            }),
            "AuthSucceeded must schedule TorrentRefresh timer"
        );

        // Phase 2: When the timer fires, ListTorrents is issued and the timer re-schedules.
        let fired_out = handle(
            &mut machine,
            QbitEvent::TimerFired(QbitTimer::TorrentRefresh),
        );
        assert!(
            fired_out
                .actions
                .contains(&QbitAction::ListTorrents { cookie }),
            "TorrentRefresh timer must issue ListTorrents"
        );
        assert!(
            fired_out.actions.contains(&QbitAction::ScheduleTimer {
                timer: QbitTimer::TorrentRefresh,
                after: Duration::from_secs(30),
            }),
            "TorrentRefresh timer must re-schedule itself"
        );
    }

    #[test]
    fn auth_succeeded_twice_schedules_refresh_timer_only_once() {
        // Two consecutive AuthSucceeded events (e.g. from dual-Init login race) must not
        // produce a second independent TorrentRefresh timer chain.
        let mut machine = machine();
        let cookie = AuthCookie::new("sid".to_string());

        let first_out = handle(
            &mut machine,
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );
        let schedule_action = QbitAction::ScheduleTimer {
            timer: QbitTimer::TorrentRefresh,
            after: Duration::from_secs(30),
        };
        assert!(
            first_out.actions.contains(&schedule_action),
            "first AuthSucceeded must schedule TorrentRefresh"
        );

        let second_out = handle(
            &mut machine,
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );
        assert!(
            !second_out.actions.contains(&schedule_action),
            "second AuthSucceeded must NOT schedule a second TorrentRefresh chain"
        );
    }
}

#[cfg(test)]
mod prop_tests {
    use std::time::{Duration, Instant};

    use proptest::prelude::*;
    use windlass_machine::{Machine, Timed};
    use windlass_types::{AuthCookie, TorrentHash, VpnPort};

    use crate::{QbitAction, QbitCommand, QbitConfig, QbitEvent, QbitMachine, QbitPublish, QbitTimer};

    fn any_vpn_port() -> impl Strategy<Value = VpnPort> {
        (1u16..=u16::MAX).prop_map(|p| VpnPort::try_new(p).unwrap())
    }

    fn any_auth_cookie() -> impl Strategy<Value = AuthCookie> {
        "[a-zA-Z0-9]{8,32}".prop_map(AuthCookie::new)
    }

    fn any_torrent_hash() -> impl Strategy<Value = TorrentHash> {
        "[a-f0-9]{40}".prop_map(TorrentHash)
    }

    // Fully-arbitrary state, including unreachable combinations: the tested
    // invariants are total.
    fn any_qbit_machine() -> impl Strategy<Value = QbitMachine> {
        (
            proptest::option::of(any_auth_cookie()),
            proptest::option::of(any_vpn_port()),
            proptest::option::of(any_vpn_port()),
            any::<bool>(),
        )
            .prop_map(|(cookie, listen_port, desired_listen_port, refresh_scheduled)| {
                let mut machine = QbitMachine::new(
                    QbitConfig {
                        auth_retry: Duration::from_secs(1),
                        sync_retry: Duration::from_secs(2),
                        torrent_refresh: Duration::from_secs(30),
                    },
                    Instant::now(),
                );
                machine.cookie = cookie;
                machine.listen_port = listen_port;
                machine.desired_listen_port = desired_listen_port;
                machine.refresh_scheduled = refresh_scheduled;
                machine
            })
    }

    fn any_qbit_event() -> impl Strategy<Value = QbitEvent> {
        prop_oneof![
            Just(QbitEvent::Init),
            any_auth_cookie().prop_map(|cookie| QbitEvent::AuthSucceeded { cookie }),
            any::<String>().prop_map(|reason| QbitEvent::AuthFailed { reason }),
            proptest::option::of(any_vpn_port())
                .prop_map(|listen_port| QbitEvent::PreferencesRead { listen_port }),
            any::<String>().prop_map(|reason| QbitEvent::PreferencesFailed { reason }),
            any_vpn_port().prop_map(|port| QbitEvent::ListenPortSet { port }),
            (any_vpn_port(), any::<String>())
                .prop_map(|(port, reason)| QbitEvent::ListenPortSetFailed { port, reason }),
            prop::collection::vec(any_torrent_hash(), 0..4)
                .prop_map(|hashes| QbitEvent::TorrentsListed { hashes }),
            Just(QbitEvent::TimerFired(QbitTimer::AuthRetry)),
            Just(QbitEvent::TimerFired(QbitTimer::SyncRetry)),
            Just(QbitEvent::TimerFired(QbitTimer::TorrentRefresh)),
        ]
    }

    fn any_qbit_command() -> impl Strategy<Value = QbitCommand> {
        prop_oneof![
            Just(QbitCommand::EnsureAuthenticated),
            any_vpn_port().prop_map(|port| QbitCommand::EnsureListenPort { port }),
            Just(QbitCommand::RefreshTorrents),
            any_torrent_hash().prop_map(|hash| QbitCommand::PauseTorrent { hash }),
            any_torrent_hash().prop_map(|hash| QbitCommand::ResumeTorrent { hash }),
        ]
    }

    fn carries_cookie(action: &QbitAction) -> bool {
        matches!(
            action,
            QbitAction::ReadPreferences { .. }
                | QbitAction::SetListenPort { .. }
                | QbitAction::ListTorrents { .. }
                | QbitAction::PauseTorrent { .. }
                | QbitAction::ResumeTorrent { .. }
        )
    }

    proptest! {
        // GLOBAL-1 (no panic).
        #[test]
        fn handle_never_panics(mut machine in any_qbit_machine(), event in any_qbit_event()) {
            let _ = machine.handle(Instant::now(), Timed::now(event));
        }

        // QBIT-1 (Guarantees C/D): no cookie-bearing action is emitted unless the
        // machine is authenticated — for events and for commands.
        #[test]
        fn events_emit_no_cookie_action_while_unauthenticated(
            mut machine in any_qbit_machine(),
            event in any_qbit_event(),
        ) {
            let out = machine.handle(Instant::now(), Timed::now(event));
            for action in &out.actions {
                if carries_cookie(action) {
                    prop_assert!(machine.is_authenticated());
                }
            }
        }

        #[test]
        fn commands_emit_no_cookie_action_while_unauthenticated(
            mut machine in any_qbit_machine(),
            command in any_qbit_command(),
        ) {
            let out = machine.handle_command(Instant::now(), command);
            for action in &out.actions {
                if carries_cookie(action) {
                    prop_assert!(machine.is_authenticated());
                }
            }
        }

        // QBIT-4 (Guarantee C): every published ListenPortReady carries a port
        // that agrees with the desired target (or there is no desired target).
        // The ListenPortSet event is constrained to the shell contract — it only
        // ever carries the desired port (it is the success of a SetListenPort the
        // machine issued). See operator-readiness story 18.
        #[test]
        fn listen_port_ready_matches_desired(
            (mut machine, event) in (any_qbit_machine(), any_qbit_event()).prop_map(
                |(machine, event)| {
                    let event = match (event, machine.desired_listen_port) {
                        (QbitEvent::ListenPortSet { .. }, Some(port)) => {
                            QbitEvent::ListenPortSet { port }
                        }
                        (other, _) => other,
                    };
                    (machine, event)
                }
            ),
        ) {
            let out = machine.handle(Instant::now(), Timed::now(event));
            for publish in &out.publish {
                if let QbitPublish::ListenPortReady { port } = publish {
                    prop_assert!(
                        machine.desired_listen_port.is_none()
                            || machine.desired_listen_port == Some(*port)
                    );
                }
            }
        }
    }
}
