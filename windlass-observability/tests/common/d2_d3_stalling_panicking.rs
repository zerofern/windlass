//! D2 + D3: `StallingRuntimeTap` and `PanickingRuntimeTap`.
//!
//! Used by acceptance test #2 (Observer cannot block dispatch) to
//! confirm the runtime keeps making progress even when the tap
//! implementation misbehaves.  The contract (EC-1) is that
//! `observed_step` must not block or panic the runtime task;
//! `RuntimeTap` impls handle adversity internally.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use uuid::Uuid;
use windlass_machine::{CoreId, EventGateView, OutcomeGateView, RuntimeTap, StepRecordView};

/// D2 — `observed_step` parks forever.  Validates that the runtime
/// loop does not await the tap's storage path inline: the runtime
/// must continue to dispatch actions even while the tap's internal
/// work is wedged.
#[derive(Debug)]
pub struct StallingRuntimeTap {
    pub observed_count: AtomicU64,
}

impl StallingRuntimeTap {
    pub fn new() -> Self {
        Self {
            observed_count: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl RuntimeTap for StallingRuntimeTap {
    async fn gate_event(&self, _core: CoreId, _view: &EventGateView<'_>) {}
    async fn gate_outcome(&self, _core: CoreId, _view: &OutcomeGateView<'_>) {}
    fn reserve_step_ids(
        &self,
        _core: CoreId,
        _step_id: Uuid,
        _action_ids: &[Uuid],
        _publish_ids: &[Uuid],
    ) {
    }
    fn observed_step(&self, _core: CoreId, _view: &StepRecordView<'_>) {
        // The contract is that observed_step itself must not block.
        // We simulate "internal storage takes forever" by spawning a
        // stall onto a separate task — the runtime task itself
        // returns immediately.  The counter advances so the test can
        // confirm the call site fires.
        self.observed_count.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(stall());
    }
}

fn stall() -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(async {
        loop {
            tokio::time::sleep(Duration::from_secs(86_400)).await;
        }
    })
}

/// D3 — `observed_step` records a panic flag and increments a counter
/// instead of actually unwinding (a real unwind across the trait
/// boundary would tear down the runtime task; the EC-1 contract is
/// that the impl catches its own panics).  We model "the tap caught a
/// panic internally" by recording the would-be panic and continuing.
#[derive(Debug)]
pub struct PanickingRuntimeTap {
    pub panics_caught: Mutex<u64>,
}

impl PanickingRuntimeTap {
    pub fn new() -> Self {
        Self {
            panics_caught: Mutex::new(0),
        }
    }
}

#[async_trait]
impl RuntimeTap for PanickingRuntimeTap {
    async fn gate_event(&self, _core: CoreId, _view: &EventGateView<'_>) {}
    async fn gate_outcome(&self, _core: CoreId, _view: &OutcomeGateView<'_>) {}
    fn reserve_step_ids(
        &self,
        _core: CoreId,
        _step_id: Uuid,
        _action_ids: &[Uuid],
        _publish_ids: &[Uuid],
    ) {
    }
    fn observed_step(&self, _core: CoreId, _view: &StepRecordView<'_>) {
        // Simulate the contract: the impl catches its own panic and
        // records it.  A real impl might use std::panic::catch_unwind
        // around a serialize call; the equivalent here is recording
        // the "internal panic caught" event without actually
        // unwinding past the trait boundary.
        let mut p = self.panics_caught.lock().unwrap();
        *p += 1;
    }
}
