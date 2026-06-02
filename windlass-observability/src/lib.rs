#![warn(clippy::all, clippy::pedantic, clippy::nursery)]
//! The live observability backend.
//!
//! The trait surface ([`RuntimeTap`], [`CoreId`], [`CoreStatus`],
//! [`StepKind`], the gate/step views, [`NullRuntimeTap`]) lives in
//! `windlass-machine::tap` so [`windlass_machine::ServiceRuntime`] can
//! call into it without a circular dependency.  This crate provides
//! the live implementation: [`ObservabilityController`], which holds
//! per-core pause flags, step semaphores, current per-core
//! [`CoreStatus`], the per-core step-record rings, the cross-core
//! HTTP exchange ring, the backward-causal indices, and the SSE
//! broadcast channel.

pub mod ring;
pub mod sse;
pub mod stored;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::{Mutex, Semaphore, broadcast};
use uuid::Uuid;
use windlass_machine::{
    CoreId, CoreStatus, EventGateView, OutcomeGateView, RuntimeTap, StepRecordView,
};
use windlass_types::{HttpAnomaly, HttpExchange, HttpRequestView, HttpTap};

pub use ring::{
    HTTP_EXCHANGE_BYTES_TOTAL, HTTP_EXCHANGES_TOTAL, HttpExchangeRing, MAX_REQUEST_BODY_BYTES,
    MAX_RESPONSE_BODY_BYTES, STEP_RECORD_BYTES_PER_CORE, STEP_RECORDS_PER_CORE, StepRecordRing,
};
pub use sse::{
    Breakpoint, CoreCounters, EvictedIds, HelloSnapshot, HttpCounters, LossCounters, SseMessage,
    StoredLogLine,
};
pub use stored::{
    BodyCapture, BodyKind, StoredAction, StoredEventCause, StoredExternalCause, StoredHttpExchange,
    StoredPublish, StoredStepRecord,
};

/// SSE protocol version emitted in `HelloSnapshot.protocol_version`.
pub const SSE_PROTOCOL_VERSION: u32 = 1;

/// Default SSE broadcast capacity.  Slow clients miss messages
/// (drop-oldest); the runtime never backpressures (EC-5).
const SSE_BROADCAST_CAPACITY: usize = 1024;

// ── ObservabilityController ───────────────────────────────────────────────────

/// The live observability backend.
///
/// Hold-and-clone the returned `Arc<ObservabilityController>` into every
/// [`windlass_machine::ServiceRuntime`] that should be observable;
/// `pause` / `resume` / `step` from any HTTP handler or test harness
/// to drive cross-core gating.  Subscribe to [`Self::subscribe`] for
/// the SSE event stream.
pub struct ObservabilityController {
    cores: [CoreState; 7],
    /// Cross-core HTTP exchange ring.
    http_ring: Mutex<HttpExchangeRing>,
    /// `action_id → (core, step_id)` index for backward-causal lookups.
    /// EC-3: cleaned on every eviction.
    action_index: Mutex<HashMap<Uuid, (CoreId, Uuid)>>,
    /// `publish_id → (core, step_id)` index.
    publish_index: Mutex<HashMap<Uuid, (CoreId, Uuid)>>,
    /// Per-core + cross-core loss counters.
    loss: Mutex<LossCounters>,
    /// SSE broadcast.  Subscribers receive every record / status /
    /// eviction / loss update.
    sse_tx: broadcast::Sender<SseMessage>,
}

struct CoreState {
    paused: AtomicBool,
    step_permits: Semaphore,
    status: ArcSwap<CoreStatus>,
    /// Per-core step-record ring.
    ring: Mutex<StepRecordRing>,
}

impl CoreState {
    fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
            step_permits: Semaphore::new(0),
            status: ArcSwap::from_pointee(CoreStatus::Running),
            ring: Mutex::new(StepRecordRing::new(
                STEP_RECORDS_PER_CORE,
                STEP_RECORD_BYTES_PER_CORE,
            )),
        }
    }
}

impl ObservabilityController {
    /// Fresh controller with all seven cores running.  Constructs
    /// empty rings sized by the compile-time §37pre B7 budgets.
    #[must_use]
    pub fn new() -> Arc<Self> {
        let (sse_tx, _) = broadcast::channel(SSE_BROADCAST_CAPACITY);
        Arc::new(Self {
            cores: std::array::from_fn(|_| CoreState::new()),
            http_ring: Mutex::new(HttpExchangeRing::new(
                HTTP_EXCHANGES_TOTAL,
                HTTP_EXCHANGE_BYTES_TOTAL,
            )),
            action_index: Mutex::new(HashMap::new()),
            publish_index: Mutex::new(HashMap::new()),
            loss: Mutex::new(LossCounters::default()),
            sse_tx,
        })
    }

