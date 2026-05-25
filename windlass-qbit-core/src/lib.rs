#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windlass_machine::{CommandOutcome, HasTopic, Machine, Outcome};
use windlass_types::{AuthCookie, TorrentHash, VpnPort};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QbitConfig {
    pub auth_retry: Duration,
    pub sync_retry: Duration,
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
        }
    }

    fn handle(
        &mut self,
        _now: Instant,
        event: Self::Event,
    ) -> Outcome<Self::Action, Self::Publish> {
        match event {
            QbitEvent::Init | QbitEvent::TimerFired(QbitTimer::AuthRetry) => Outcome {
                actions: vec![QbitAction::Login],
                publish: Vec::new(),
            },
            QbitEvent::AuthSucceeded { cookie } => {
                self.cookie = Some(cookie.clone());
                Outcome {
                    actions: self.desired_listen_port.map_or_else(
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
                    ),
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
            QbitEvent::TimerFired(QbitTimer::TorrentRefresh) => Outcome {
                actions: self
                    .cookie
                    .clone()
                    .map_or_else(Vec::new, |cookie| vec![QbitAction::ListTorrents { cookie }]),
                publish: Vec::new(),
            },
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

    use windlass_machine::Machine;
    use windlass_types::{AuthCookie, VpnPort};

    use crate::{
        QbitAction, QbitCommand, QbitConfig, QbitEvent, QbitMachine, QbitPublish, QbitTimer,
    };

    fn machine() -> QbitMachine {
        QbitMachine::new(
            QbitConfig {
                auth_retry: Duration::from_secs(1),
                sync_retry: Duration::from_secs(2),
            },
            Instant::now(),
        )
    }

    #[test]
    fn init_logs_in() {
        let mut machine = machine();

        let out = machine.handle(Instant::now(), QbitEvent::Init);

        assert_eq!(out.actions, vec![QbitAction::Login]);
    }

    #[test]
    fn auth_success_publishes_ready_and_reads_preferences() {
        let mut machine = machine();

        let cookie = AuthCookie("sid".to_string());
        let out = machine.handle(
            Instant::now(),
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );

        assert!(machine.is_authenticated());
        assert_eq!(out.actions, vec![QbitAction::ReadPreferences { cookie }]);
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
        let cookie = AuthCookie("sid".to_string());
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = machine.handle_command(Instant::now(), QbitCommand::EnsureListenPort { port });

        let out = machine.handle(
            Instant::now(),
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );

        assert_eq!(
            out.actions,
            vec![QbitAction::SetListenPort { cookie, port }]
        );
        assert_eq!(out.publish, vec![QbitPublish::Ready]);
    }

    #[test]
    fn ensure_listen_port_carries_cookie_when_authenticated() {
        let mut machine = machine();
        let cookie = AuthCookie("sid".to_string());
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = machine.handle(
            Instant::now(),
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
        let cookie = AuthCookie("sid".to_string());
        let desired = VpnPort::try_new(51_820).unwrap();
        let observed = VpnPort::try_new(42_000).unwrap();
        let _ = machine.handle(
            Instant::now(),
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );
        let _ = machine.handle_command(
            Instant::now(),
            QbitCommand::EnsureListenPort { port: desired },
        );

        let out = machine.handle(
            Instant::now(),
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
        let cookie = AuthCookie("sid".to_string());
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = machine.handle(
            Instant::now(),
            QbitEvent::AuthSucceeded {
                cookie: cookie.clone(),
            },
        );
        let _ = machine.handle_command(Instant::now(), QbitCommand::EnsureListenPort { port });

        let failed = machine.handle(
            Instant::now(),
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

        let retry = machine.handle(Instant::now(), QbitEvent::TimerFired(QbitTimer::SyncRetry));

        assert_eq!(
            retry.actions,
            vec![QbitAction::SetListenPort { cookie, port }]
        );
    }

    #[test]
    fn ensure_listen_port_publishes_when_already_converged() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();
        let _ = machine.handle(Instant::now(), QbitEvent::ListenPortSet { port });

        let out = machine.handle_command(Instant::now(), QbitCommand::EnsureListenPort { port });

        assert!(out.actions.is_empty());
        assert_eq!(out.publish, vec![QbitPublish::ListenPortReady { port }]);
    }

    #[test]
    fn listen_port_set_publishes_ready_port() {
        let mut machine = machine();
        let port = VpnPort::try_new(51_820).unwrap();

        let out = machine.handle(Instant::now(), QbitEvent::ListenPortSet { port });

        assert_eq!(machine.listen_port(), Some(port));
        assert_eq!(out.publish, vec![QbitPublish::ListenPortReady { port }]);
    }
}
