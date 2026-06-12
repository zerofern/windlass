use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use chrono::Utc;
use serde_json::json;
use windlass_db_core::{AlertRecord, DbCommand, DbMachine, DbResponse, SystemSnapshotRecord};
use windlass_docker_core::{DockerMachine, DockerResponse};
use windlass_domain_core::{WindlassAction, WindlassEvent};
use windlass_machine::{Command, EventCause, ExternalCause, KeyedTimers, Shell, Timed};
use windlass_mam_core::{MamMachine, MamResponse};
use windlass_qbit_core::{QbitMachine, QbitResponse};
use windlass_vpn_core::{VpnMachine, VpnResponse};

/// Configuration holding all service command senders the `DomainShell` needs
/// to route domain actions to the appropriate downstream runtimes.
pub struct DomainShellConfig {
    pub db: UnboundedSender<Command<DbMachine>>,
    pub vpn: UnboundedSender<Command<VpnMachine>>,
    pub qbit: UnboundedSender<Command<QbitMachine>>,
    pub mam: UnboundedSender<Command<MamMachine>>,
    pub docker: UnboundedSender<Command<DockerMachine>>,
}

/// Imperative shell for the `WindlassMachine` domain runtime.
///
/// Routes each `WindlassAction` to the appropriate downstream runtime:
/// - `Db(cmd)` → DB command channel (fire-and-forget oneshot).
/// - `Vpn/Qbit/Mam/Docker(cmd)` → respective command channels.
/// - `ScheduleTimer` → tokio sleep that sends `Timed<WindlassEvent::TimerFired>`
///   back through the domain event channel, preserving the scheduled fire time.
pub struct DomainShell {
    db: UnboundedSender<Command<DbMachine>>,
    vpn: UnboundedSender<Command<VpnMachine>>,
    qbit: UnboundedSender<Command<QbitMachine>>,
    mam: UnboundedSender<Command<MamMachine>>,
    docker: UnboundedSender<Command<DockerMachine>>,
    /// Replace-semantics timers: at most one pending sleep per
    /// [`windlass_domain_core::WindlassTimer`] id (see [`KeyedTimers`]).
    timers: KeyedTimers<windlass_domain_core::WindlassTimer>,
}

impl Shell for DomainShell {
    type Config = DomainShellConfig;
    type Event = WindlassEvent;
    type Action = WindlassAction;

    async fn new(config: Self::Config, _event_tx: UnboundedSender<Timed<WindlassEvent>>) -> Self {
        Self {
            db: config.db,
            vpn: config.vpn,
            qbit: config.qbit,
            mam: config.mam,
            docker: config.docker,
            timers: KeyedTimers::new(),
        }
    }

