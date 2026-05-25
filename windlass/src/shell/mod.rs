mod actions;
mod compliance;
mod config;
mod dequeue;
mod download;
mod init;
mod service;

use std::collections::HashMap;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::debug;

use windlass_clients::{mam, qbit};
use windlass_core::{actions::Action, events::Event, types::SystemState};
use windlass_db::DbPool;
use windlass_db::actor::PostgresDbActor;
use windlass_db_core::DbEvent;
use windlass_debug::{CausalTx, DebugController, DebugDispatcher, DebugHistory};
use windlass_local::docker;
use windlass_types::WakeupId;

use dequeue::dequeue_debug;
use init::{ShellRuntime, init_shell};
use service::{ServiceAction, service_debug_actions};

/// Entry point for the imperative shell. Bootstraps all infrastructure,
/// then runs the event loop forever.
///
/// `debug_ctrl` and `debug_owned` are created in `main` so the log layer
/// can be registered on the tracing subscriber before the shell starts.
pub async fn run(
    debug_ctrl: DebugController,
    debug_owned: windlass_debug::DebugOwnedPart,
) -> Result<()> {
    let ShellRuntime {
        mut debug_stream,
        docker,
        dependents,
        qbit,
        mam,
        db_pool,
        obs_tx,
        tx,
        vpn_ip_file,
        vpn_port_file,
        data_path,
        mut wakeups,
        mut state,
        mut history,
        mut cmd_rx,
        mut log_rx,
        mut queue_rx,
        mut exchange_rx,
        causal_debug_tx,
        mut causal_rx,
        mut service_cores,
        execute_service_actions,
    } = init_shell(&debug_ctrl, debug_owned).await?;
    let debug_dispatcher = DebugDispatcher::new(debug_ctrl.clone());

    'main: loop {
        drain_channels(
            &mut history,
            &debug_ctrl,
            &mut cmd_rx,
            &mut log_rx,
            &mut exchange_rx,
        );

        let (event, event_id) = if debug_ctrl.is_debug_mode() {
            match dequeue_debug(
                &mut history,
                &mut queue_rx,
                &mut causal_rx,
                &mut cmd_rx,
                &mut log_rx,
                &state,
                &debug_ctrl,
            )
            .await
            {
                None => break 'main,
                Some(v) => v,
            }
        } else {
            match debug_stream.recv().await {
                None => break 'main,
                Some(e) => (e, None),
            }
        };

        debug!(?event, "←");

        let service_actions = service_cores.observe(&event);
        for action in &service_actions {
            dispatch_service_db_action(&db_pool, action);
        }
        let outcome = state.process_event(event, chrono::Utc::now());
        if outcome.state_changed {
            let _ = obs_tx.send(windlass_core::Observation::StateSnapshot(Box::new(
                state.clone(),
            )));
            if !debug_ctrl.is_debug_mode() {
                debug_ctrl.update_latest_state(state.clone());
            }
        }

        let mut ctx = ShellContext {
            docker: &docker,
            qbit: &qbit,
            mam: &mam,
            wakeups: &mut wakeups,
            dependents: &dependents,
            tx: &tx,
            vpn_ip_file: &vpn_ip_file,
            vpn_port_file: &vpn_port_file,
            data_path: &data_path,
            db_pool: &db_pool,
        };

        let legacy_actions =
            legacy_actions_for_service_mode(execute_service_actions, outcome.actions);

        dispatch_event(
            legacy_actions,
            service_actions,
            execute_service_actions,
            event_id,
            &state,
            &mut history,
            &debug_ctrl,
            &debug_dispatcher,
            &causal_debug_tx,
            &tx,
            &mut ctx,
        )
        .await;
    }

    Ok(())
}

fn legacy_actions_for_service_mode(
    execute_service_actions: bool,
    actions: Vec<Action>,
) -> Vec<Action> {
    if !execute_service_actions {
        return actions;
    }
    actions
        .into_iter()
        .filter(|action| !service_replaces_legacy_action(action))
        .collect()
}

const fn service_replaces_legacy_action(action: &Action) -> bool {
    matches!(
        action,
        Action::ReadPortFiles
            | Action::AuthenticateQbit
            | Action::SyncQbitPort(_, _)
            | Action::UpdateMam(_)
            | Action::CheckMamConnectability
            | Action::CheckNewTorrents(_)
            | Action::FetchQbitPreferences(_)
            | Action::ScheduleWakeup(
                WakeupId::QbitAuthRetry
                    | WakeupId::QbitSyncRetry
                    | WakeupId::Heartbeat
                    | WakeupId::RetryPortRead,
                _
            )
    )
}

