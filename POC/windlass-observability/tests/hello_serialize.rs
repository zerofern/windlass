//! Regression probe: a *populated* `HelloSnapshot` must serialize.
//! The SSE route falls back to an empty `data:` payload when
//! `serde_json::to_string` fails, and browsers silently drop
//! data-less SSE events — so a serialization failure here is
//! invisible except as a dashboard that never backfills.

use std::time::Duration;

use chrono::Utc;
use uuid::Uuid;
use windlass_machine::{CoreId, EventCause, ExternalCause, RuntimeTap, StepKind, StepRecordView};
use windlass_observability::{ObservabilityController, SseMessage};
use windlass_types::{HttpExchange, HttpTap};

#[tokio::test]
async fn populated_hello_snapshot_serializes() {
    let ctrl = ObservabilityController::new();

    let step_id = Uuid::new_v4();
    let action_id = Uuid::new_v4();
    let publish_id = Uuid::new_v4();
    ctrl.reserve_step_ids(CoreId::Mam, step_id, &[action_id], &[publish_id]);
    let event = serde_json::json!({"StatusFetched": {"connectable": true}});
    let state = serde_json::json!({"phase": "Ready"});
    let cause = EventCause::External(ExternalCause::Timer {
        name: "MamTimer::KeepAlive",
    });
    ctrl.observed_step(
        CoreId::Mam,
        &StepRecordView {
            step_id,
            core: CoreId::Mam,
            recorded_at: Utc::now(),
            duration: Duration::from_millis(3),
            kind: StepKind::Event,
            event_variant: "StatusFetched",
            event: &event,
            event_cause: &cause,
            state_after: &state,
            action_ids: &[action_id],
            action_variants: &["ScheduleTimer"],
            action_payloads: &[serde_json::json!({"ScheduleTimer": {"timer": "KeepAlive"}})],
            publish_ids: &[publish_id],
            publish_variants: &["Connectable"],
            publish_payloads: &[serde_json::json!({"Connectable": {}})],
            publish_topics: &["Status"],
        },
    );
    ctrl.observed_exchange(
        CoreId::Mam,
        &HttpExchange {
            module: "mam".to_string(),
            method: "GET".to_string(),
            url: "https://example.test/jsonLoad".to_string(),
            request_headers: vec![("cookie".to_string(), "mam_id=secret".to_string())],
            request_body: Some("{}".to_string()),
            response_status: 200,
            response_headers: Vec::new(),
            response_body: "{\"ok\":true}".to_string(),
        },
    );
    // Ring writes flow through the controller's async worker; give it
    // a beat to land before snapshotting.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let hello = ctrl.hello().await;
    assert!(
        !hello.steps.is_empty(),
        "fixture step never reached the ring"
    );
    assert!(
        !hello.http.is_empty(),
        "fixture exchange never reached the ring"
    );

    let serialized = serde_json::to_string(&SseMessage::Hello(hello));
    assert!(
        serialized.is_ok(),
        "populated Hello must serialize: {:?}",
        serialized.err()
    );
}
