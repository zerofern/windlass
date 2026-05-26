use tokio::sync::mpsc;

use windlass_db::DbPool;
use windlass_db::actor::PostgresDbActor;
use windlass_db_core::{DbEvent, DbFailure};
use windlass_domain_core::WindlassEvent;

use super::service::{ServiceAction, ServiceCores};

pub(super) fn service_domain_event_channel()
-> (mpsc::Sender<WindlassEvent>, mpsc::Receiver<WindlassEvent>) {
    mpsc::channel(128)
}

pub(super) fn drain_service_events(
    service_cores: &mut ServiceCores,
    service_event_rx: &mut mpsc::Receiver<WindlassEvent>,
    db_pool: &DbPool,
    service_event_tx: &mpsc::Sender<WindlassEvent>,
) {
    while let Ok(event) = service_event_rx.try_recv() {
        for action in service_cores.observe_domain_event(event) {
            dispatch_service_db_action(db_pool, &action, service_event_tx);
        }
    }
}

pub(super) fn dispatch_service_db_action(
    db_pool: &DbPool,
    action: &ServiceAction,
    service_event_tx: &mpsc::Sender<WindlassEvent>,
) {
    if let ServiceAction::Db(command) = action {
        let actor = PostgresDbActor::new(db_pool.clone());
        let command = command.clone();
        let service_event_tx = service_event_tx.clone();
        tokio::spawn(async move {
            let event = actor.handle(command).await;
            if let DbEvent::Failed(error) = event {
                tracing::warn!(
                    operation = %error.operation,
                    "Service domain DB command failed: {}",
                    error.message
                );
                if let Some(event) = db_failure_to_domain_event(error) {
                    let _ = service_event_tx.send(event).await;
                }
            }
        });
    }
}

fn db_failure_to_domain_event(error: DbFailure) -> Option<WindlassEvent> {
    if error.operation == "RecordActivity" {
        return None;
    }
    Some(WindlassEvent::DbFailed {
        operation: error.operation,
        message: error.message,
    })
}

#[cfg(test)]
mod tests {
    use windlass_db_core::DbFailure;
    use windlass_domain_core::WindlassEvent;

    use super::db_failure_to_domain_event;

    #[test]
    fn db_failure_becomes_domain_event() {
        let event = db_failure_to_domain_event(DbFailure {
            operation: "SaveSystemSnapshot".to_string(),
            message: "database unavailable".to_string(),
            retryable: true,
        });

        assert_eq!(
            event,
            Some(WindlassEvent::DbFailed {
                operation: "SaveSystemSnapshot".to_string(),
                message: "database unavailable".to_string(),
            })
        );
    }

    #[test]
    fn activity_log_failure_does_not_recurse() {
        let event = db_failure_to_domain_event(DbFailure {
            operation: "RecordActivity".to_string(),
            message: "database unavailable".to_string(),
            retryable: true,
        });

        assert_eq!(event, None);
    }
}
