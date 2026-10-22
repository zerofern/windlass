//! Scenario tests that assert the core's actual state and returned actions.

use std::time::{Instant, SystemTime};

use windlass::core::{
    Action, ClockReading, Core, Event, MamPauseEvent, MamPreferencePersistence as Persistence,
    MamPreferenceState, MamPreferenceUpdate as Update, MamRequestMode as Mode, StartupEvent,
    StartupMode, State,
};

fn send(core: &mut Core, event: Event) -> Vec<Action> {
    core.handle(
        ClockReading {
            monotonic: Instant::now(),
            wall: SystemTime::UNIX_EPOCH,
        },
        event,
    )
}

const fn ready_state(
    saved: Option<Mode>,
    requested: Update,
    effective: Mode,
    persistence: Persistence,
) -> State {
    State::Ready(MamPreferenceState {
        saved,
        requested,
        effective,
        persistence,
    })
}

fn ready(saved: Option<Mode>) -> Core {
    let mut core = Core::new();
    assert_eq!(core.state(), &State::NotStarted);
    assert_eq!(
        send(
            &mut core,
            Event::Startup(StartupEvent::Start {
                mode: StartupMode::Resume
            })
        ),
        vec![Action::OpenStorage {
            mode: StartupMode::Resume
        }]
    );
    assert_eq!(core.state(), &State::OpeningStorage { requested: None });
    assert_eq!(
        send(&mut core, Event::Startup(StartupEvent::StorageOpened)),
        vec![Action::ReadMamRequestMode]
    );
    assert_eq!(
        core.state(),
        &State::ReadingMamPreference { requested: None }
    );
    assert!(
        send(
            &mut core,
            Event::MamPause(MamPauseEvent::Read { mode: saved })
        )
        .is_empty()
    );
    core
}

#[test]
fn defaults_and_saved_preferences() {
    for (saved, expected) in [
        (None, Mode::Paused),
        (Some(Mode::Paused), Mode::Paused),
        (Some(Mode::Enabled), Mode::Enabled),
    ] {
        assert_eq!(
            ready(saved).state(),
            &ready_state(saved, Update::Set(expected), expected, Persistence::Idle)
        );
        assert!(!ready(saved).state().has_failed());
    }
}

#[test]
fn enabling_waits_for_persistence_and_failure_does_not_retry() {
    let mut core = ready(None);
    assert_eq!(
        send(
            &mut core,
            Event::MamPause(MamPauseEvent::Set {
                mode: Mode::Enabled
            })
        ),
        vec![Action::WriteMamRequestMode {
            mode: Mode::Enabled
        }]
    );
    assert_eq!(
        core.state(),
        &ready_state(
            None,
            Update::Set(Mode::Enabled),
            Mode::Paused,
            Persistence::Pending(Update::Set(Mode::Enabled))
        )
    );
    assert!(send(&mut core, Event::MamPause(MamPauseEvent::WriteFailed)).is_empty());
    assert!(core.state().has_failed());
    assert_eq!(
        core.state(),
        &ready_state(
            None,
            Update::Set(Mode::Enabled),
            Mode::Paused,
            Persistence::Failed
        )
    );
    send(
        &mut core,
        Event::MamPause(MamPauseEvent::Set {
            mode: Mode::Enabled,
        }),
    );
    assert!(send(&mut core, Event::MamPause(MamPauseEvent::Written)).is_empty());
    assert_eq!(
        core.state(),
        &ready_state(
            Some(Mode::Enabled),
            Update::Set(Mode::Enabled),
            Mode::Enabled,
            Persistence::Idle
        )
    );
}

#[test]
fn a_newer_pause_wins_over_an_outstanding_enable() {
    let mut core = ready(None);
    send(
        &mut core,
        Event::MamPause(MamPauseEvent::Set {
            mode: Mode::Enabled,
        }),
    );
    assert!(
        send(
            &mut core,
            Event::MamPause(MamPauseEvent::Set { mode: Mode::Paused })
        )
        .is_empty()
    );
    assert_eq!(
        core.state(),
        &ready_state(
            None,
            Update::Set(Mode::Paused),
            Mode::Paused,
            Persistence::Pending(Update::Set(Mode::Enabled))
        )
    );
    assert_eq!(
        send(&mut core, Event::MamPause(MamPauseEvent::Written)),
        vec![Action::WriteMamRequestMode { mode: Mode::Paused }]
    );
    assert!(send(&mut core, Event::MamPause(MamPauseEvent::Written)).is_empty());
    assert_eq!(
        core.state(),
        &ready_state(
            Some(Mode::Paused),
            Update::Set(Mode::Paused),
            Mode::Paused,
            Persistence::Idle
        )
    );
}