fn dispatch_service_db_action(db_pool: &DbPool, action: &ServiceAction) {
    match action {
        ServiceAction::Db(command) => {
            let actor = PostgresDbActor::new(db_pool.clone());
            let command = command.clone();
            tokio::spawn(async move {
                let event = actor.handle(command).await;
                if let DbEvent::Failed(error) = event {
                    tracing::warn!(
                        operation = %error.operation,
                        "Service domain DB command failed: {}",
                        error.message
                    );
                }
            });
        }
        ServiceAction::Vpn(_)
        | ServiceAction::Qbit(_)
        | ServiceAction::Mam(_)
        | ServiceAction::ScheduleTimer { .. } => {}
    }
}

fn execute_service_actions_if_enabled(
    enabled: bool,
    actions: Vec<ServiceAction>,
    tx: &mpsc::Sender<Event>,
    ctx: &mut ShellContext<'_>,
) {
    if !enabled {
        return;
    }
    for action in actions {
        let causal = CausalTx::plain(uuid::Uuid::new_v4(), tx.clone());
        ctx.execute_service_action(action, causal);
    }
}

fn drain_channels(
    history: &mut DebugHistory,
    debug_ctrl: &DebugController,
    cmd_rx: &mut mpsc::Receiver<windlass_debug::DebugCommand>,
    log_rx: &mut mpsc::Receiver<windlass_debug::LogEntry>,
    exchange_rx: &mut mpsc::Receiver<(uuid::Uuid, windlass_types::HttpExchange)>,
) {
    while let Ok(cmd) = cmd_rx.try_recv() {
        history.apply_cmd(cmd);
        debug_ctrl.publish(history);
    }
    while let Ok(log) = log_rx.try_recv() {
        history.append_log(log);
        debug_ctrl.publish(history);
    }
    while let Ok((action_id, exchange)) = exchange_rx.try_recv() {
        history.action_http_exchange(action_id, exchange);
        debug_ctrl.publish(history);
    }
}

// Each parameter corresponds to a distinct piece of mutable or shared shell
// state; combining them into a struct would just shift the issue elsewhere.
#[allow(clippy::too_many_arguments)]
async fn dispatch_event(
    outcome_actions: Vec<Action>,
    service_actions: Vec<ServiceAction>,
    execute_service_actions: bool,
    event_id: Option<uuid::Uuid>,
    state: &SystemState,
    history: &mut DebugHistory,
    debug_ctrl: &DebugController,
    debug_dispatcher: &DebugDispatcher,
    causal_debug_tx: &mpsc::Sender<(Event, uuid::Uuid)>,
    tx: &mpsc::Sender<Event>,
    ctx: &mut ShellContext<'_>,
) {
    if let Some(eid) = event_id {
        let plain_tx = tx.clone();
        let mut debug_actions = service_debug_actions(&service_actions);
        debug_actions.extend(outcome_actions.iter().cloned());
        history.actions_ready(&debug_actions);
        debug_ctrl.publish(history);
        execute_service_actions_debug(
            execute_service_actions,
            service_actions,
            eid,
            history,
            tx,
            causal_debug_tx,
            ctx,
        );
        debug_dispatcher
            .dispatch(outcome_actions, |action| {
                let action_id = history.action_started(&action, eid);
                let causal = CausalTx::debug(action_id, causal_debug_tx.clone());
                ctx.execute(action, causal);
            })
            .await;
        history.event_completed(eid, state.clone());
        debug_ctrl.publish(history);
        drop(plain_tx);
    } else {
        let plain_tx = tx.clone();
        execute_service_actions_if_enabled(execute_service_actions, service_actions, tx, ctx);
        debug_dispatcher
            .dispatch(outcome_actions, |action| {
                let causal = CausalTx::plain(uuid::Uuid::new_v4(), plain_tx.clone());
                ctx.execute(action, causal);
            })
            .await;
    }
}

fn execute_service_actions_debug(
    enabled: bool,
    actions: Vec<ServiceAction>,
    parent_event_id: uuid::Uuid,
    history: &mut DebugHistory,
    tx: &mpsc::Sender<Event>,
    causal_debug_tx: &mpsc::Sender<(Event, uuid::Uuid)>,
    ctx: &mut ShellContext<'_>,
) {
    if !enabled {
        return;
    }
    for action in actions {
        let causal = action.debug_action().map_or_else(
            || CausalTx::plain(uuid::Uuid::new_v4(), tx.clone()),
            |debug_action| {
                let action_id = history.action_started(&debug_action, parent_event_id);
                CausalTx::debug(action_id, causal_debug_tx.clone())
            },
        );
        ctx.execute_service_action(action, causal);
    }
}

