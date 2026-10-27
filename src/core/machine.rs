use super::{
    Action, ClockReading, DecidingAfter, Event, HistoryState, Mam, MamApproval,
    MamPreferencePersistence, MamPreferenceState, MamPreferenceUpdate, Notifications,
    PortForwarding, PortForwardingEvent, Protection, Qbittorrent, Registry, SettingsState, Step,
    TimerName, Tunnel, TunnelEvent,
    mam::{ClientReadiness, Context, LeaseRenewal, TunnelProtection},
    mam_pause,
    notifications::{Facts, Glance, Startup},
    settings, startup,
    timer::Timers,
};

/// Owns application state and makes decisions without performing I/O.
#[derive(Debug, Default)]
pub struct Core {
    state: State,
    settings: SettingsState,
    history: HistoryState,
    port_forwarding: PortForwarding,
    qbittorrent: Qbittorrent,
    registry: Registry,
    tunnel: Tunnel,
    mam: Mam,
    notifications: Notifications,
    timers: Timers,
    step: Step,
}

/// The actual in-memory state of the core.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum State {
    /// Startup has not been requested.
    #[default]
    NotStarted,
    /// Waiting for the shell to open storage.
    OpeningStorage {
        /// The latest preference update received during startup.
        requested: Option<MamPreferenceUpdate>,
    },
    /// Waiting for the shell to read the MAM preference.
    ReadingMamPreference {
        /// The latest preference update received during startup.
        requested: Option<MamPreferenceUpdate>,
    },
    /// Startup completed; this is the live MAM preference state.
    Ready(MamPreferenceState),
    /// Startup failed; external work remains blocked.
    Failed,
}

impl State {
    /// Whether startup or the latest preference update failed.
    #[must_use]
    pub const fn has_failed(&self) -> bool {
        matches!(
            self,
            Self::Failed
                | Self::Ready(MamPreferenceState {
                    persistence: MamPreferencePersistence::Failed,
                    ..
                })
        )
    }
}

