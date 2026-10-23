//! Scenario tests for dashboard settings, history bookkeeping, and status queries.

use std::time::{Instant, SystemTime};

use windlass::core::{
    Action, ApiCookie, ClockReading, Core, EntryCount, Event, HistoryEvent, HistoryLimit,
    HistoryState, InvalidApiCookie, MamPauseEvent, MamRequestMode as Mode, MamRequestsStatus,
    Setting, SettingPersistence, SettingStatus, Settings, SettingsEvent, SettingsState,
    StartupEvent, StartupMode, State, Trim,
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

fn cookie(value: &str) -> ApiCookie {
    ApiCookie::parse(value).unwrap_or_else(|_| unreachable!("valid test cookie"))
}

fn limit(value: u32) -> HistoryLimit {
    HistoryLimit::try_new(value).unwrap_or_else(|_| unreachable!("valid test limit"))
}

fn settings_event(core: &mut Core, event: SettingsEvent) -> Vec<Action> {
    send(core, Event::Settings(event))
}

/// Start a core, load saved settings, and open history with `retained` entries.
fn started(history_limit: Option<HistoryLimit>, api_cookie: Option<ApiCookie>) -> Core {
    let mut core = Core::new();
    assert_eq!(core.settings(), &SettingsState::Unread);
    assert_eq!(core.history(), &HistoryState::Closed);
    let _ = send(
        &mut core,
        Event::Startup(StartupEvent::Start {
            mode: StartupMode::Resume,
        }),
    );
    let _ = send(&mut core, Event::Startup(StartupEvent::StorageOpened));
    assert_eq!(core.settings(), &SettingsState::Reading);
    assert!(matches!(core.history(), HistoryState::Opening(_)));
    assert!(
        settings_event(
            &mut core,
            SettingsEvent::Read {
                history_limit,
                api_cookie,
            },
        )
        .is_empty()
    );
    core
}

fn retained(core: &Core) -> u64 {
    match core.history() {
        HistoryState::Open { retained, .. } => retained.get(),
        other => unreachable!("history is not open: {other:?}"),
    }
}

#[test]
fn absent_settings_use_defaults_and_mam_requests_start_paused() {
    let mut core = started(None, None);
    assert_eq!(
        core.settings(),
        &SettingsState::Loaded(Settings {
            history_limit: Setting {
                saved: HistoryLimit::default(),
                requested: HistoryLimit::default(),
                persistence: SettingPersistence::Idle,
            },
            api_cookie: Setting {
                saved: None,
                requested: None,
                persistence: SettingPersistence::Idle,
            },
        })
    );
    assert_eq!(HistoryLimit::default().into_inner(), 10_000);
    assert_eq!(core.state().mam_requests(), MamRequestsStatus::Starting);
    let _ = send(
        &mut core,
        Event::MamPause(MamPauseEvent::Read { mode: None }),
    );
    assert_eq!(core.state().mam_requests(), MamRequestsStatus::Paused);
}

#[test]
fn settings_changes_before_loading_or_after_a_failed_read_are_ignored() {
    let mut core = Core::new();
    assert!(settings_event(&mut core, SettingsEvent::ClearApiCookie).is_empty());
    assert!(
        settings_event(&mut core, SettingsEvent::ReadFailed).is_empty(),
        "a read result without a read request is ignored"
    );
    assert!(
        settings_event(
            &mut core,
            SettingsEvent::Read {
                history_limit: None,
                api_cookie: None
            }
        )
        .is_empty()
    );
    assert_eq!(core.settings(), &SettingsState::Unread);
    let _ = send(
        &mut core,
        Event::Startup(StartupEvent::Start {
            mode: StartupMode::Resume,
        }),
    );
    let _ = send(&mut core, Event::Startup(StartupEvent::StorageOpened));
    let _ = settings_event(&mut core, SettingsEvent::ReadFailed);
    assert_eq!(core.settings(), &SettingsState::ReadFailed);
    assert_eq!(core.settings().loaded(), None);
    assert_eq!(core.settings().history_limit(), None);
    assert!(
        settings_event(
            &mut core,
            SettingsEvent::SetHistoryLimit { limit: limit(500) }
        )
        .is_empty()
    );
}

#[test]
fn a_replacement_cookie_is_used_only_after_it_is_saved() {
    let old = cookie("old-session");
    let new = cookie("new-session");
    let mut core = started(None, Some(old.clone()));
    let loaded = |core: &Core| core.settings().loaded().cloned();
    assert_eq!(
        loaded(&core).and_then(|settings| settings.usable_api_cookie().cloned()),
        Some(old)
    );
    assert_eq!(
        settings_event(
            &mut core,
            SettingsEvent::SetApiCookie {
                cookie: new.clone()
            }
        ),
        vec![Action::WriteApiCookie {
            cookie: Some(new.clone())
        }]
    );
    let pending = loaded(&core).unwrap_or_else(|| unreachable!());
    assert_eq!(pending.api_cookie.status(), SettingStatus::Saving);
    assert_eq!(
        pending.usable_api_cookie(),
        None,
        "the old cookie is not used"
    );

    assert!(settings_event(&mut core, SettingsEvent::ApiCookieWriteFailed).is_empty());
    let failed = loaded(&core).unwrap_or_else(|| unreachable!());
    assert_eq!(failed.api_cookie.status(), SettingStatus::NotSaved);
    assert_eq!(failed.usable_api_cookie(), None, "never fall back");

    assert_eq!(
        settings_event(
            &mut core,
            SettingsEvent::SetApiCookie {
                cookie: new.clone()
            }
        ),
        vec![Action::WriteApiCookie {
            cookie: Some(new.clone())
        }],
        "the user can retry"
    );
    assert!(settings_event(&mut core, SettingsEvent::ApiCookieWritten).is_empty());
    let saved = loaded(&core).unwrap_or_else(|| unreachable!());
    assert_eq!(saved.api_cookie.status(), SettingStatus::Saved);
    assert_eq!(saved.usable_api_cookie(), Some(&new));

    assert_eq!(
        settings_event(&mut core, SettingsEvent::ClearApiCookie),
        vec![Action::WriteApiCookie { cookie: None }]
    );
    assert!(settings_event(&mut core, SettingsEvent::ApiCookieWritten).is_empty());
    assert_eq!(
        loaded(&core).and_then(|settings| settings.usable_api_cookie().cloned()),
        None
    );
}

#[test]
fn one_setting_save_is_outstanding_and_the_latest_request_is_saved_next() {
    let mut core = started(None, None);
    assert_eq!(
        settings_event(
            &mut core,
            SettingsEvent::SetHistoryLimit { limit: limit(500) }
        ),
        vec![Action::WriteHistoryLimit { limit: limit(500) }]
    );
    assert!(
        settings_event(
            &mut core,
            SettingsEvent::SetHistoryLimit { limit: limit(700) }
        )
        .is_empty()
    );
    assert_eq!(
        settings_event(&mut core, SettingsEvent::HistoryLimitWritten),
        vec![Action::WriteHistoryLimit { limit: limit(700) }]
    );
    assert_eq!(core.settings().history_limit(), Some(limit(500)));
    assert!(settings_event(&mut core, SettingsEvent::HistoryLimitWriteFailed).is_empty());
    assert_eq!(core.settings().history_limit(), Some(limit(500)));
    let setting = |core: &Core| {
        core.settings()
            .loaded()
            .map(|settings| settings.history_limit.clone())
    };
    assert_eq!(
        setting(&core).map(|setting| setting.status()),
        Some(SettingStatus::NotSaved)
    );
    // Asking for the saved value again needs no write and clears the failure.
    assert!(
        settings_event(
            &mut core,
            SettingsEvent::SetHistoryLimit { limit: limit(500) }
        )
        .is_empty()
    );
    assert_eq!(
        setting(&core).map(|setting| setting.status()),
        Some(SettingStatus::Saved)
    );
    // Results without an outstanding save change nothing.
    assert!(settings_event(&mut core, SettingsEvent::HistoryLimitWritten).is_empty());
    assert!(settings_event(&mut core, SettingsEvent::ApiCookieWritten).is_empty());
    assert!(
        settings_event(
            &mut core,
            SettingsEvent::Read {
                history_limit: Some(limit(100)),
                api_cookie: None
            }
        )
        .is_empty()
    );
    assert!(settings_event(&mut core, SettingsEvent::ReadFailed).is_empty());
    assert_eq!(core.settings().history_limit(), Some(limit(500)));
}

#[test]
fn a_failed_save_followed_by_a_newer_request_saves_the_newer_value() {
    let mut core = started(None, None);
    let _ = settings_event(
        &mut core,
        SettingsEvent::SetHistoryLimit { limit: limit(500) },
    );
    let _ = settings_event(
        &mut core,
        SettingsEvent::SetHistoryLimit { limit: limit(600) },
    );
    assert_eq!(
        settings_event(&mut core, SettingsEvent::HistoryLimitWriteFailed),
        vec![Action::WriteHistoryLimit { limit: limit(600) }]
    );
}

#[test]
fn history_counts_every_step_and_trims_below_the_limit() {
    let mut core = started(Some(limit(100)), None);
    // Start, storage opened, and settings read precede the open result.
    let _ = send(
        &mut core,
        Event::History(HistoryEvent::Opened {
            retained: EntryCount::new(2),
        }),
    );
    // Entries up to the opening step, the settings step, and this step.
    assert_eq!(retained(&core), 4);
    let pause = Event::MamPause(MamPauseEvent::Set { mode: Mode::Paused });
    for _ in 0..96 {
        assert!(send(&mut core, pause.clone()).is_empty());
    }
    assert_eq!(retained(&core), 100);
    assert_eq!(
        send(&mut core, pause.clone()),
        vec![Action::TrimHistory {
            keep: EntryCount::new(90)
        }]
    );
    assert!(matches!(
        core.history(),
        HistoryState::Open {
            trim: Trim::Pending(_),
            ..
        }
    ));
    assert!(send(&mut core, pause).is_empty(), "one trim at a time");
    let _ = send(
        &mut core,
        Event::History(HistoryEvent::Trimmed {
            retained: EntryCount::new(90),
        }),
    );
    // The step between the trim and its result, and the result itself.
    assert_eq!(retained(&core), 92);
    let _ = send(
        &mut core,
        Event::History(HistoryEvent::EntriesLost {
            count: EntryCount::new(5),
            from: SystemTime::UNIX_EPOCH,
            until: SystemTime::UNIX_EPOCH,
        }),
    );
    assert_eq!(retained(&core), 88);
}

#[test]
fn a_failed_trim_is_not_retried_until_restart() {
    let mut core = started(Some(limit(100)), None);
    assert_eq!(
        send(
            &mut core,
            Event::History(HistoryEvent::Opened {
                retained: EntryCount::new(200),
            })
        ),
        vec![Action::TrimHistory {
            keep: EntryCount::new(90)
        }]
    );
    let pause = Event::MamPause(MamPauseEvent::Set { mode: Mode::Paused });
    let _ = send(&mut core, Event::History(HistoryEvent::TrimFailed));
    assert!(matches!(
        core.history(),
        HistoryState::Open {
            trim: Trim::Failed,
            ..
        }
    ));
    assert!(send(&mut core, pause).is_empty());
    // Stray results change nothing.
    let _ = send(
        &mut core,
        Event::History(HistoryEvent::Trimmed {
            retained: EntryCount::new(1),
        }),
    );
    let _ = send(&mut core, Event::History(HistoryEvent::TrimFailed));
    assert!(retained(&core) > 200);
}

#[test]
fn history_waits_for_settings_before_trimming_and_reports_open_failure() {
    let mut core = Core::new();
    let _ = send(
        &mut core,
        Event::Startup(StartupEvent::Start {
            mode: StartupMode::Resume,
        }),
    );
    let _ = send(&mut core, Event::Startup(StartupEvent::StorageOpened));
    assert!(
        send(
            &mut core,
            Event::History(HistoryEvent::Opened {
                retained: EntryCount::new(50_000),
            })
        )
        .is_empty(),
        "no limit is known yet"
    );
    assert_eq!(
        settings_event(
            &mut core,
            SettingsEvent::Read {
                history_limit: None,
                api_cookie: None
            }
        ),
        vec![Action::TrimHistory {
            keep: EntryCount::new(9_000)
        }]
    );

    let mut failed = Core::new();
    let _ = send(
        &mut failed,
        Event::Startup(StartupEvent::Start {
            mode: StartupMode::Resume,
        }),
    );
    let _ = send(&mut failed, Event::Startup(StartupEvent::StorageOpened));
    let _ = send(&mut failed, Event::History(HistoryEvent::OpenFailed));
    assert_eq!(failed.history(), &HistoryState::Unavailable);
    let _ = send(
        &mut failed,
        Event::History(HistoryEvent::EntriesLost {
            count: EntryCount::new(1),
            from: SystemTime::UNIX_EPOCH,
            until: SystemTime::UNIX_EPOCH,
        }),
    );
    assert_eq!(failed.history(), &HistoryState::Unavailable);
}

#[test]
fn mam_request_status_follows_the_latest_request_and_its_save() {
    let mut core = started(None, None);
    let _ = send(
        &mut core,
        Event::MamPause(MamPauseEvent::Read {
            mode: Some(Mode::Enabled),
        }),
    );
    let status = |core: &Core| core.state().mam_requests();
    let set = |mode| Event::MamPause(MamPauseEvent::Set { mode });
    assert_eq!(status(&core), MamRequestsStatus::Running);
    assert_eq!(status(&core).requested(), Some(Mode::Enabled));
    let _ = send(&mut core, set(Mode::Paused));
    assert_eq!(status(&core), MamRequestsStatus::PausePending);
    let _ = send(&mut core, Event::MamPause(MamPauseEvent::WriteFailed));
    assert_eq!(status(&core), MamRequestsStatus::PauseNotSaved);
    assert_eq!(status(&core).requested(), Some(Mode::Paused));
    let _ = send(&mut core, set(Mode::Paused));
    let _ = send(&mut core, Event::MamPause(MamPauseEvent::Written));
    assert_eq!(status(&core), MamRequestsStatus::Paused);
    let _ = send(&mut core, set(Mode::Enabled));
    assert_eq!(status(&core), MamRequestsStatus::ResumePending);
    let _ = send(&mut core, Event::MamPause(MamPauseEvent::WriteFailed));
    assert_eq!(status(&core), MamRequestsStatus::ResumeNotSaved);
    assert_eq!(status(&core).requested(), Some(Mode::Enabled));
    assert_eq!(MamRequestsStatus::Starting.requested(), None);

    let mut failed = Core::new();
    let _ = send(
        &mut failed,
        Event::Startup(StartupEvent::Start {
            mode: StartupMode::Resume,
        }),
    );
    let _ = send(&mut failed, Event::Startup(StartupEvent::StorageOpenFailed));
    assert_eq!(failed.state(), &State::Failed);
    assert_eq!(
        failed.state().mam_requests(),
        MamRequestsStatus::StartupFailed
    );
    assert_eq!(MamRequestsStatus::StartupFailed.requested(), None);
}

#[test]
fn api_cookies_are_validated_and_never_revealed() -> Result<(), serde_json::Error> {
    assert_eq!(ApiCookie::parse("  "), Err(InvalidApiCookie::Empty));
    for invalid in [
        "has space",
        "semi;colon",
        "comma,",
        "quote\"",
        "back\\slash",
        "é",
    ] {
        assert_eq!(
            ApiCookie::parse(invalid),
            Err(InvalidApiCookie::InvalidCharacter),
            "{invalid}"
        );
    }
    let secret = cookie(" abc%2B123= ");
    assert_eq!(secret.expose_secret(), "abc%2B123=");
    assert!(!format!("{secret:?}").contains("abc"));
    let event = Event::Settings(SettingsEvent::SetApiCookie {
        cookie: secret.clone(),
    });
    let recorded = serde_json::to_string(&event)?;
    assert!(!recorded.contains("abc"), "{recorded}");
    assert!(recorded.contains("[redacted]"));
    assert_ne!(secret, cookie("other"));
    assert!(!InvalidApiCookie::Empty.to_string().is_empty());
    assert!(!InvalidApiCookie::InvalidCharacter.to_string().is_empty());
    Ok(())
}

#[test]
fn history_limits_are_bounded() {
    assert!(HistoryLimit::try_new(99).is_err());
    assert!(HistoryLimit::try_new(1_000_001).is_err());
    assert_eq!(limit(100).trim_target(), EntryCount::new(90));
    assert!(limit(100).is_exceeded_by(EntryCount::new(101)));
    assert!(!limit(100).is_exceeded_by(EntryCount::new(100)));
}
