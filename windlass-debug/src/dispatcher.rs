use tracing::debug;
use windlass_core::actions::Action;

use crate::{DebugController, PausedOn, stream::action_variant};

/// Wraps action dispatch with debug-mode pause/skip logic.
///
/// The shell passes a plain `execute` callback for the actual I/O spawn.
/// This struct owns only the debug concerns: breakpoints and stepping.
/// No shell types leak into this crate.
pub struct DebugDispatcher {
    debug_ctrl: DebugController,
}

impl DebugDispatcher {
    #[must_use]
    pub const fn new(debug_ctrl: DebugController) -> Self {
        Self { debug_ctrl }
    }

    /// Dispatches each action in order, pausing before those that match an
    /// active breakpoint. `execute` is called (or skipped) for each action.
    pub async fn dispatch(&self, actions: Vec<Action>, mut execute: impl FnMut(Action)) {
        let total = actions.len();

        for (idx, action) in actions.into_iter().enumerate() {
            debug!(?action, "→");

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
    }
}
