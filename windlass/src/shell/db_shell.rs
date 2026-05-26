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
                tokio::spawn(async move {
                    let result = actor.handle(command).await;
                    let _ = event_tx.send(Timed::now(result));
                });
            }
        }
    }
}
