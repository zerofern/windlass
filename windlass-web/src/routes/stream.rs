use axum::{
    extract::State,
    response::{
        Sse,
        sse::{Event as SseEvent, KeepAlive},
    },
};
use futures_util::stream::{self, StreamExt};
use tokio_stream::wrappers::BroadcastStream;
use windlass_core::Observation;
use windlass_debug::DebugController;

use crate::AppState;

pub fn router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/api/v1/stream", axum::routing::get(stream_handler))
        .with_state(state)
}

/// Returns the two observations that every new SSE subscriber should receive
/// immediately: the current state snapshot and the current debug-mode flag.
///
/// Extracted so it can be unit-tested without HTTP or database infrastructure.
fn initial_observations(ctrl: &DebugController) -> [Observation; 2] {
    let latest_state = ctrl.snapshot.load().latest_state.clone();
    let debug_mode = ctrl.is_debug_mode();
    [
        Observation::StateSnapshot(Box::new(latest_state)),
        Observation::DebugModeChanged(debug_mode),
    ]
}

fn obs_to_sse(obs: &Observation) -> SseEvent {
    let json = serde_json::to_string(&obs).unwrap_or_default();
    SseEvent::default().event("observation").data(json)
}

async fn stream_handler(
    State(app): State<AppState>,
) -> Sse<impl futures_util::Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    // Subscribe before reading the snapshot. Any state change that arrives
    // between the subscribe call and the snapshot read lands in `live`; the
    // client may receive a duplicate StateSnapshot, which is harmless.
    let rx = app.observations.subscribe();

    let initial = stream::iter(
        initial_observations(&app.debug_ctrl)
            .into_iter()
            .map(|obs| obs_to_sse(&obs))
            .map(Ok),
    );

    let live = BroadcastStream::new(rx).filter_map(|msg| async move {
        msg.ok()
            .map(|obs| Ok::<_, std::convert::Infallible>(obs_to_sse(&obs)))
    });

    Sse::new(initial.chain(live)).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;
    use windlass_core::types::SystemState;
    use windlass_debug::DebugController;

    // ── Unit tests: initial_observations ──────────────────────────────────────

    #[test]
    fn initial_observations_starts_with_state_snapshot() {
        let ctrl = DebugController::new();
        let obs = initial_observations(&ctrl);
        assert!(
            matches!(obs[0], Observation::StateSnapshot(_)),
            "first observation must be StateSnapshot"
        );
    }

    #[test]
    fn initial_observations_second_is_debug_mode_false_by_default() {
        let ctrl = DebugController::new();
        let obs = initial_observations(&ctrl);
        assert!(matches!(obs[1], Observation::DebugModeChanged(false)));
    }

    #[test]
    fn initial_observations_reflects_debug_mode_enabled() {
        let ctrl = DebugController::new();
        ctrl.enable_debug();
        let obs = initial_observations(&ctrl);
        assert!(matches!(obs[1], Observation::DebugModeChanged(true)));
    }

    #[test]
    fn initial_observations_snapshot_reflects_latest_known_state() {
        let ctrl = DebugController::new();
        let custom = SystemState::initial().with_compliance_config(99, 300);
        ctrl.update_latest_state(custom.clone());
        let obs = initial_observations(&ctrl);
        let Observation::StateSnapshot(got) = &obs[0] else {
            panic!("expected StateSnapshot");
        };
        assert_eq!(**got, custom);
    }

    // ── Integration test: HTTP endpoint ──────────────────────────────────────

    /// Collects up to `n` complete SSE events from `text` (separated by blank
    /// lines) and parses their `data:` lines as JSON values.
    fn parse_sse_data(text: &str, n: usize) -> Vec<serde_json::Value> {
        text.split("\n\n")
            .filter(|block| !block.trim().is_empty())
            .take(n)
            .filter_map(|block| {
                block
                    .lines()
                    .find(|l| l.starts_with("data:"))
                    .and_then(|l| serde_json::from_str(l.trim_start_matches("data:").trim()).ok())
            })
            .collect()
    }

    #[tokio::test]
    async fn fresh_subscriber_receives_state_snapshot_then_debug_mode() {
        use futures_util::StreamExt as _;

        let state = crate::test_helpers::test_state().await;
        let app = router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);

        // SSE streams stay open indefinitely; read with a timeout until we
        // have at least two complete events (each ends with a blank line).
        let mut data_stream = response.into_body().into_data_stream();
        let mut text = String::new();

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while parse_sse_data(&text, 2).len() < 2 {
                match data_stream.next().await {
                    Some(Ok(chunk)) => text.push_str(&String::from_utf8_lossy(&chunk)),
                    _ => break,
                }
            }
        })
        .await
        .expect("timed out before receiving two initial SSE events");

        let events = parse_sse_data(&text, 2);
        assert_eq!(
            events.len(),
            2,
            "expected two initial events; got: {text:?}"
        );
        assert_eq!(
            events[0]["type"].as_str(),
            Some("StateSnapshot"),
            "first event must be StateSnapshot"
        );
        assert_eq!(
            events[1]["type"].as_str(),
            Some("DebugModeChanged"),
            "second event must be DebugModeChanged"
        );
    }
}
