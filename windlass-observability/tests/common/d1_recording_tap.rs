//! D1: `RecordingRuntimeTap` — captures every observed call into a
//! `Vec` for assertions.  Used by acceptance test #1 (Observer
//! equivalence) to confirm the runtime hands the tap the same calls
//! it would have made into a `NullRuntimeTap` (i.e. dispatch order +
//! step shape unchanged).
//!
//! The tap is intentionally minimal: no parking, no breakpoints, no
//! channels.  EC-6 only requires that the runtime's *observable*
//! behavior not change.  Anything beyond capture-into-Vec would
//! conflate behavior validation with assertions about controller
//! internals.

use std::sync::Mutex;

use async_trait::async_trait;
use uuid::Uuid;
use windlass_machine::{CoreId, EventGateView, OutcomeGateView, RuntimeTap, StepRecordView};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedStep {
    pub core: CoreId,
    pub step_id: Uuid,
    pub event_variant: String,
    pub action_variants: Vec<String>,
    pub publish_variants: Vec<String>,
}

#[derive(Debug, Default)]
pub struct RecordingRuntimeTap {
    steps: Mutex<Vec<CapturedStep>>,
}

impl RecordingRuntimeTap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn steps(&self) -> Vec<CapturedStep> {
        self.steps.lock().unwrap().clone()
    }
}

#[async_trait]
impl RuntimeTap for RecordingRuntimeTap {
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
    fn observed_step(&self, core: CoreId, view: &StepRecordView<'_>) {
        let captured = CapturedStep {
            core,
            step_id: view.step_id,
            event_variant: view.event_variant.to_owned(),
            action_variants: view
                .action_variants
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            publish_variants: view
                .publish_variants
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        };
        self.steps.lock().unwrap().push(captured);
    }
}
