use super::{
    Action, ClockReading, Event, HistoryState, MamPreferencePersistence, MamPreferenceState,
    MamPreferenceUpdate, PortForwarding, Qbittorrent, Registry, SettingsState, Step, TimerName,
    mam_pause, settings, startup, timer::Timers,
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
        let mut actions = match event {
            Event::Startup(event) => {
                let opening = matches!(self.state, State::OpeningStorage { .. });
                let mut actions = startup::handle(&mut self.state, occurred_at, event);
                if opening && matches!(self.state, State::ReadingMamPreference { .. }) {
                    actions.extend(self.settings.start_reading());
                    actions.extend(self.history.open(self.step));
                    actions.extend(self.registry.start_reading());
                }
                actions
            }
            Event::MamPause(event) => mam_pause::handle(&mut self.state, occurred_at, event),
            Event::Settings(event) => settings::handle(&mut self.settings, event),
            Event::History(event) => {
                self.history.handle(self.step, event);
                Vec::new()
            }
            Event::TimerExpired { name, id } => match name {
                TimerName::PortLease => self.port_forwarding.timer_expired(id),
                TimerName::QbittorrentPoll => self.qbittorrent.timer_expired(id),
            },
            Event::PortForwarding(event) => {
                self.port_forwarding
                    .handle(occurred_at, event, &mut self.timers)
            }
            Event::Qbittorrent(event) => {
                self.qbittorrent
                    .handle(occurred_at, event, &mut self.timers)
            }
            Event::Registry(event) => self.registry.handle(occurred_at.monotonic, event),
        };
        self.port_forwarding.expire(occurred_at.monotonic);
        if let Some((session, true)) = self.qbittorrent.deciding() {
            actions.extend(self.registry.observe(occurred_at, session));
        }
        let registry = &mut self.registry;
        actions.extend(self.qbittorrent.decide(
            occurred_at,
            &mut self.timers,
            self.port_forwarding.forwarded_port(),
            |session| registry.command(occurred_at.monotonic, session),
        ));
        actions.extend(
            self.qbittorrent
                .follow_forwarded_port(self.port_forwarding.forwarded_port()),
        );
        actions.extend(
            self.history
                .after_step(self.step, self.settings.history_limit()),
        );
        actions
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
