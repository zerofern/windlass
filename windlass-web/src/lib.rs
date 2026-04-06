#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

mod app_state;
mod routes;

pub use app_state::AppState;

/// Builds the application router with all API routes attached.
#[must_use = "pass to axum::serve"]
pub fn router(state: AppState) -> axum::Router {
    routes::router(state)
}