    fn core(&self, id: CoreId) -> &CoreState {
        &self.cores[id as usize]
    }

    /// Subscribe to the SSE event stream.  Drops oldest messages on
    /// slow consumers; never backpressures the runtime (EC-5).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<SseMessage> {
        self.sse_tx.subscribe()
    }

    fn broadcast(&self, msg: SseMessage) {
        // Sender::send returns Err when there are no active receivers
        // — that is normal (no /observability page open).  Drop.
        let _ = self.sse_tx.send(msg);
    }

    // ── Pause / step controls ─────────────────────────────────────────────────

    pub fn pause(&self, id: CoreId) {
        let c = self.core(id);
        if !c.paused.swap(true, Ordering::SeqCst) {
            c.status.store(Arc::new(CoreStatus::PauseRequested));
            self.broadcast(SseMessage::CoreStatus {
                core: id,
                status: CoreStatus::PauseRequested,
            });
        }
    }

    pub fn pause_all(&self) {
        for id in CoreId::all() {
            self.pause(id);
        }
    }

    pub fn resume(&self, id: CoreId) {
        let c = self.core(id);
        if c.paused.swap(false, Ordering::SeqCst) {
            c.step_permits.add_permits(1);
            c.status.store(Arc::new(CoreStatus::Running));
            self.broadcast(SseMessage::CoreStatus {
                core: id,
                status: CoreStatus::Running,
            });
        }
    }

    pub fn resume_all(&self) {
        for id in CoreId::all() {
            self.resume(id);
        }
    }

    pub fn step(&self, id: CoreId) {
        let c = self.core(id);
        c.step_permits.add_permits(1);
        c.status.store(Arc::new(CoreStatus::Stepping));
        self.broadcast(SseMessage::CoreStatus {
            core: id,
            status: CoreStatus::Stepping,
        });
    }

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

    /// Snapshot of the current loss counters.  Used by the SSE Hello
    /// payload and operator UI badges.
    pub async fn loss_snapshot(&self) -> LossCounters {
        self.loss.lock().await.clone()
    }

    // ── Internal: ring + index maintenance ────────────────────────────────────

    async fn push_step_record(&self, core_id: CoreId, record: StoredStepRecord) {
        let evicted = {
            let mut ring = self.core(core_id).ring.lock().await;
            ring.push(record.clone())
        };

        // Register the new record's IDs.
        {
            let mut a_idx = self.action_index.lock().await;
            for action in &record.actions {
                a_idx.insert(action.action_id, (core_id, record.step_id));
            }
        }
        {
            let mut p_idx = self.publish_index.lock().await;
            for publish in &record.publishes {
                p_idx.insert(publish.publish_id, (core_id, record.step_id));
            }
        }

        // EC-3: every evicted step's action_ids/publish_ids leave the
        // indices, and the frontend mirror gets an Evicted message.
        if !evicted.is_empty() {
            let mut evicted_ids = EvictedIds::default();
            let mut a_idx = self.action_index.lock().await;
            let mut p_idx = self.publish_index.lock().await;
            for old in &evicted {
                evicted_ids.step_ids.push(old.step_id);
                for action in &old.actions {
                    a_idx.remove(&action.action_id);
                    evicted_ids.action_ids.push(action.action_id);
                }
                for publish in &old.publishes {
                    p_idx.remove(&publish.publish_id);
                    evicted_ids.publish_ids.push(publish.publish_id);
                }
            }
            drop(a_idx);
            drop(p_idx);

            // Advance the per-core dropped counter for the evicted steps.
            {
                let mut loss = self.loss.lock().await;
                let counters = loss.core_mut(core_id);
                let dropped = u64::try_from(evicted.len()).unwrap_or(u64::MAX);
                counters.dropped_steps = counters.dropped_steps.saturating_add(dropped);
                let loss_msg = loss.clone();
                drop(loss);
                self.broadcast(SseMessage::Loss(loss_msg));
            }

            self.broadcast(SseMessage::Evicted(evicted_ids));
        }

        self.broadcast(SseMessage::Step(record));
    }

    async fn push_http_exchange(&self, exchange: StoredHttpExchange) {
        let evicted = {
            let mut ring = self.http_ring.lock().await;
            ring.push(exchange.clone())
        };

        if !evicted.is_empty() {
            let dropped = u64::try_from(evicted.len()).unwrap_or(u64::MAX);
            let mut loss = self.loss.lock().await;
            loss.http.dropped_exchanges = loss.http.dropped_exchanges.saturating_add(dropped);
            let loss_msg = loss.clone();
            drop(loss);
            self.broadcast(SseMessage::Loss(loss_msg));
        }

        self.broadcast(SseMessage::HttpExchange(exchange));
    }

    /// Build a HelloSnapshot containing every ring's current contents
    /// + per-core statuses + loss counters.  Sent to a fresh SSE
    /// subscriber so the frontend can hydrate its local store from one
    /// payload (§37pre B9).
    pub async fn hello(&self) -> HelloSnapshot {
        let mut cores = Vec::with_capacity(CoreId::all().len());
        let mut steps = Vec::new();
        for id in CoreId::all() {
            cores.push((id, (*self.core(id).status.load_full()).clone()));
            let ring = self.core(id).ring.lock().await;
            steps.extend(ring.iter().cloned());
        }
        let http: Vec<StoredHttpExchange> = self.http_ring.lock().await.iter().cloned().collect();
        let loss = self.loss.lock().await.clone();
        HelloSnapshot {
            protocol_version: SSE_PROTOCOL_VERSION,
            cores,
            steps,
            http,
            logs: Vec::new(), // §37 follow-up: hook DebugLogLayer → StoredLogLine
            loss,
            active_breakpoints: Vec::new(), // §37g
        }
    }
}

