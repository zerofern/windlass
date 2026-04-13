use windlass_core::events::Event;
use windlass_core::types::SystemState;
use windlass_debug::{
    DebugCommand, DebugController, DebugHistory, LogEntry, PausedOn, StoredEvent,
};

/// Waits for the next event in debug mode by draining the queue channel and
/// history, pausing on the front event for a step permit.
///
/// Returns `None` if all input channels have closed (shutdown). Otherwise
/// returns `(Event, Some(event_id))` after recording the event as started.
pub(super) async fn dequeue_debug(
    history: &mut DebugHistory,
    queue_rx: &mut tokio::sync::mpsc::Receiver<StoredEvent>,
    causal_rx: &mut tokio::sync::mpsc::Receiver<(Event, uuid::Uuid)>,
    cmd_rx: &mut tokio::sync::mpsc::Receiver<DebugCommand>,
    log_rx: &mut tokio::sync::mpsc::Receiver<LogEntry>,
    state: &SystemState,
    debug_ctrl: &DebugController,
) -> Option<(Event, Option<uuid::Uuid>)> {
    loop {
        // Drain all pending channels before checking the queue.
        while let Ok(stored) = queue_rx.try_recv() {
            history.push_stored_event(stored);
            debug_ctrl.publish(history);
        }
        while let Ok((event, action_id)) = causal_rx.try_recv() {
            let event_id = history.push_causal_event(event, action_id);
            history.action_completed(action_id, Some(event_id));
            debug_ctrl.publish(history);
        }
        while let Ok(cmd) = cmd_rx.try_recv() {
            history.apply_cmd(cmd);
            debug_ctrl.publish(history);
        }
        while let Ok(log) = log_rx.try_recv() {
            history.append_log(log);
            debug_ctrl.publish(history);
        }

        if history.queue_is_empty() {
            // Nothing to process — wait for an event, command, or log.
            tokio::select! {
                stored = queue_rx.recv() => if let Some(s) = stored {
                    history.push_stored_event(s);
                    debug_ctrl.publish(history);
                } else {
                    return None;
                },
                causal = causal_rx.recv() => {
                    if let Some((event, action_id)) = causal {
                        let event_id = history.push_causal_event(event, action_id);
                        history.action_completed(action_id, Some(event_id));
                        debug_ctrl.publish(history);
                    }
                },
                cmd = cmd_rx.recv() => if let Some(c) = cmd {
                    history.apply_cmd(c);
                    debug_ctrl.publish(history);
                } else {
                    return None;
                },
                log = log_rx.recv() => {
                    if let Some(l) = log { history.append_log(l); debug_ctrl.publish(history); }
                },
            }
            continue;
        }

        // Pause on the front event before processing it.
        let front_variant = history.queue_front_variant().unwrap();
        if debug_ctrl.should_pause_on_event(front_variant) {
            debug_ctrl.set_paused_on(Some(PausedOn::Event {
                variant: front_variant,
            }));
            debug_ctrl.publish(history);

            let execute = loop {
                tokio::select! {
                    execute = debug_ctrl.acquire_step() => break execute,
                    stored = queue_rx.recv() => if let Some(s) = stored {
                        history.push_stored_event(s);
                        debug_ctrl.publish(history);
                    } else {
                        debug_ctrl.set_paused_on(None);
                        return None;
                    },
                    causal = causal_rx.recv() => {
                        if let Some((event, action_id)) = causal {
                            let event_id = history.push_causal_event(event, action_id);
                            history.action_completed(action_id, Some(event_id));
                            debug_ctrl.publish(history);
                        }
                    },
                    cmd = cmd_rx.recv() => if let Some(c) = cmd {
                        history.apply_cmd(c);
                        debug_ctrl.publish(history);
                    } else {
                        debug_ctrl.set_paused_on(None);
                        return None;
                    },
                    log = log_rx.recv() => {
                        if let Some(l) = log { history.append_log(l); debug_ctrl.publish(history); }
                    },
                }
            };

            debug_ctrl.set_paused_on(None);

            if !execute {
                // Skip: pop the front event without processing.
                history.pop_queue_front();
                debug_ctrl.publish(history);
                continue;
            }

            // Re-check: the queue may have changed while we waited.
            if history.queue_is_empty() {
                continue;
            }
        }

        // Pop the front event and record it as started.
        let stored = history.pop_queue_front().unwrap();
        let id = stored.id;
        let event = stored.event().clone();
        history.event_started_stored(stored, state.clone());
        debug_ctrl.publish(history);

        return Some((event, Some(id)));
    }
}
