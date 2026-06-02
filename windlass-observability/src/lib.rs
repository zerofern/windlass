#![warn(clippy::all, clippy::pedantic, clippy::nursery)]
//! The live observability backend.
//!
//! The trait surface ([`RuntimeTap`], [`CoreId`], [`CoreStatus`],
//! [`StepKind`], the gate/step views, [`NullRuntimeTap`]) lives in
//! `windlass-machine::tap` so [`windlass_machine::ServiceRuntime`] can
//! call into it without a circular dependency. This crate provides the
//! live implementation: [`ObservabilityController`], which holds per-
//! core pause flags, step semaphores, and the current per-core
//! [`CoreStatus`].
//!
//! §37d ships the skeletal controller. The bounded per-core
//! `StoredStepRecord` rings, the cross-core HTTP ring, the
//! `action_id → step_id` / `publish_id → step_id` indices, and the
//! `SseMessage` envelope land in §37f / §37g.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::Semaphore;
use uuid::Uuid;
use windlass_machine::{
    CoreId, CoreStatus, EventGateView, OutcomeGateView, RuntimeTap, StepRecordView,
};
use windlass_types::{HttpAnomaly, HttpExchange, HttpRequestView, HttpTap};

// ── ObservabilityController ───────────────────────────────────────────────────

/// The live observability backend.
///
/// Hold-and-clone the returned `Arc<ObservabilityController>` into every
/// [`windlass_machine::ServiceRuntime`] that should be observable;
/// `pause` / `resume` / `step` from any HTTP handler or test harness to
/// drive cross-core gating.
pub struct ObservabilityController {
    cores: [CoreState; 7],
}

struct CoreState {
    paused: AtomicBool,
    step_permits: Semaphore,
    status: ArcSwap<CoreStatus>,
}

impl CoreState {
    fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
            step_permits: Semaphore::new(0),
            status: ArcSwap::from_pointee(CoreStatus::Running),
        }
    }
}

