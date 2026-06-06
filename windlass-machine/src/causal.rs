//! Task-local action-id propagation for the runtime → shell → HTTP
//! exchange chain.
//!
//! When [`crate::runtime::ServiceRuntime::apply`] dispatches an action,
//! it sets [`CURRENT_ACTION_ID`] to the action's envelope id for the
//! duration of `Shell::dispatch`.  Each shell propagates the task-local
//! into any `tokio::spawn`ed HTTP work via [`scope`] so the eventual
//! `HttpTap::observed_exchange` call inside the HTTP client can read
//! the current id and tag the exchange.
//!
//! Without this thread, the `action_id → step_id` index built by
//! `reserve_step_ids` has no HTTP-side referent — the cross-core
//! "click HTTP row → jump to originating step" affordance breaks.

use std::future::Future;

use tokio::task_local;
use uuid::Uuid;

task_local! {
    /// The id of the action whose dispatch is currently in flight on
    /// this task.  `None` when no dispatch is active (e.g. timer fire,
    /// startup paths).  Read by `HttpTap::observed_exchange` to tag
    /// captured exchanges.
    pub static CURRENT_ACTION_ID: Option<Uuid>;
}

/// Run `fut` with [`CURRENT_ACTION_ID`] set to `Some(id)`.  Use inside
/// `Shell::dispatch` when spawning HTTP work:
///
/// ```ignore
/// tokio::spawn(causal::scope(action_id, async move {
///     let _ = client.do_something().await;
/// }));
/// ```
pub fn scope<F: Future>(
    id: Uuid,
    fut: F,
) -> tokio::task::futures::TaskLocalFuture<Option<Uuid>, F> {
    CURRENT_ACTION_ID.scope(Some(id), fut)
}

/// Capture the calling task's [`CURRENT_ACTION_ID`] and wrap `fut` so
/// the spawned task inherits it.
///
/// Returns the future verbatim if no action id is currently set
/// (e.g. a timer-fire path that has no originating action).
pub fn propagate<F: Future>(fut: F) -> tokio::task::futures::TaskLocalFuture<Option<Uuid>, F> {
    CURRENT_ACTION_ID.scope(current(), fut)
}

/// Convenience wrapper around `tokio::spawn`.
///
/// Propagates [`CURRENT_ACTION_ID`] into spawned shell work so HTTP captures can
/// be tagged with the originating action id.
///
/// ```ignore
/// causal::spawn(async move {
///     let _ = client.do_something().await;  // hook.observed_exchange sees the id
/// });
/// ```
pub fn spawn<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(propagate(fut))
}

/// Read the current action id from the task-local.  Returns `None`
/// when no dispatch is active (e.g. a timer fired and the HTTP work
/// runs outside any `scope`).
#[must_use]
pub fn current() -> Option<Uuid> {
    CURRENT_ACTION_ID.try_with(|id| *id).ok().flatten()
}