// ── RuntimeTap impl ───────────────────────────────────────────────────────────

#[async_trait]
impl RuntimeTap for ObservabilityController {
    async fn gate_event(&self, core: CoreId, view: &EventGateView<'_>) {
        let c = self.core(core);
        if !c.paused.load(Ordering::SeqCst) {
            return;
        }
        let parked = CoreStatus::ParkedAtEvent {
            variant: view.variant.to_owned(),
            since: Utc::now(),
        };
        c.status.store(Arc::new(parked.clone()));
        self.broadcast(SseMessage::CoreStatus {
            core,
            status: parked,
        });
        if let Ok(p) = c.step_permits.acquire().await {
            p.forget();
        }
        c.status.store(Arc::new(CoreStatus::Running));
        self.broadcast(SseMessage::CoreStatus {
            core,
            status: CoreStatus::Running,
        });
    }

    async fn gate_outcome(&self, core: CoreId, view: &OutcomeGateView<'_>) {
        let c = self.core(core);
        if !c.paused.load(Ordering::SeqCst) {
            return;
        }
        let parked = CoreStatus::ParkedAtOutcome {
            source_variant: view.source_event_variant.to_owned(),
            since: Utc::now(),
        };
        c.status.store(Arc::new(parked.clone()));
        self.broadcast(SseMessage::CoreStatus {
            core,
            status: parked,
        });
        if let Ok(p) = c.step_permits.acquire().await {
            p.forget();
        }
        c.status.store(Arc::new(CoreStatus::Running));
        self.broadcast(SseMessage::CoreStatus {
            core,
            status: CoreStatus::Running,
        });
    }

    fn reserve_step_ids(
        &self,
        core: CoreId,
        step_id: Uuid,
        action_ids: &[Uuid],
        publish_ids: &[Uuid],
    ) {
        // EC-8: lightweight, synchronous, non-fallible.  Two
        // try_lock'd hashmap inserts; counter increments on contention.
        let Ok(mut a_idx) = self.action_index.try_lock() else {
            // Contention — record and move on.  EC-8: never block.
            if let Ok(mut loss) = self.loss.try_lock() {
                loss.core_mut(core).reservation_failures =
                    loss.core_mut(core).reservation_failures.saturating_add(1);
            }
            return;
        };
        let Ok(mut p_idx) = self.publish_index.try_lock() else {
            if let Ok(mut loss) = self.loss.try_lock() {
                loss.core_mut(core).reservation_failures =
                    loss.core_mut(core).reservation_failures.saturating_add(1);
            }
            return;
        };
        for id in action_ids {
            a_idx.insert(*id, (core, step_id));
        }
        for id in publish_ids {
            p_idx.insert(*id, (core, step_id));
        }
    }

