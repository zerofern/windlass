//! Deliver timestamped events to the core and dispatch its actions through the shell.

use std::path::PathBuf;

use tokio::sync::watch;

use crate::{
    core::{
        Core, HistoryState, Mam, Notifications, PortForwarding, Qbittorrent, Registry,
        SettingsState, State, TimedEvent, Tunnel,
    },
    shell::{EventSender, Integrations, Shell, ShellResult, read_clock},
};

/// A read-only observation containing the actual core state and event metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// The core's in-memory state after handling the event.
    pub state: State,
    /// The core's dashboard settings.
    pub settings: SettingsState,
    /// What the core knows about stored history.
    pub history: HistoryState,
    /// Proton VPN port forwarding.
    pub port_forwarding: PortForwarding,
    /// What the core knows about qBittorrent.
    pub qbittorrent: Qbittorrent,
    /// The torrent registry.
    pub registry: Registry,
    /// The VPN tunnel.
    pub tunnel: Tunnel,
    /// Notifications and their delivery.
    pub notifications: Notifications,
    /// Windlass's work with MAM.
    pub mam: Mam,
    /// The most recently handled event, absent before the first input.
    pub last_event: Option<TimedEvent>,
    /// The latest storage error, retained across subsequent state updates.
    pub storage_error: Option<String>,
}

impl Observation {
    fn of(core: &Core, last_event: Option<TimedEvent>, storage_error: Option<String>) -> Self {
        Self {
            state: core.state().clone(),
            settings: core.settings().clone(),
            history: *core.history(),
            port_forwarding: *core.port_forwarding(),
            qbittorrent: core.qbittorrent().clone(),
            registry: core.registry().clone(),
            tunnel: core.tunnel().clone(),
            notifications: core.notifications().clone(),
            mam: core.mam().clone(),
            last_event,
            storage_error,
        }
    }
}

/// Owns the core and consumes one event stream, independently of input sources.
#[derive(Debug)]
pub struct Runner {
    core: Core,
    shell: Shell,
    observations: watch::Sender<Observation>,
}

impl Runner {
    /// Create a runner, a cloneable event ingress, and a state subscription.
    ///
    /// Producers submit startup and user requests through the ingress. Cloning
    /// the subscription gives another observer access to the latest core state.
    #[must_use]
    pub fn new(database: PathBuf) -> (Self, EventSender, watch::Receiver<Observation>) {
        Self::with_integrations(database, Integrations::default())
    }

    /// Like [`Runner::new`], with external services the shell may reach.
    ///
    /// Send [`Integrations::enable_events`] to start using them.
    #[must_use]
    pub fn with_integrations(
        database: PathBuf,
        integrations: Integrations,
    ) -> (Self, EventSender, watch::Receiver<Observation>) {
        let core = Core::new();
        let (shell, events) = Shell::new(database, integrations);
        let (observations, receiver) = watch::channel(Observation::of(&core, None, None));
        (
            Self {
                core,
                shell,
                observations,
            },
            events,
            receiver,
        )
    }

    /// Handle events until shutdown finishes all accepted inputs and effects.
    ///
    /// Each handled event and its actions are recorded in history, in processing
    /// order, before the actions are dispatched.
    ///
    /// # Errors
    /// Returns an error if a storage worker fails unexpectedly. Ordinary storage
    /// failures return as core events and remain visible in the observations.
    pub async fn run(mut self) -> ShellResult<State> {
        while let Some(event) = self.shell.next_event().await? {
            let processed_at = read_clock().wall;
            let actions = self.core.handle(event.occurred_at, event.event.clone());
            self.shell.record(
                event.occurred_at.wall,
                processed_at,
                event.event.clone(),
                actions.clone(),
            );
            for action in actions {
                self.shell.dispatch(action);
            }
            self.observations.send_replace(Observation::of(
                &self.core,
                Some(event),
                self.shell.latest_storage_error().cloned(),
            ));
        }
        self.shell.finish().await?;
        Ok(self.core.state().clone())
    }
}
