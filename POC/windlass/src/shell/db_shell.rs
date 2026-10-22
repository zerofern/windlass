use tokio::sync::mpsc::UnboundedSender;
use windlass_db::DbPool;
use windlass_db::actor::PostgresDbActor;
use windlass_db_core::{DbAction, DbEvent};
use windlass_machine::{Shell, Timed};

pub struct DbShell {
    actor: PostgresDbActor,
}

impl Shell for DbShell {
    type Config = DbPool;
    type Event = DbEvent;
    type Action = DbAction;

    async fn new(pool: Self::Config, _event_tx: UnboundedSender<Timed<Self::Event>>) -> Self {
        Self {
            actor: PostgresDbActor::new(pool),
        }
    }

    fn dispatch(&mut self, action: Self::Action, event_tx: &UnboundedSender<Timed<Self::Event>>) {
        match action {
            DbAction::Execute(command) => {
                let actor = self.actor.clone();
                let event_tx = event_tx.clone();
                windlass_machine::causal::spawn(async move {
                    let result = actor.handle(command).await;
                    if let DbEvent::Failed(ref f) = result {
                        // Surface DB failures at WARN — silent failures
                        // hid the torrents_state_valid CHECK violation
                        // that turned out to be a real qBit-state
                        // mapping bug (fixed in actor::torrent_state_str).
                        tracing::warn!("DB operation {} failed: {}", f.operation, f.message);
                    }
                    let _ = event_tx.send(Timed::from_dispatch(std::time::Instant::now(), result));
                });
            }
        }
    }
}