    fn observed_step(&self, core: CoreId, view: &StepRecordView<'_>) {
        // EC-1: must not block.  The view is borrowed; copy into an
        // owned record and hand off to the async tasks via a spawn.
        // §37d shipped the call site; §37f fills in the storage.
        let record = StoredStepRecord {
            step_id: view.step_id,
            core,
            recorded_at: view.recorded_at,
            duration_ms: StoredStepRecord::duration_ms_from(view.duration),
            kind: view.kind.clone(),
            event_variant: view.event_variant.to_owned(),
            event: view.event.clone(),
            event_cause: view.event_cause.into(),
            state_after: view.state_after.clone(),
            actions: view
                .action_ids
                .iter()
                .zip(view.action_variants.iter())
                .map(|(id, variant)| StoredAction {
                    action_id: *id,
                    variant: (*variant).to_owned(),
                    payload: serde_json::Value::Null,
                })
                .collect(),
            publishes: view
                .publish_ids
                .iter()
                .zip(view.publish_variants.iter())
                .map(|(id, variant)| StoredPublish {
                    publish_id: *id,
                    topic: String::new(),
                    variant: (*variant).to_owned(),
                    payload: serde_json::Value::Null,
                })
                .collect(),
        };

        // Push into the async-locked ring on a detached task so this
        // method stays non-blocking (EC-1).  The internal channel /
        // worker pattern described in EC-5 is over-engineered while
        // tokio::spawn is available; if we ever need backpressure
        // accounting we can introduce a bounded channel here.
        let this = self.clone_arc();
        tokio::spawn(async move {
            this.push_step_record(core, record).await;
        });
    }
}

impl ObservabilityController {
    /// Internal helper: get an `Arc<Self>` to pass into spawned tasks.
    /// Cheap because the outer storage is a single allocation reached
    /// via `Arc::clone`.
    fn clone_arc(&self) -> Arc<Self> {
        // Safety net: the controller is always held via Arc by
        // construction.  This trick obtains a fresh Arc to it.
        // SAFETY:  Manufactured via the `Arc::increment_strong_count` pattern.
        unsafe {
            let raw: *const Self = self;
            Arc::increment_strong_count(raw);
            Arc::from_raw(raw)
        }
    }
}

// ── HttpTap impl ──────────────────────────────────────────────────────────────

#[async_trait]
impl HttpTap for ObservabilityController {
    async fn gate_request(&self, core: CoreId, view: &HttpRequestView<'_>) {
        let c = self.core(core);
        if !c.paused.load(Ordering::SeqCst) {
            return;
        }
        let parked = CoreStatus::ParkedAtHttp {
            method: view.method.to_owned(),
            url: view.url.to_owned(),
            since: Utc::now(),
        };
        c.status.store(Arc::new(parked.clone()));
        self.broadcast(SseMessage::CoreStatus {
            core,
            status: parked,
        });
        if let Ok(p) = c.step_permits.acquire().await {
            p.forget();
        }
        c.status.store(Arc::new(CoreStatus::Running));
        self.broadcast(SseMessage::CoreStatus {
            core,
            status: CoreStatus::Running,
        });
    }

    fn observed_exchange(&self, core: CoreId, exchange: &HttpExchange) {
        // Enforce request/response body byte budgets at capture time.
        let (request_body, request_truncated) = exchange.request_body.as_deref().map_or_else(
            || (BodyCapture::None, false),
            |body| BodyCapture::from_text(body, MAX_REQUEST_BODY_BYTES),
        );
        let (response_body, response_truncated) =
            BodyCapture::from_text(&exchange.response_body, MAX_RESPONSE_BODY_BYTES);

        let stored = StoredHttpExchange {
            exchange_id: Uuid::new_v4(),
            action_id: None, // §37 follow-up: read from CausalTx task-local
            core,
            at: Utc::now(),
            method: exchange.method.clone(),
            url: exchange.url.clone(),
            request_body,
            response_status: exchange.response_status,
            response_body,
            duration_ms: 0,
        };

        let this = self.clone_arc();
        tokio::spawn(async move {
            if request_truncated || response_truncated {
                let mut loss = this.loss.lock().await;
                if request_truncated {
                    loss.http.truncated_request_bodies =
                        loss.http.truncated_request_bodies.saturating_add(1);
                }
                if response_truncated {
                    loss.http.truncated_response_bodies =
                        loss.http.truncated_response_bodies.saturating_add(1);
                }
                let loss_msg = loss.clone();
                drop(loss);
                this.broadcast(SseMessage::Loss(loss_msg));
            }
            this.push_http_exchange(stored).await;
        });
    }

    fn signal_anomaly(&self, core: CoreId, anomaly: HttpAnomaly) {
        // P7 wiring: translate the anomaly into a per-core pause so
        // the next gate_request from the same client parks the
        // offending request before client.execute(req) is invoked.
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
        assert!(!ctrl.is_paused(CoreId::Qbit));

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

    #[tokio::test]
    async fn hello_contains_empty_rings_and_running_cores() {
        let ctrl = ObservabilityController::new();
        let h = ctrl.hello().await;
        assert_eq!(h.protocol_version, SSE_PROTOCOL_VERSION);
        assert_eq!(h.cores.len(), 7);
        for (_id, status) in &h.cores {
            assert!(matches!(status, CoreStatus::Running));
        }
        assert!(h.steps.is_empty());
        assert!(h.http.is_empty());
        assert!(h.loss.is_empty());
    }
}