impl ObservabilityController {
    /// Fresh controller with all seven cores running.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cores: std::array::from_fn(|_| CoreState::new()),
        })
    }

    fn core(&self, id: CoreId) -> &CoreState {
        &self.cores[id as usize]
    }

    /// Set this core's pause flag. Next gate it hits parks until
    /// [`Self::step`] grants a permit.
    pub fn pause(&self, id: CoreId) {
        let c = self.core(id);
        if !c.paused.swap(true, Ordering::SeqCst) {
            c.status.store(Arc::new(CoreStatus::PauseRequested));
        }
    }

    pub fn pause_all(&self) {
        for id in CoreId::all() {
            self.pause(id);
        }
    }

    /// Clear this core's pause flag; release any task currently parked.
    pub fn resume(&self, id: CoreId) {
        let c = self.core(id);
        if c.paused.swap(false, Ordering::SeqCst) {
            c.step_permits.add_permits(1);
            c.status.store(Arc::new(CoreStatus::Running));
        }
    }

    pub fn resume_all(&self) {
        for id in CoreId::all() {
            self.resume(id);
        }
    }

    /// Grant one step permit. The next gate-acquire consumes it; the
    /// core stays paused after passing through.
    pub fn step(&self, id: CoreId) {
        let c = self.core(id);
        c.step_permits.add_permits(1);
        c.status.store(Arc::new(CoreStatus::Stepping));
    }

    /// Grant one step permit to every currently-paused core.
    pub fn step_all(&self) {
        for id in CoreId::all() {
            if self.core(id).paused.load(Ordering::SeqCst) {
                self.step(id);
            }
        }
    }

    #[must_use]
    pub fn status(&self, id: CoreId) -> Arc<CoreStatus> {
        self.core(id).status.load_full()
    }

    #[must_use]
    pub fn is_paused(&self, id: CoreId) -> bool {
        self.core(id).paused.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl RuntimeTap for ObservabilityController {
    async fn gate_event(&self, core: CoreId, view: &EventGateView<'_>) {
        let c = self.core(core);
        if !c.paused.load(Ordering::SeqCst) {
            return;
        }
        c.status.store(Arc::new(CoreStatus::ParkedAtEvent {
            variant: view.variant.to_owned(),
            since: Utc::now(),
        }));
        if let Ok(p) = c.step_permits.acquire().await {
            p.forget();
        }
        c.status.store(Arc::new(CoreStatus::Running));
    }

    async fn gate_outcome(&self, core: CoreId, view: &OutcomeGateView<'_>) {
        let c = self.core(core);
        if !c.paused.load(Ordering::SeqCst) {
            return;
        }
        c.status.store(Arc::new(CoreStatus::ParkedAtOutcome {
            source_variant: view.source_event_variant.to_owned(),
            since: Utc::now(),
        }));
        if let Ok(p) = c.step_permits.acquire().await {
            p.forget();
        }
        c.status.store(Arc::new(CoreStatus::Running));
    }

    fn reserve_step_ids(
        &self,
        _core: CoreId,
        _step_id: Uuid,
        _action_ids: &[Uuid],
        _publish_ids: &[Uuid],
    ) {
        // §37f attaches the actual indices. §37d ships the call site
        // for EC-8 / acceptance-test #5 to exercise.
    }

    fn observed_step(&self, _core: CoreId, _view: &StepRecordView<'_>) {
        // §37f attaches the per-core StepRecord ring + SSE broadcast.
    }
}

#[async_trait]
impl HttpTap for ObservabilityController {
    async fn gate_request(&self, core: CoreId, view: &HttpRequestView<'_>) {
        let c = self.core(core);
        if !c.paused.load(Ordering::SeqCst) {
            return;
        }
        c.status.store(Arc::new(CoreStatus::ParkedAtHttp {
            method: view.method.to_owned(),
            url: view.url.to_owned(),
            since: Utc::now(),
        }));
        if let Ok(p) = c.step_permits.acquire().await {
            p.forget();
        }
        c.status.store(Arc::new(CoreStatus::Running));
    }

    fn observed_exchange(&self, _core: CoreId, _exchange: &HttpExchange) {
        // §37f attaches the cross-core HTTP exchange ring + SSE
        // broadcast.  §37e ships the call site so clients can be
        // wired now and §37f only fills in the storage.
    }

    fn signal_anomaly(&self, core: CoreId, anomaly: HttpAnomaly) {
        // Translate the anomaly into a per-core pause so the next
        // `gate_request` call from the same client parks the
        // offending request before `client.execute(req)` is invoked.
        //
        // This is the §37e wiring for the MAM rate-limit guardrail
        // (P7 in the redesign doc): the bad request never leaves the
        // host.  The operator sees the parked request on the
        // ParkedAtHttp status.
        match anomaly {
            HttpAnomaly::RateLimitViolation { reason: _ } => {
                self.pause(core);
            }
        }
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use windlass_machine::{EventCause, ExternalCause};

    use super::*;

    #[test]
    fn controller_pause_resume_round_trip() {
        let c = ObservabilityController::new();
        assert!(!c.is_paused(CoreId::Vpn));
        c.pause(CoreId::Vpn);
        assert!(c.is_paused(CoreId::Vpn));
        c.resume(CoreId::Vpn);
        assert!(!c.is_paused(CoreId::Vpn));
    }

    #[test]
    fn pause_all_pauses_every_core() {
        let c = ObservabilityController::new();
        c.pause_all();
        for id in CoreId::all() {
            assert!(c.is_paused(id), "core {id} should be paused");
        }
    }

    #[tokio::test]
    async fn gate_event_returns_immediately_when_not_paused() {
        let ctrl = ObservabilityController::new();
        let v = serde_json::Value::Null;
        let cause = EventCause::External(ExternalCause::Init);
        ctrl.gate_event(
            CoreId::Vpn,
            &EventGateView {
                variant: "Init",
                cause: &cause,
                event: &v,
            },
        )
        .await;
    }

    #[tokio::test]
    async fn gate_event_parks_when_paused_and_releases_on_step() {
        let ctrl = ObservabilityController::new();
        ctrl.pause(CoreId::Vpn);
        let v = serde_json::Value::Null;
        let cause = EventCause::External(ExternalCause::Init);

        let ctrl2 = ctrl.clone();
        let handle = tokio::spawn(async move {
            ctrl2
                .gate_event(
                    CoreId::Vpn,
                    &EventGateView {
                        variant: "Init",
                        cause: &cause,
                        event: &v,
                    },
                )
                .await;
        });

        tokio::time::sleep(Duration::from_millis(20)).await;
        ctrl.step(CoreId::Vpn);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn signal_rate_limit_violation_flips_pause_flag() {
        // §37e P7: HTTP tap signal_anomaly translates into a per-core
        // pause flip, so the next gate_request from the same client
        // parks the offending request before it is sent.
        use windlass_types::{HttpAnomaly, HttpRequestView, HttpTap};
        let ctrl = ObservabilityController::new();
        assert!(!ctrl.is_paused(CoreId::Mam));

        ctrl.signal_anomaly(
            CoreId::Mam,
            HttpAnomaly::RateLimitViolation {
                reason: "test".into(),
            },
        );
        assert!(ctrl.is_paused(CoreId::Mam));
        // Other cores untouched.
        assert!(!ctrl.is_paused(CoreId::Qbit));

        // Confirm gate_request now parks for MAM.
        let ctrl2 = ctrl.clone();
        let parked = tokio::spawn(async move {
            ctrl2
                .gate_request(
                    CoreId::Mam,
                    &HttpRequestView {
                        method: "GET",
                        url: "https://example/test",
                        body: None,
                    },
                )
                .await;
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!parked.is_finished(), "gate_request should have parked");
        ctrl.step(CoreId::Mam);
        parked.await.unwrap();
    }
}