#[test]
fn pause_is_immediate_and_failed_pause_stays_blocked() {
    let mut core = ready(Some(Mode::Enabled));
    send(
        &mut core,
        Event::MamPause(MamPauseEvent::Set { mode: Mode::Paused }),
    );
    assert_eq!(
        core.state(),
        &ready_state(
            Some(Mode::Enabled),
            Update::Set(Mode::Paused),
            Mode::Paused,
            Persistence::Pending(Update::Set(Mode::Paused))
        )
    );
    assert!(send(&mut core, Event::MamPause(MamPauseEvent::WriteFailed)).is_empty());
    assert_eq!(
        core.state(),
        &ready_state(
            Some(Mode::Enabled),
            Update::Set(Mode::Paused),
            Mode::Paused,
            Persistence::Failed
        )
    );
}

#[test]
fn deletion_defaults_to_paused_and_waits_for_an_outstanding_write() {
    let mut core = ready(None);
    send(
        &mut core,
        Event::MamPause(MamPauseEvent::Set {
            mode: Mode::Enabled,
        }),
    );
    assert!(send(&mut core, Event::MamPause(MamPauseEvent::Delete)).is_empty());
    assert_eq!(
        core.state(),
        &ready_state(
            None,
            Update::Delete,
            Mode::Paused,
            Persistence::Pending(Update::Set(Mode::Enabled))
        )
    );
    assert_eq!(
        send(&mut core, Event::MamPause(MamPauseEvent::Written)),
        vec![Action::DeleteMamRequestMode]
    );
    assert!(send(&mut core, Event::MamPause(MamPauseEvent::Deleted)).is_empty());
    assert_eq!(
        core.state(),
        &ready_state(None, Update::Delete, Mode::Paused, Persistence::Idle)
    );
    assert!(send(&mut core, Event::MamPause(MamPauseEvent::Deleted)).is_empty());
}

#[test]
fn deletion_completion_reconciles_with_confirmed_storage() {
    for completion in [
        Event::MamPause(MamPauseEvent::DeleteFailed),
        Event::MamPause(MamPauseEvent::Deleted),
    ] {
        let mut core = ready(Some(Mode::Enabled));
        assert_eq!(
            send(&mut core, Event::MamPause(MamPauseEvent::Delete)),
            vec![Action::DeleteMamRequestMode]
        );
        assert!(
            send(
                &mut core,
                Event::MamPause(MamPauseEvent::Set {
                    mode: Mode::Enabled
                })
            )
            .is_empty()
        );
        let actions = send(&mut core, completion);
        if completion == Event::MamPause(MamPauseEvent::Deleted) {
            assert_eq!(
                actions,
                vec![Action::WriteMamRequestMode {
                    mode: Mode::Enabled
                }]
            );
        } else {
            assert!(actions.is_empty());
        }
        assert!(send(&mut core, Event::MamPause(MamPauseEvent::Written)).is_empty());
        assert_eq!(
            core.state(),
            &ready_state(
                Some(Mode::Enabled),
                Update::Set(Mode::Enabled),
                Mode::Enabled,
                Persistence::Idle
            )
        );
    }
}

#[test]
fn commands_during_startup_do_not_get_overwritten_by_loaded_data() {
    for before_open in [true, false] {
        let mut core = Core::new();
        send(
            &mut core,
            Event::Startup(StartupEvent::Start {
                mode: StartupMode::Resume,
            }),
        );
        if before_open {
            assert!(
                send(
                    &mut core,
                    Event::MamPause(MamPauseEvent::Set {
                        mode: Mode::Enabled
                    })
                )
                .is_empty()
            );
            assert_eq!(
                core.state(),
                &State::OpeningStorage {
                    requested: Some(Update::Set(Mode::Enabled))
                }
            );
        }
        send(&mut core, Event::Startup(StartupEvent::StorageOpened));
        if !before_open {
            assert!(
                send(
                    &mut core,
                    Event::MamPause(MamPauseEvent::Set {
                        mode: Mode::Enabled
                    })
                )
                .is_empty()
            );
        }
        assert_eq!(
            core.state(),
            &State::ReadingMamPreference {
                requested: Some(Update::Set(Mode::Enabled))
            }
        );
        assert_eq!(
            send(
                &mut core,
                Event::MamPause(MamPauseEvent::Read {
                    mode: Some(Mode::Paused)
                })
            ),
            vec![Action::WriteMamRequestMode {
                mode: Mode::Enabled
            }]
        );
        assert_eq!(
            core.state(),
            &ready_state(
                Some(Mode::Paused),
                Update::Set(Mode::Enabled),
                Mode::Paused,
                Persistence::Pending(Update::Set(Mode::Enabled))
            )
        );
    }
}

