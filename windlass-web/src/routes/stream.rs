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

use crate::AppState;

pub fn router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/api/v1/stream", axum::routing::get(stream_handler))
        .with_state(state)
}

async fn stream_handler(
    State(app): State<AppState>,
) -> Sse<impl futures_util::Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let current_state = (*app.state.load_full()).clone();
    let snapshot = Observation::StateSnapshot(current_state);

    let initial = stream::once(async move {
        let json = serde_json::to_string(&snapshot).unwrap_or_default();
        Ok::<_, std::convert::Infallible>(SseEvent::default().event("observation").data(json))
    });

    let rx = app.observations.subscribe();
    let live = BroadcastStream::new(rx).filter_map(|msg| async move {
        msg.ok().map(|obs| {
            let json = serde_json::to_string(&obs).unwrap_or_default();
            Ok(SseEvent::default().event("observation").data(json))
        })
    });

    Sse::new(initial.chain(live)).keep_alive(KeepAlive::default())
}
