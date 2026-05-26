use tokio::sync::{mpsc, oneshot};

use windlass_db_core::DbMachine;
use windlass_machine::Command;

use super::service::ServiceAction;

pub(super) fn dispatch_service_db_action(
    db_command_tx: &mpsc::UnboundedSender<Command<DbMachine>>,
    action: &ServiceAction,
) {
    if let ServiceAction::Db(command) = action {
        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = db_command_tx.send((command.clone(), reply_tx));
    }
}

#[cfg(test)]
mod tests {
    use windlass_db_core::{DbCommand, DbFailure, DbMachine};
    use windlass_machine::Command;
    use windlass_db_core::ActivityRecord;
    use chrono::Utc;
    use serde_json::json;

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
            metadata: json!({}),
        }));

        dispatch_service_db_action(&tx, &action);

        assert!(
            rx.try_recv().is_ok(),
            "DB command should be forwarded to the DB runtime channel"
        );
    }

    #[test]
    fn non_db_service_action_does_not_send_to_db_runtime() {
        use std::time::Duration;
        use windlass_domain_core::WindlassTimer;

        let (tx, mut rx) = make_channel();

        let action = ServiceAction::ScheduleTimer {
            timer: WindlassTimer::Snapshot,
            after: Duration::from_secs(60),
        };

        dispatch_service_db_action(&tx, &action);

        assert!(
            rx.try_recv().is_err(),
            "non-DB action must not touch the DB runtime channel"
        );
    }
}
