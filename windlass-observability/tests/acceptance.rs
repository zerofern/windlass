//! §37pre acceptance tests, locked 2026-06-01.
//!
//! Each acceptance test corresponds to a numbered scenario in
//! `docs/observability-redesign.md` "Acceptance tests".  They exercise
//! the full ServiceRuntime + ObservabilityController seam and assert
//! the §37pre engineering contracts hold end-to-end.
//!
//! Test harnesses (D1..D8 in the checklist) live under
//! `tests/common/` so Cargo treats them as shared modules instead of
//! standalone integration-test crates.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::d1_recording_tap::RecordingRuntimeTap;
use common::support::{TinyAction, TinyEvent, TinyMachine, TinyShell};
use tokio::sync::mpsc;
use windlass_machine::{ExternalCause, NullRuntimeTap, Timed};

// ── Acceptance test #1 — Observer equivalence + publish_id preservation ──────

#[tokio::test]
async fn observer_equivalence_for_event_sequence() {
    // Drive the same event sequence into two ServiceRuntimes — one
    // with NullRuntimeTap, one with RecordingRuntimeTap — and assert
    // each receives the same actions in the same order on the shell's
    // sink.  EC-6: observation must not change observable behavior.
    let null_actions = drive_sequence(NullRuntimeTap::arc()).await;
    let recording = Arc::new(RecordingRuntimeTap::new());
    let recorded_actions = drive_sequence(recording.clone() as _).await;
    assert_eq!(
        null_actions, recorded_actions,
        "live tap must not change dispatched-action order"
    );
    // The recording tap must have observed every step.
    assert!(
        !recording.steps().is_empty(),
        "recording tap should have captured every step"
    );
}

#[tokio::test]
async fn fanout_bridge_preserves_publish_id() {
    // D8: a publish emitted by core A with publish_id X must arrive
    // at a subscriber bridge in core B with `Timed::from_publish(now,
    // X, derived_event)`.  Without this the cross-core causal graph
    // would silently lose every jump.
    common::d8_fanout_bridge::run().await;
}

// ── Acceptance test #2 — Observer cannot block dispatch ──────────────────────

#[tokio::test]
async fn stalling_tap_does_not_block_runtime_progress() {
    // D2: `observed_step` simulates "internal storage stalled" by
    // spawning a forever-sleep on its own task and returning
    // immediately.  EC-1: the runtime keeps making forward progress
    // because the runtime task never awaits the tap's internal work.
    use std::sync::atomic::Ordering;
    let tap = Arc::new(common::d2_d3_stalling_panicking::StallingRuntimeTap::new());
    let actions = drive_sequence(tap.clone() as _).await;
    assert_eq!(
        actions.len(),
        5,
        "runtime should dispatch all five actions even while the tap is stalled"
    );
    assert!(
        tap.observed_count.load(Ordering::SeqCst) >= 5,
        "tap should have been invoked at least once per dispatched action"
    );
}

#[tokio::test]
async fn panic_catching_tap_keeps_runtime_alive() {
    // D3: every `observed_step` would have panicked internally; the
    // tap impl catches its own panic (EC-1 trait-boundary contract)
    // and increments a counter.  The runtime keeps dispatching.
    let tap = Arc::new(common::d2_d3_stalling_panicking::PanickingRuntimeTap::new());
    let actions = drive_sequence(tap.clone() as _).await;
    assert_eq!(actions.len(), 5, "runtime should dispatch every action");
    assert_eq!(
        *tap.panics_caught.lock().unwrap(),
        5,
        "tap should have caught five would-be panics"
    );
}

/// Drive a fixed `Ping` sequence into a runtime built with the given
/// tap, returning the actions the shell dispatched in order.  The
/// inputs are identical across taps so any observable difference must
/// come from the tap itself.
async fn drive_sequence(tap: Arc<dyn windlass_machine::RuntimeTap>) -> Vec<TinyAction> {
    let (sink_tx, mut sink_rx) = mpsc::unbounded_channel::<TinyAction>();
    let (handles, _join) = windlass_machine::spawn::<TinyMachine, TinyShell>(
        windlass_machine::CoreId::Vpn,
        tap,
        (),
        sink_tx,
    )
    .await;

    let now = std::time::Instant::now();
    for _ in 0..5 {
        handles
            .events
            .send(Timed::external(now, ExternalCause::Init, TinyEvent::Ping))
            .unwrap();
    }

    let mut received = Vec::new();
    for _ in 0..5 {
        let action = tokio::time::timeout(Duration::from_millis(500), sink_rx.recv())
            .await
            .expect("dispatch produced action within timeout")
            .expect("sink open");
        received.push(action);
    }
    drop(handles);
    received
}