    fn dispatch(
        &mut self,
        action: WindlassAction,
        event_tx: &UnboundedSender<Timed<WindlassEvent>>,
    ) {
        // `dispatch` runs inside the runtime's per-action causal scope,
        // so this is the id of the WindlassAction being routed.  Carried
        // on every forwarded command so the receiving core's command
        // step links back to the domain step that issued it.
        let cause = windlass_machine::causal::current().map_or(
            EventCause::External(ExternalCause::Unknown),
            EventCause::Action,
        );
        match action {
            WindlassAction::Db(cmd) => {
                let (reply_tx, _reply_rx) = oneshot::channel::<DbResponse>();
                let _ = self.db.send((cmd, cause, reply_tx));
            }
            WindlassAction::SaveSystemSnapshot(state) => {
                let (reply_tx, _reply_rx) = oneshot::channel::<DbResponse>();
                let cmd = DbCommand::SaveSystemSnapshot(SystemSnapshotRecord {
                    at: Utc::now(),
                    state: json!(state),
                });
                let _ = self.db.send((cmd, cause, reply_tx));
            }
            WindlassAction::SendAlert {
                priority,
                title,
                body,
            } => {
                let (reply_tx, _reply_rx) = oneshot::channel::<DbResponse>();
                let cmd = DbCommand::RecordAlert(AlertRecord {
                    at: Utc::now(),
                    priority,
                    title,
                    body,
                });
                let _ = self.db.send((cmd, cause, reply_tx));
            }
            WindlassAction::Vpn(cmd) => {
                let (reply_tx, _reply_rx) = oneshot::channel::<VpnResponse>();
                let _ = self.vpn.send((cmd, cause, reply_tx));
            }
            WindlassAction::Qbit(cmd) => {
                let (reply_tx, _reply_rx) = oneshot::channel::<QbitResponse>();
                let _ = self.qbit.send((cmd, cause, reply_tx));
            }
            WindlassAction::Mam(cmd) => {
                let (reply_tx, _reply_rx) = oneshot::channel::<MamResponse>();
                let _ = self.mam.send((cmd, cause, reply_tx));
            }
            WindlassAction::Docker(cmd) => {
                let (reply_tx, _reply_rx) = oneshot::channel::<DockerResponse>();
                let _ = self.docker.send((cmd, cause, reply_tx));
            }
            WindlassAction::ScheduleTimer { timer, after } => {
                self.timers.schedule(
                    timer,
                    timer.name(),
                    after,
                    event_tx,
                    WindlassEvent::TimerFired(timer),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use windlass_machine::causal;
    use windlass_mam_core::MamCommand;
    use windlass_types::VpnPort;

    /// The whole point of the command-cause plumbing: a command the
    /// domain routes to another core must carry the id of the domain
    /// action that produced it (read from the dispatch scope the
    /// runtime establishes), so the receiving core's command step
    /// links back to the originating domain step on the
    /// observability page.
    #[tokio::test]
    async fn routed_commands_carry_the_domain_action_id_as_cause() {
        let (db_tx, _db_rx) = mpsc::unbounded_channel();
        let (vpn_tx, _vpn_rx) = mpsc::unbounded_channel();
        let (qbit_tx, _qbit_rx) = mpsc::unbounded_channel();
        let (mam_tx, mut mam_rx) = mpsc::unbounded_channel();
        let (docker_tx, _docker_rx) = mpsc::unbounded_channel();
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let mut shell = DomainShell::new(
            DomainShellConfig {
                db: db_tx,
                vpn: vpn_tx,
                qbit: qbit_tx,
                mam: mam_tx,
                docker: docker_tx,
            },
            event_tx.clone(),
        )
        .await;

        let port = VpnPort::try_new(51_820).unwrap();
        let action_id = uuid::Uuid::new_v4();
        // The runtime wraps every Shell::dispatch in this scope.
        causal::CURRENT_ACTION_ID.sync_scope(Some(action_id), || {
            shell.dispatch(
                WindlassAction::Mam(MamCommand::EnsureSeedboxPort { port }),
                &event_tx,
            );
        });

        let (cmd, cause, _reply) = mam_rx.try_recv().expect("command forwarded");
        assert!(matches!(cmd, MamCommand::EnsureSeedboxPort { port: p } if p == port));
        assert_eq!(cause, windlass_machine::EventCause::Action(action_id));
    }

    /// Outside any dispatch scope (defensive path only — the runtime
    /// always establishes one) the cause degrades to Unknown rather
    /// than inventing an id.
    #[tokio::test]
    async fn routed_commands_without_scope_fall_back_to_unknown() {
        let (db_tx, _db_rx) = mpsc::unbounded_channel();
        let (vpn_tx, _vpn_rx) = mpsc::unbounded_channel();
        let (qbit_tx, _qbit_rx) = mpsc::unbounded_channel();
        let (mam_tx, mut mam_rx) = mpsc::unbounded_channel();
        let (docker_tx, _docker_rx) = mpsc::unbounded_channel();
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let mut shell = DomainShell::new(
            DomainShellConfig {
                db: db_tx,
                vpn: vpn_tx,
                qbit: qbit_tx,
                mam: mam_tx,
                docker: docker_tx,
            },
            event_tx.clone(),
        )
        .await;

        let port = VpnPort::try_new(51_820).unwrap();
        shell.dispatch(
            WindlassAction::Mam(MamCommand::EnsureSeedboxPort { port }),
            &event_tx,
        );
        let (_cmd, cause, _reply) = mam_rx.try_recv().expect("command forwarded");
        assert_eq!(
            cause,
            windlass_machine::EventCause::External(ExternalCause::Unknown)
        );
    }
}
