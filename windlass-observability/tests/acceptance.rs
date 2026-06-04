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
use windlass_machine::{ExternalCause, NullRuntimeTap, RuntimeTap, Timed};

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
async fn step_record_publishes_carry_topic_name() {
    // §37pre Stored records: `StoredPublish.topic` carries the topic
    // name the publish was emitted on, so the UI can filter and the
    // operator can audit which topic produced a downstream event.
    use windlass_observability::{ObservabilityController, SseMessage};

    let ctrl = ObservabilityController::new();
    let mut rx = ctrl.subscribe();

    let (sink_tx, _sink_rx) = mpsc::unbounded_channel::<TinyAction>();
    let (handles, _join) = windlass_machine::spawn::<TinyMachine, TinyShell>(
        windlass_machine::CoreId::Vpn,
        ctrl.clone(),
        (),
        sink_tx,
    )
    .await;

    handles
        .events
        .send(Timed::external(
            std::time::Instant::now(),
            ExternalCause::Init,
            TinyEvent::Ping,
        ))
        .unwrap();

    let mut saw_topic = false;
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        if let Ok(msg) = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
            if let Ok(SseMessage::Step(record)) = msg {
                for publish in &record.publishes {
                    assert_eq!(
                        publish.topic, "Beeps",
                        "publish topic must round-trip into StoredPublish"
                    );
                    saw_topic = true;
                }
                if saw_topic {
                    break;
                }
            }
        }
    }
    assert!(saw_topic, "expected at least one publish with a topic");
    drop(handles);
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

// ── Acceptance test #6 — Secret behavior ─────────────────────────────────────

#[tokio::test]
async fn secret_behavior_no_cleartext_on_wire() {
    // D7: walk every secret-bearing wire type through
    // `serde_json::to_value` and assert the result never carries
    // cleartext.  Covers the three locked types:
    //
    // - `ServerSecretSlot` (server-side holder) → must serialize as
    //   `WireRedacted { redacted: true, reveal_id }`.
    // - `MaybeSecret::Secret(_)` → must redact.
    // - `MaybeSecret::Plain(_)` → must pass through unchanged.
    use windlass_observability::{MaybeSecret, ServerSecretSlot};

    const SECRET: &str = "MAM_SESSION_v1_supersecret_token_value";

    let slot = ServerSecretSlot::new(SECRET);
    let slot_json = serde_json::to_string(&slot).expect("slot serializes");
    assert!(
        !slot_json.contains(SECRET),
        "ServerSecretSlot must not leak cleartext: {slot_json}"
    );
    assert!(slot_json.contains("\"redacted\":true"));
    assert!(slot_json.contains("reveal_id"));

    let secret = MaybeSecret::secret(SECRET);
    let secret_json = serde_json::to_string(&secret).expect("MaybeSecret serializes");
    assert!(
        !secret_json.contains(SECRET),
        "MaybeSecret::Secret must not leak cleartext: {secret_json}"
    );
    assert!(secret_json.contains("\"redacted\":true"));

    let plain = MaybeSecret::plain("application/json");
    let plain_json = serde_json::to_value(&plain).expect("plain serializes");
    assert_eq!(
        plain_json,
        serde_json::Value::String("application/json".into()),
        "MaybeSecret::Plain must pass through unchanged"
    );
}

#[tokio::test]
async fn reveal_endpoint_returns_410_for_unknown_id() {
    // Decision 14 + B6: the reveal endpoint is the only path to
    // cleartext, and missing IDs (evicted or never minted) must
    // surface as 410 Gone via the controller method.  The HTTP
    // route in `windlass-web` maps `None` → `StatusCode::GONE`.
    use windlass_observability::ObservabilityController;
    let ctrl = ObservabilityController::new();
    assert!(ctrl.reveal(uuid::Uuid::new_v4()).await.is_none());
}

// ── Acceptance test #5 — Ring eviction cleans indices ────────────────────────

#[tokio::test]
async fn ring_eviction_emits_evicted_message_and_drops_indices() {
    // D6: fill a core's step ring past its count budget; assert the
    // controller emits an `Evicted` SSE message listing the dropped
    // step + action + publish IDs.  EC-3: the controller's
    // action_id / publish_id indices must be cleaned in lockstep.
    use windlass_machine::{CoreId, StepKind, StepRecordView};
    use windlass_observability::{ObservabilityConfig, ObservabilityController, SseMessage};

    // Squeeze the per-core step ring down to 3 entries so we hit
    // eviction quickly without flooding the channel.
    let cfg = ObservabilityConfig {
        step_records_per_core: 3,
        ..ObservabilityConfig::default()
    };
    let ctrl = ObservabilityController::with_config(cfg);
    let mut rx = ctrl.subscribe();

    let event = serde_json::Value::Null;
    let state = serde_json::Value::Null;
    let cause = windlass_machine::EventCause::External(ExternalCause::Init);
    let action_ids: Vec<uuid::Uuid> = (0..5).map(|_| uuid::Uuid::new_v4()).collect();
    let publish_ids: Vec<uuid::Uuid> = (0..5).map(|_| uuid::Uuid::new_v4()).collect();
    let action_payload = vec![serde_json::Value::Null];
    let publish_payload = vec![serde_json::Value::Null];

    for i in 0..5 {
        ctrl.observed_step(
            CoreId::Vpn,
            &StepRecordView {
                core: CoreId::Vpn,
                step_id: uuid::Uuid::new_v4(),
                recorded_at: chrono::Utc::now(),
                duration: Duration::from_millis(0),
                kind: StepKind::Event,
                event_variant: "T",
                event: &event,
                event_cause: &cause,
                state_after: &state,
                action_ids: std::slice::from_ref(&action_ids[i]),
                action_variants: &["A"],
                action_payloads: &action_payload,
                publish_ids: std::slice::from_ref(&publish_ids[i]),
                publish_variants: &["P"],
                publish_payloads: &publish_payload,
                publish_topics: &["TestTopic"],
            },
        );
    }

    // Give the EC-5 worker time to drain.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Drain the SSE broadcast for at least one Evicted message that
    // lists action + publish ids — proves the indices were cleaned.
    let mut saw_evicted = false;
    while let Ok(msg) = rx.try_recv() {
        if let SseMessage::Evicted(ids) = msg {
            saw_evicted = true;
            assert!(
                !ids.action_ids.is_empty() || !ids.publish_ids.is_empty(),
                "Evicted must list at least one action or publish id (EC-3)"
            );
        }
    }
    assert!(saw_evicted, "expected at least one Evicted SSE message");
}