impl Core {
    /// Construct an idle core. Storage is read only after a startup event.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::NotStarted,
            settings: SettingsState::Unread,
            history: HistoryState::Closed,
            port_forwarding: PortForwarding::Disabled,
            qbittorrent: Qbittorrent::Disabled,
            registry: Registry::Unread,
            tunnel: Tunnel::new(),
            mam: Mam::new(),
            notifications: Notifications::new(),
            timers: Timers::new(),
            step: Step::ZERO,
        }
    }

    /// Handle one input and return effects for the shell to execute.
    ///
    /// Both event handlers receive the full state and the event's occurrence time.
    /// Every handled event is one history entry; trimming follows from its count.
    #[must_use]
    pub fn handle(&mut self, occurred_at: ClockReading, event: Event) -> Vec<Action> {
        self.step = self.step.next();
        let mut notices = self.notifications.reported(&event);
        let before = self.glance();
        let mut actions = match event {
            Event::Startup(event) => self.handle_startup(occurred_at, event),
            Event::MamPause(event) => mam_pause::handle(&mut self.state, occurred_at, event),
            Event::Settings(event) => settings::handle(&mut self.settings, event),
            Event::History(event) => {
                self.history.handle(self.step, event);
                Vec::new()
            }
            Event::TimerExpired { name, id } => match name {
                TimerName::PortLease => self.port_forwarding.timer_expired(id),
                TimerName::QbittorrentPoll => self.qbittorrent.timer_expired(id),
                TimerName::TunnelCheck => self.tunnel.timer_expired(occurred_at, id),
                TimerName::NotificationGrace | TimerName::NotificationRetry => {
                    self.notifications.timer_expired(id);
                    Vec::new()
                }
                TimerName::Mam => {
                    self.mam.timer_expired(id);
                    Vec::new()
                }
            },
            Event::PortForwarding(event) => {
                let failed = matches!(event, PortForwardingEvent::MappingFailed { .. });
                let mut actions = self
                    .port_forwarding
                    .handle(occurred_at, event, &mut self.timers);
                if failed {
                    actions.extend(self.tunnel.suspect(occurred_at));
                }
                actions
            }
            Event::Qbittorrent(event) => {
                self.qbittorrent
                    .handle(occurred_at, event, &mut self.timers)
            }
            Event::Registry(event) => self.registry.handle(occurred_at.monotonic, event),
            Event::Tunnel(event) => {
                let switched = matches!(event, TunnelEvent::Switched);
                let previous = self.tunnel.current().cloned();
                let mut actions = self.tunnel.handle(occurred_at, event, &mut self.timers);
                // A new server has a new gateway, forwarded port, and exit IP.
                if switched && self.tunnel.current() != previous.as_ref() {
                    actions.extend(self.port_forwarding.renew());
                    self.mam.vpn_switched();
                }
                actions
            }
            Event::Notification(event) => {
                self.notifications
                    .handle(occurred_at, &event, &mut self.timers)
            }
            Event::Mam(event) => self.handle_mam(occurred_at, event),
        };
        self.port_forwarding.expire(occurred_at.monotonic);
        if let Some((session, DecidingAfter::Sync)) = self.qbittorrent.deciding() {
            actions.extend(self.registry.observe(occurred_at, session));
        }
        actions.extend(self.decide_mam(occurred_at));
        if self.mam.take_lease_renewal() == LeaseRenewal::Requested {
            actions.extend(self.port_forwarding.renew());
        }
        actions.extend(self.decide_qbittorrent(occurred_at));
        actions.extend(
            self.history
                .after_step(self.step, self.settings.history_limit()),
        );
        notices.extend(self.notifications.changed(&before, &self.glance()));
        notices.extend(self.mam.take_notices());
        let facts = Facts {
            tunnel: &self.tunnel,
            port_forwarding: &self.port_forwarding,
            qbittorrent: &self.qbittorrent,
            registry: &self.registry,
            mam: &self.mam,
            reserve: self.settings.points_reserve(),
        };
        actions.extend(self.notifications.after_step(
            occurred_at,
            notices,
            facts,
            &self.settings,
            &mut self.timers,
        ));
        actions
    }

    /// Once storage opens, every stored part of the state is read.
    fn handle_startup(&mut self, now: ClockReading, event: super::StartupEvent) -> Vec<Action> {
        let opening = matches!(self.state, State::OpeningStorage { .. });
        let mut actions = startup::handle(&mut self.state, now, event);
        if opening && matches!(self.state, State::ReadingMamPreference { .. }) {
            actions.extend(self.settings.start_reading());
            actions.extend(self.history.open(self.step));
            actions.extend(self.registry.start_reading());
            actions.extend(self.tunnel.start_reading());
            actions.extend(self.mam.start_reading());
            actions.extend(Notifications::start_reading());
        }
        if matches!(self.state, State::Failed) {
            self.notifications.log_unavailable();
        }
        actions
    }

    /// A new cookie MAM issued is saved through settings.
    fn handle_mam(&mut self, now: ClockReading, event: super::MamEvent) -> Vec<Action> {
        let (mut actions, rotated) = self.mam.handle(now, event, &mut self.registry);
        if let Some(cookie) = rotated {
            actions.extend(settings::rotate(&mut self.settings, cookie));
        }
        actions
    }

    /// Let MAM work choose its next request from the rest of the core's state.
    fn decide_mam(&mut self, now: ClockReading) -> Vec<Action> {
        let loaded = self.settings.loaded();
        let session = match &self.qbittorrent {
            Qbittorrent::Connected(session) => Some(&**session),
            _ => None,
        };
        let context = Context {
            requests: self.state.mam_requests(),
            protection: if self
                .tunnel
                .watch
                .as_ref()
                .is_some_and(|watch| matches!(watch.protection, Protection::Available { .. }))
            {
                TunnelProtection::Protected
            } else {
                TunnelProtection::Unprotected
            },
            cookie: loaded.and_then(super::Settings::usable_api_cookie),
            requested_cookie: loaded.and_then(|settings| settings.api_cookie.requested.as_ref()),
            reserve: self.settings.points_reserve(),
            readiness: match &self.qbittorrent {
                Qbittorrent::Connected(session)
                    if session.listed() && session.approval != MamApproval::NotAllowed =>
                {
                    ClientReadiness::Ready
                }
                _ => ClientReadiness::NotReady,
            },
            client_version: session.and_then(|session| session.version.release()),
            connection: super::Connection {
                listen_port: session.and_then(|session| session.preferences.listen_port),
                forwarded_port: self.port_forwarding.forwarded_port(),
                tunnel_ip: self
                    .tunnel
                    .watch
                    .as_ref()
                    .and_then(|watch| match watch.protection {
                        Protection::Available { exit_ip, .. } => Some(exit_ip),
                        _ => None,
                    }),
                interface: session.and_then(|session| session.preferences.interface.clone()),
            },
            // Until settings are read, the default limits apply.
            downloading_limit: self
                .settings
                .torrent_limits()
                .unwrap_or_default()
                .downloading(),
            registry: &self.registry,
        };
        self.mam.decide(now, &context, &mut self.timers)
    }

    fn decide_qbittorrent(&mut self, now: ClockReading) -> Vec<Action> {
        let wanted = super::Wanted {
            listen_port: self.port_forwarding.forwarded_port(),
            queue: self
                .settings
                .torrent_limits()
                .map(super::QueuePreferences::for_limits),
        };
        let registry = &mut self.registry;
        let mam = &self.mam;
        let mut actions = self
            .qbittorrent
            .decide(now, &mut self.timers, wanted, |session| {
                registry.command(now.monotonic, session, |id| mam.permits_download(id))
            });
        actions.extend(self.qbittorrent.follow_preferences(wanted));
        actions
    }

    fn glance(&self) -> Glance {
        Glance::of(
            &self.tunnel,
            match self.state {
                State::Failed => Startup::Failed,
                State::Ready(_) => Startup::Ready,
                _ => Startup::Starting,
            },
        )
    }

    /// Inspect notifications and their delivery.
    #[must_use]
    pub const fn notifications(&self) -> &Notifications {
        &self.notifications
    }

    /// Inspect the actual state without allowing mutation outside event handling.
    #[must_use]
    pub const fn state(&self) -> &State {
        &self.state
    }

    /// Inspect the dashboard settings.
    #[must_use]
    pub const fn settings(&self) -> &SettingsState {
        &self.settings
    }

    /// Inspect Proton VPN port forwarding.
    #[must_use]
    pub const fn port_forwarding(&self) -> &PortForwarding {
        &self.port_forwarding
    }

    /// Inspect what the core knows about qBittorrent.
    #[must_use]
    pub const fn qbittorrent(&self) -> &Qbittorrent {
        &self.qbittorrent
    }

    /// Inspect the VPN tunnel.
    #[must_use]
    pub const fn tunnel(&self) -> &Tunnel {
        &self.tunnel
    }

    /// Inspect Windlass's work with MAM.
    #[must_use]
    pub const fn mam(&self) -> &Mam {
        &self.mam
    }

    /// Inspect the torrent registry.
    #[must_use]
    pub const fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Inspect what the core knows about stored history.
    #[must_use]
    pub const fn history(&self) -> &HistoryState {
        &self.history
    }
}
