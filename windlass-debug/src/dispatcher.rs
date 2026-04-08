use tokio::sync::broadcast;
use tracing::debug;
use windlass_core::{Observation, actions::Action};

use crate::{DebugController, PausedOn, stream::action_variant};

/// Wraps action dispatch with debug-mode pause/skip logic.
///
/// The shell passes a plain `execute` callback for the actual I/O spawn.
/// This struct owns only the debug concerns: breakpoints, stepping, and
/// the `ActionDispatched` observation — no shell types leak into this crate.
pub struct DebugDispatcher {
    debug_ctrl: DebugController,
    obs_tx: broadcast::Sender<Observation>,
}

impl DebugDispatcher {
    #[must_use]
    pub const fn new(debug_ctrl: DebugController, obs_tx: broadcast::Sender<Observation>) -> Self {
        Self { debug_ctrl, obs_tx }
    }

    /// Dispatches each action in order, pausing before those that match an
    /// active breakpoint. `execute` is called (or skipped) for each action.
    pub async fn dispatch(&self, actions: Vec<Action>, mut execute: impl FnMut(Action)) {
        let total = actions.len();
        self.debug_ctrl.set_pending_actions(&actions);

        for (idx, action) in actions.into_iter().enumerate() {
            debug!(?action, "→");

            let _ = self
                .obs_tx
                .send(Observation::ActionDispatched(action.clone()));

            let variant = action_variant(&action);
            if self.debug_ctrl.should_pause_on_action(variant) {
                self.debug_ctrl.set_paused_on(Some(PausedOn::Action {
                    variant,
                    index: idx + 1,
                    of: total,
                }));
                let execute_it = self.debug_ctrl.acquire_step().await;
                self.debug_ctrl.set_paused_on(None);
                if !execute_it {
                    continue;
                }
            }

            execute(action);
        }

        self.debug_ctrl.clear_pending_actions();
    }
}