/// All shared shell state bundled together so action handlers don't need
/// a long argument list.
struct ShellContext<'a> {
    docker: &'a docker::DockerClient,
    qbit: &'a qbit::QbitClient,
    mam: &'a mam::MamClient,
    wakeups: &'a mut HashMap<WakeupId, JoinHandle<()>>,
    dependents: &'a [String],
    tx: &'a mpsc::Sender<Event>,
    vpn_ip_file: &'a str,
    vpn_port_file: &'a str,
    data_path: &'a str,
    db_pool: &'a DbPool,
}

impl ShellContext<'_> {
    /// Executes a single action produced by the Core.
    fn execute(&mut self, action: Action, causal_tx: CausalTx) {
        match action {
            Action::ScheduleWakeup(id, duration) => self.schedule_wakeup(id, duration),
            Action::ReadPortFiles => self.read_port_files(causal_tx),
            Action::FetchAndDumpAllLogs => self.fetch_and_dump_all_logs(causal_tx),
            Action::StopDependentContainers => self.stop_dependent_containers(),
            Action::StartDependentContainers => self.start_dependent_containers(),
            Action::RestartGluetun => self.restart_gluetun(),
            Action::AuthenticateQbit => self.authenticate_qbit(causal_tx),
            Action::SyncQbitPort(cookie, port) => self.sync_qbit_port(cookie, port, causal_tx),
            Action::UpdateMam(ip) => self.update_mam(ip, causal_tx),
            Action::CheckMamConnectability => self.check_mam_connectability(causal_tx),
            Action::CheckDiskSpace => self.check_disk_space(causal_tx),
            Action::CheckNewTorrents(cookie) => self.check_new_torrents(cookie, causal_tx),
            Action::SendAlert {
                priority,
                title,
                body,
            } => self.send_alert(priority, title, body),
            // Compliance
            Action::FetchTorrentDetails(cookie) => {
                self.fetch_torrent_details(cookie, causal_tx);
            }
            Action::FetchQbitPreferences(cookie) => {
                self.fetch_qbit_preferences(cookie, causal_tx);
            }
            Action::PauseTorrent(hash, cookie) => self.pause_torrent(hash, cookie),
            Action::ForceResumeTorrent(hash, cookie) => self.force_resume_torrent(hash, cookie),
            Action::DeleteTorrent(hash, cookie) => self.delete_torrent(hash, cookie),
            Action::SetAllFilesPriority(hash, cookie) => {
                self.set_all_files_priority(hash, cookie);
            }
            Action::UpsertTorrentRecords(records) => self.upsert_torrent_records(records),
            Action::BlacklistMamId(mam_id) => self.blacklist_mam_id(mam_id),
            Action::WriteActivity {
                source,
                action,
                book_id,
                detail,
            } => self.write_activity(source, action, book_id, detail),
            Action::FetchAndAddTorrent { mam_id, cookie } => {
                self.fetch_and_add_torrent(mam_id, cookie, causal_tx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use windlass_types::{AuthCookie, MamTorrentId, WakeupId};

    use super::{Action, legacy_actions_for_service_mode, service_replaces_legacy_action};

    #[test]
    fn service_mode_filters_only_service_orchestration_actions() {
        let actions = vec![
            Action::AuthenticateQbit,
            Action::ScheduleWakeup(WakeupId::QbitAuthRetry, Duration::from_secs(1)),
            Action::ScheduleWakeup(WakeupId::DiskCheck, Duration::from_secs(1)),
            Action::FetchAndAddTorrent {
                mam_id: MamTorrentId(1),
                cookie: AuthCookie("sid".to_string()),
            },
        ];

        let filtered = legacy_actions_for_service_mode(true, actions);

        assert_eq!(filtered.len(), 2);
        assert!(matches!(
            filtered[0],
            Action::ScheduleWakeup(WakeupId::DiskCheck, _)
        ));
        assert!(matches!(filtered[1], Action::FetchAndAddTorrent { .. }));
    }

    #[test]
    fn service_mode_keeps_legacy_actions_when_disabled() {
        let actions = vec![Action::AuthenticateQbit];

        let filtered = legacy_actions_for_service_mode(false, actions);

        assert_eq!(filtered.len(), 1);
        assert!(matches!(filtered[0], Action::AuthenticateQbit));
    }

    #[test]
    fn service_replaces_qbit_preference_fetches() {
        assert!(service_replaces_legacy_action(
            &Action::FetchQbitPreferences(AuthCookie("sid".to_string()))
        ));
    }
}