#[test]
fn startup_failure_is_distinct_from_absent_preference() {
    for failure in [
        Event::Startup(StartupEvent::StorageOpenFailed),
        Event::MamPause(MamPauseEvent::ReadFailed),
    ] {
        let mut core = Core::new();
        send(
            &mut core,
            Event::Startup(StartupEvent::Start {
                mode: StartupMode::Resume,
            }),
        );
        if failure == Event::MamPause(MamPauseEvent::ReadFailed) {
            send(&mut core, Event::Startup(StartupEvent::StorageOpened));
        }
        assert!(send(&mut core, failure).is_empty());
        assert_eq!(core.state(), &State::Failed);
        assert!(core.state().has_failed());
        assert!(
            send(
                &mut core,
                Event::MamPause(MamPauseEvent::Set {
                    mode: Mode::Enabled
                })
            )
            .is_empty()
        );
    }
}

#[test]
fn commands_matching_confirmed_storage_do_not_write() {
    for (saved, change, expected) in [
        (None, Update::Delete, Mode::Paused),
        (Some(Mode::Paused), Update::Set(Mode::Paused), Mode::Paused),
        (
            Some(Mode::Enabled),
            Update::Set(Mode::Enabled),
            Mode::Enabled,
        ),
    ] {
        let mut core = ready(saved);
        let event = match change {
            Update::Set(mode) => MamPauseEvent::Set { mode },
            Update::Delete => MamPauseEvent::Delete,
        };
        assert!(send(&mut core, Event::MamPause(event)).is_empty());
        assert_eq!(
            core.state(),
            &ready_state(saved, change, expected, Persistence::Idle)
        );
    }
}

#[test]
fn a_failed_enable_does_not_rewrite_an_already_saved_pause() {
    let mut core = ready(Some(Mode::Paused));
    send(
        &mut core,
        Event::MamPause(MamPauseEvent::Set {
            mode: Mode::Enabled,
        }),
    );
    send(
        &mut core,
        Event::MamPause(MamPauseEvent::Set { mode: Mode::Paused }),
    );
    assert!(send(&mut core, Event::MamPause(MamPauseEvent::WriteFailed)).is_empty());
    assert_eq!(
        core.state(),
        &ready_state(
            Some(Mode::Paused),
            Update::Set(Mode::Paused),
            Mode::Paused,
            Persistence::Idle
        )
    );
    assert!(!core.state().has_failed());
}

#[test]
fn a_deleted_preference_is_not_deleted_again_after_a_failed_enable() {
    let mut core = ready(Some(Mode::Enabled));
    send(&mut core, Event::MamPause(MamPauseEvent::Delete));
    send(&mut core, Event::MamPause(MamPauseEvent::Deleted));
    send(
        &mut core,
        Event::MamPause(MamPauseEvent::Set {
            mode: Mode::Enabled,
        }),
    );
    send(&mut core, Event::MamPause(MamPauseEvent::Delete));
    assert!(send(&mut core, Event::MamPause(MamPauseEvent::WriteFailed)).is_empty());
    assert_eq!(
        core.state(),
        &ready_state(None, Update::Delete, Mode::Paused, Persistence::Idle)
    );
}

#[test]
fn unexpected_results_do_not_advance_the_lifecycle() {
    let mut core = Core::new();
    for event in [
        Event::Startup(StartupEvent::StorageOpened),
        Event::Startup(StartupEvent::StorageOpenFailed),
        Event::MamPause(MamPauseEvent::ReadFailed),
        Event::MamPause(MamPauseEvent::Read { mode: None }),
        Event::MamPause(MamPauseEvent::Written),
        Event::MamPause(MamPauseEvent::WriteFailed),
        Event::MamPause(MamPauseEvent::Deleted),
        Event::MamPause(MamPauseEvent::DeleteFailed),
        Event::MamPause(MamPauseEvent::Delete),
    ] {
        assert!(send(&mut core, event).is_empty());
        assert_eq!(core.state(), &State::NotStarted);
    }
    let mut core = ready(None);
    assert!(
        send(
            &mut core,
            Event::Startup(StartupEvent::Start {
                mode: StartupMode::Initialise
            })
        )
        .is_empty()
    );
    assert!(send(&mut core, Event::MamPause(MamPauseEvent::Written)).is_empty());
    send(
        &mut core,
        Event::MamPause(MamPauseEvent::Set {
            mode: Mode::Enabled,
        }),
    );
    assert!(send(&mut core, Event::MamPause(MamPauseEvent::Deleted)).is_empty());
    assert_eq!(
        core.state(),
        &ready_state(
            None,
            Update::Set(Mode::Enabled),
            Mode::Paused,
            Persistence::Pending(Update::Set(Mode::Enabled))
        )
    );
}
