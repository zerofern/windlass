use tokio::sync::{mpsc, oneshot};

use windlass_db_core::DbMachine;
use windlass_machine::Command;

use super::service::ServiceAction;

pub(super) fn dispatch_service_db_action(
    db_command_tx: &mpsc::UnboundedSender<Command<DbMachine>>,
    action: &ServiceAction,
) {
    let ServiceAction::Db(command) = action;
    let (reply_tx, _reply_rx) = oneshot::channel();
    let _ = db_command_tx.send((command.clone(), reply_tx));
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use windlass_db_core::ActivityRecord;
    use windlass_db_core::{DbCommand, DbMachine};
    use windlass_machine::Command;

    use super::{ServiceAction, dispatch_service_db_action};

    fn make_channel() -> (
        tokio::sync::mpsc::UnboundedSender<Command<DbMachine>>,
        tokio::sync::mpsc::UnboundedReceiver<Command<DbMachine>>,
    ) {
        tokio::sync::mpsc::unbounded_channel()
    }

    #[test]
    fn db_service_action_is_forwarded_to_db_runtime() {
        let (tx, mut rx) = make_channel();

        let action = ServiceAction::Db(DbCommand::RecordActivity(ActivityRecord {
            at: Utc::now(),
            source: windlass_db_core::ActivitySource::Domain,
            action: "test".to_string(),
            book_id: None,
            detail: None,
            metadata: serde_json::json!({}),
        }));

        dispatch_service_db_action(&tx, &action);

        assert!(
            rx.try_recv().is_ok(),
            "DB command should be forwarded to the DB runtime channel"
        );
    }
}