// ── Acceptance test #4 — HTTP gate prevents send ─────────────────────────────

#[tokio::test]
async fn signal_rate_limit_anomaly_parks_next_request() {
    // D5: A real httpmock server is overkill for this property — what
    // matters is that `signal_anomaly(RateLimitViolation)` flips the
    // per-core pause so the *next* `gate_request` for that core parks.
    // The controller's HttpTap impl is the unit under test.  P7
    // wiring: anomaly → pause → next gate_request parks until release.
    use std::sync::atomic::{AtomicBool, Ordering};
    use windlass_machine::CoreId;
    use windlass_observability::ObservabilityController;
    use windlass_types::{HttpAnomaly, HttpRequestView, HttpTap};

    let ctrl = ObservabilityController::new();

    // Anomaly fires → pause flips for the affected core only.
    ctrl.signal_anomaly(
        CoreId::Mam,
        HttpAnomaly::RateLimitViolation {
            reason: "test".to_string(),
        },
    );
    assert!(ctrl.is_paused(CoreId::Mam));
    assert!(!ctrl.is_paused(CoreId::Qbit));

    // The next gate_request parks.  We launch it on a task and
    // confirm it does not complete within a short window.
    let returned = Arc::new(AtomicBool::new(false));
    let returned_clone = Arc::clone(&returned);
    let ctrl_clone = ctrl.clone();
    let handle = tokio::spawn(async move {
        ctrl_clone
            .gate_request(
                CoreId::Mam,
                &HttpRequestView {
                    method: "POST",
                    url: "https://mam.example/api",
                    body: None,
                },
            )
            .await;
        returned_clone.store(true, Ordering::SeqCst);
    });

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !returned.load(Ordering::SeqCst),
        "gate_request must park while the core is paused"
    );

    // Release: stepping advances the parked gate.
    ctrl.step(CoreId::Mam);
    handle.await.expect("parked gate eventually completes");
    assert!(returned.load(Ordering::SeqCst));
}

// ── Acceptance test #3 — Per-core pause isolation ────────────────────────────

#[tokio::test]
async fn pause_mam_does_not_block_qbit() {
    // D4: drive two separately-spawned ServiceRuntimes attached to a
    // single ObservabilityController.  Pause one core, feed events to
    // both, assert only the paused core stalls — the other runs to
    // completion.  EC-2: pause is per-core, gates are the only place
    // a tap may park.
    use windlass_machine::CoreId;
    use windlass_observability::ObservabilityController;

    let ctrl = ObservabilityController::new();

    let (mam_sink_tx, mut mam_sink_rx) = mpsc::unbounded_channel::<TinyAction>();
    let (mam_handles, _mam_join) = windlass_machine::spawn::<TinyMachine, TinyShell>(
        CoreId::Mam,
        ctrl.clone(),
        (),
        mam_sink_tx,
    )
    .await;

    let (qbit_sink_tx, mut qbit_sink_rx) = mpsc::unbounded_channel::<TinyAction>();
    let (qbit_handles, _qbit_join) = windlass_machine::spawn::<TinyMachine, TinyShell>(
        CoreId::Qbit,
        ctrl.clone(),
        (),
        qbit_sink_tx,
    )
    .await;

    ctrl.pause(CoreId::Mam);
    assert!(ctrl.is_paused(CoreId::Mam));
    assert!(!ctrl.is_paused(CoreId::Qbit));

    let now = std::time::Instant::now();
    mam_handles
        .events
        .send(Timed::external(now, ExternalCause::Init, TinyEvent::Ping))
        .unwrap();
    qbit_handles
        .events
        .send(Timed::external(now, ExternalCause::Init, TinyEvent::Ping))
        .unwrap();

    // qBit must dispatch quickly — it's not paused.
    let qbit_action = tokio::time::timeout(Duration::from_millis(500), qbit_sink_rx.recv())
        .await
        .expect("qbit dispatched within timeout")
        .expect("qbit sink open");
    assert_eq!(qbit_action, TinyAction::Pong);

    // MAM must NOT dispatch while paused — gate_event parks it.
    let mam_result = tokio::time::timeout(Duration::from_millis(150), mam_sink_rx.recv()).await;
    assert!(
        mam_result.is_err(),
        "mam should remain parked at gate_event while paused"
    );

    // Release MAM and confirm its action lands.
    ctrl.resume(CoreId::Mam);
    let mam_action = tokio::time::timeout(Duration::from_millis(500), mam_sink_rx.recv())
        .await
        .expect("mam dispatched after resume")
        .expect("mam sink open");
    assert_eq!(mam_action, TinyAction::Pong);
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
