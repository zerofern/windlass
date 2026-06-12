use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use chrono::Utc;
use serde_json::json;
use windlass_db_core::{AlertRecord, DbCommand, DbMachine, DbResponse, SystemSnapshotRecord};
use windlass_docker_core::{DockerMachine, DockerResponse};
use windlass_domain_core::{WindlassAction, WindlassEvent};
use windlass_machine::{Command, KeyedTimers, Shell, Timed};
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
        match action {
            WindlassAction::Db(cmd) => {
                let (reply_tx, _reply_rx) = oneshot::channel::<DbResponse>();
                let _ = self.db.send((cmd, reply_tx));
            }
            WindlassAction::SaveSystemSnapshot(state) => {
                let (reply_tx, _reply_rx) = oneshot::channel::<DbResponse>();
                let cmd = DbCommand::SaveSystemSnapshot(SystemSnapshotRecord {
                    at: Utc::now(),
                    state: json!(state),
                });
                let _ = self.db.send((cmd, reply_tx));
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
                let _ = self.db.send((cmd, reply_tx));
            }
            WindlassAction::Vpn(cmd) => {
                let (reply_tx, _reply_rx) = oneshot::channel::<VpnResponse>();
                let _ = self.vpn.send((cmd, reply_tx));
            }
            WindlassAction::Qbit(cmd) => {
                let (reply_tx, _reply_rx) = oneshot::channel::<QbitResponse>();
                let _ = self.qbit.send((cmd, reply_tx));
            }
            WindlassAction::Mam(cmd) => {
                let (reply_tx, _reply_rx) = oneshot::channel::<MamResponse>();
                let _ = self.mam.send((cmd, reply_tx));
            }
            WindlassAction::Docker(cmd) => {
                let (reply_tx, _reply_rx) = oneshot::channel::<DockerResponse>();
                let _ = self.docker.send((cmd, reply_tx));
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
