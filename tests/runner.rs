//! Exercise the shared event ingress and read-only observations with real storage.

use std::{
    error::Error,
    io,
    time::{Duration, Instant, SystemTime},
};

use rusqlite::Connection;
use tokio::sync::watch;
use windlass::{
    core::{
        ClockReading, Event, MamPauseEvent, MamPreferencePersistence, MamPreferenceUpdate,
        MamRequestMode, StartupEvent, StartupMode, State, TimedEvent,
    },
    runner::{Observation, Runner},
};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

async fn observe(
    observations: &mut watch::Receiver<Observation>,
    predicate: impl FnMut(&Observation) -> bool,
) -> TestResult<Observation> {
    let observation =
        tokio::time::timeout(Duration::from_secs(10), observations.wait_for(predicate)).await??;
    Ok(observation.clone())
}

const fn is_idle(observation: &Observation) -> bool {
    matches!(&observation.state, State::Ready(preference)
        if matches!(preference.persistence, MamPreferencePersistence::Idle))
}

#[tokio::test]
async fn queued_occurrence_time_is_preserved() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    let (runner, events, observations) = Runner::new(path.clone());
    let event = TimedEvent {
        occurred_at: ClockReading {
            monotonic: Instant::now(),
            wall: SystemTime::UNIX_EPOCH + Duration::from_secs(123),
        },
        event: Event::MamPause(MamPauseEvent::Set {
            mode: MamRequestMode::Paused,
        }),
    };
    events.send_timed(event).await?;
    events.shutdown().await?;
    let state = tokio::time::timeout(Duration::from_secs(10), runner.run()).await??;
    assert_eq!(state, State::NotStarted);
    assert_eq!(observations.borrow().last_event, Some(event));
    assert!(!path.exists());
    Ok(())
}

#[tokio::test]
async fn a_full_ingress_waits_for_the_runner_to_consume_events() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    let (runner, events, observations) = Runner::new(path);
    let event = Event::MamPause(MamPauseEvent::Set {
        mode: MamRequestMode::Paused,
    });
    for _ in 0..64 {
        events.send(event).await?;
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(20), events.send(event))
            .await
            .is_err()
    );
    let worker = tokio::spawn(runner.run());
    let latest = TimedEvent {
        occurred_at: ClockReading {
            monotonic: Instant::now(),
            wall: SystemTime::UNIX_EPOCH,
        },
        event,
    };
    events.send_timed(latest).await?;
    events.shutdown().await?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), worker).await???,
        State::NotStarted
    );
    assert_eq!(observations.borrow().last_event, Some(latest));
    Ok(())
}

#[tokio::test]
async fn producers_share_one_runner_and_shutdown_drains_follow_up_writes() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    let (runner, console_events, mut console_observations) = Runner::new(path.clone());
    let api_events = console_events.clone();
    let mut api_observations = console_observations.clone();
    console_events
        .send(Event::Startup(StartupEvent::Start {
            mode: StartupMode::Initialise,
        }))
        .await?;
    let worker = tokio::spawn(runner.run());
    observe(&mut console_observations, is_idle).await?;

    let connection = Connection::open(&path)?;
    connection.execute_batch("BEGIN IMMEDIATE")?;
    api_events
        .send(Event::MamPause(MamPauseEvent::Set {
            mode: MamRequestMode::Enabled,
        }))
        .await?;
    observe(&mut api_observations, |observation| {
        matches!(&observation.state, State::Ready(preference)
            if preference.persistence == MamPreferencePersistence::Pending(
                MamPreferenceUpdate::Set(MamRequestMode::Enabled)))
    })
    .await?;
    console_events
        .send(Event::MamPause(MamPauseEvent::Set {
            mode: MamRequestMode::Paused,
        }))
        .await?;
    let paused = observe(&mut console_observations, |observation| {
        matches!(&observation.state, State::Ready(preference)
            if preference.requested == MamPreferenceUpdate::Set(MamRequestMode::Paused)
                && preference.effective == MamRequestMode::Paused)
    })
    .await?;
    assert_eq!(api_observations.borrow().state, paused.state);
    api_events.shutdown().await?;
    let rejected = console_events
        .send(Event::MamPause(MamPauseEvent::Delete))
        .await;
    assert!(matches!(rejected, Err(error) if error.kind() == io::ErrorKind::BrokenPipe));
    assert!(!worker.is_finished());

    connection.execute_batch("COMMIT")?;
    let final_state = tokio::time::timeout(Duration::from_secs(10), worker).await???;
    assert!(matches!(&final_state, State::Ready(preference)
        if preference.saved == Some(MamRequestMode::Paused)
            && preference.effective == MamRequestMode::Paused
            && preference.persistence == MamPreferencePersistence::Idle));
    assert_eq!(console_observations.borrow().state, final_state);
    assert_eq!(api_observations.borrow().state, final_state);
    let saved: String =
        connection.query_row("SELECT request_mode FROM mam_configuration", [], |row| {
            row.get(0)
        })?;
    assert_eq!(saved, "paused");
    assert_eq!(
        console_observations
            .borrow()
            .last_event
            .map(|event| event.event),
        Some(Event::MamPause(MamPauseEvent::Written))
    );
    let rejected = api_events
        .send(Event::MamPause(MamPauseEvent::Delete))
        .await;
    assert!(matches!(rejected, Err(error) if error.kind() == io::ErrorKind::BrokenPipe));
    assert!(api_events.shutdown().await.is_err());
    Ok(())
}

#[tokio::test]
async fn storage_failures_are_observable_without_a_console() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    let (runner, events, mut observations) = Runner::new(path.clone());
    events
        .send(Event::Startup(StartupEvent::Start {
            mode: StartupMode::Initialise,
        }))
        .await?;
    let worker = tokio::spawn(runner.run());
    observe(&mut observations, is_idle).await?;
    Connection::open(&path)?.execute_batch("CREATE TRIGGER reject_write BEFORE INSERT ON mam_configuration BEGIN SELECT RAISE(ABORT, 'write rejected'); END;")?;
    events
        .send(Event::MamPause(MamPauseEvent::Set {
            mode: MamRequestMode::Enabled,
        }))
        .await?;
    let failed = observe(&mut observations, |observation| {
        observation.state.has_failed()
    })
    .await?;
    assert!(
        failed
            .storage_error
            .as_deref()
            .is_some_and(|detail| detail.contains("write rejected"))
    );
    assert_eq!(
        failed.last_event.map(|event| event.event),
        Some(Event::MamPause(MamPauseEvent::WriteFailed))
    );
    events.shutdown().await?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), worker).await???,
        failed.state
    );
    Ok(())
}
