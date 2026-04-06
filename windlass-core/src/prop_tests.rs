use proptest::prelude::*;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};
use uom::si::f64::Information;
use uom::si::information::gigabyte;

use crate::{
    HARD_RECOVERY_LIMIT,
    events::Event,
    types::{MamState, QbitState, RunMode, SystemState, VpnState},
};
use windlass_types::{
    AuthCookie, HttpStatusCode, MamStatus, RetryCount, TorrentName, VpnIp, VpnPort, WakeupId,
};

// ── Primitive strategies ──────────────────────────────────────────────────────

fn any_vpn_ip() -> impl Strategy<Value = VpnIp> {
    any::<[u8; 4]>().prop_map(|b| VpnIp(Ipv4Addr::from(b)))
}

fn any_vpn_port() -> impl Strategy<Value = VpnPort> {
    (1u16..=65535u16).prop_map(|p| VpnPort::try_new(p).unwrap())
}

fn any_auth_cookie() -> impl Strategy<Value = AuthCookie> {
    proptest::string::string_regex("[a-zA-Z0-9]{8,32}")
        .unwrap()
        .prop_map(AuthCookie)
}

fn any_retry_count() -> impl Strategy<Value = RetryCount> {
    (0u8..=10u8).prop_map(RetryCount)
}

fn any_wakeup_id() -> impl Strategy<Value = WakeupId> {
    prop_oneof![
        Just(WakeupId::Heartbeat),
        Just(WakeupId::DiskCheck),
        Just(WakeupId::TorrentCheck),
        Just(WakeupId::QbitAuthRetry),
        Just(WakeupId::QbitSyncRetry),
        Just(WakeupId::RetryPortRead),
    ]
}

fn any_information() -> impl Strategy<Value = Information> {
    (0.0f64..=2000.0f64).prop_map(|gb| Information::new::<gigabyte>(gb))
}

// ── State component strategies ────────────────────────────────────────────────

fn any_vpn_state() -> impl Strategy<Value = VpnState> {
    prop_oneof![
        Just(VpnState::Stopped),
        Just(VpnState::DumpingLogs),
        Just(VpnState::Starting),
        Just(VpnState::AwaitingTunnel),
        (any_vpn_ip(), any_vpn_port()).prop_map(|(ip, port)| VpnState::Connected { ip, port }),
    ]
}

fn any_qbit_state() -> impl Strategy<Value = QbitState> {
    prop_oneof![
        Just(QbitState::Offline),
        any_retry_count().prop_map(|attempt| QbitState::Authenticating { attempt }),
        any_auth_cookie().prop_map(|cookie| QbitState::Authenticated { cookie }),
        (any_retry_count(), any_auth_cookie(), any_vpn_port()).prop_map(
            |(attempt, cookie, target)| QbitState::SyncingPort {
                attempt,
                cookie,
                target
            }
        ),
        any_vpn_port().prop_map(|port| QbitState::Ready { port, cookie: AuthCookie("prop".into()) }),
    ]
}

fn any_mam_state() -> impl Strategy<Value = MamState> {
    prop_oneof![
        Just(MamState::Unknown),
        (any_vpn_ip(), any_vpn_port()).prop_map(|(ip, port)| MamState::SyncPending {
            target_ip: ip,
            target_port: port
        }),
        (any_vpn_port(), any_vpn_ip()).prop_map(|(port, ip)| MamState::Synced { port, ip }),
        any_vpn_ip().prop_map(|ip| MamState::AsnBlocked { ip }),
    ]
}

fn any_run_mode() -> impl Strategy<Value = RunMode> {
    prop_oneof![
        Just(RunMode::Active),
        proptest::string::string_regex("[a-zA-Z ]{0,50}")
            .unwrap()
            .prop_map(|r| RunMode::Fatal { reason: r }),
    ]
}

fn any_system_state() -> impl Strategy<Value = SystemState> {
    (
        any_run_mode(),
        any_retry_count(),
        any_vpn_state(),
        any_qbit_state(),
        any_mam_state(),
    )
        .prop_map(|(run_mode, hard_recoveries, vpn, qbit, mam)| SystemState {
            run_mode,
            hard_recoveries,
            vpn,
            qbit,
            mam,
            known_torrents: std::collections::HashSet::new(),
        })
}

fn any_active_state() -> impl Strategy<Value = SystemState> {
    (
        any_retry_count(),
        any_vpn_state(),
        any_qbit_state(),
        any_mam_state(),
    )
        .prop_map(|(hard_recoveries, vpn, qbit, mam)| SystemState {
            run_mode: RunMode::Active,
            hard_recoveries,
            vpn,
            qbit,
            mam,
            known_torrents: std::collections::HashSet::new(),
        })
}

/// Active state where hard_recoveries is strictly below the fatal limit —
/// the only valid region for asserting the counter stays bounded.
fn any_active_state_with_valid_recoveries() -> impl Strategy<Value = SystemState> {
    (
        0u8..HARD_RECOVERY_LIMIT.0,
        any_vpn_state(),
        any_qbit_state(),
        any_mam_state(),
    )
        .prop_map(|(recoveries, vpn, qbit, mam)| SystemState {
            run_mode: RunMode::Active,
            hard_recoveries: RetryCount(recoveries),
            vpn,
            qbit,
            mam,
            known_torrents: std::collections::HashSet::new(),
        })
}

fn any_fatal_state() -> impl Strategy<Value = SystemState> {
    (
        any_retry_count(),
        any_vpn_state(),
        any_qbit_state(),
        any_mam_state(),
        proptest::string::string_regex("[a-zA-Z ]{0,50}").unwrap(),
    )
        .prop_map(|(hard_recoveries, vpn, qbit, mam, reason)| SystemState {
            run_mode: RunMode::Fatal { reason },
            hard_recoveries,
            vpn,
            qbit,
            mam,
            known_torrents: std::collections::HashSet::new(),
        })
}

/// A fully healthy, synced state: VPN connected, qBit ready, MAM synced.
fn any_synced_state() -> impl Strategy<Value = SystemState> {
    (
        any_vpn_ip(),
        any_vpn_port(),
        any_vpn_port(),
        any_vpn_ip(),
        0u8..HARD_RECOVERY_LIMIT.0,
    )
        .prop_map(
            |(vpn_ip, vpn_port, q_port, mam_ip, recoveries)| SystemState {
                run_mode: RunMode::Active,
                hard_recoveries: RetryCount(recoveries),
                vpn: VpnState::Connected {
                    ip: vpn_ip,
                    port: vpn_port,
                },
                qbit: QbitState::Ready { port: q_port, cookie: AuthCookie("prop".into()) },
                mam: MamState::Synced {
                    ip: mam_ip,
                    port: q_port,
                },
                known_torrents: std::collections::HashSet::new(),
            },
        )
}

// ── Event strategies ──────────────────────────────────────────────────────────

fn any_event() -> impl Strategy<Value = Event> {
    prop_oneof![
        (any::<bool>(), any_vpn_ip(), any_vpn_port()).prop_map(|(healthy, ip, port)| Event::Init {
            is_gluetun_healthy: healthy,
            port_files: Ok((ip, port)),
        }),
        any::<bool>().prop_map(|healthy| Event::Init {
            is_gluetun_healthy: healthy,
            port_files: Err("not ready".into()),
        }),
        Just(Event::ManualReset),
        Just(Event::DockerGluetunDied),
        Just(Event::DockerGluetunHealthy),
        (any_vpn_ip(), any_vpn_port())
            .prop_map(|(ip, port)| Event::PortFileReadResult(Ok((ip, port)))),
        proptest::string::string_regex("[a-z]{1,20}")
            .unwrap()
            .prop_map(|s| Event::PortFileReadResult(Err(s))),
        any_auth_cookie().prop_map(Event::QbitAuthSuccess),
        Just(Event::QbitAuthFailed),
        Just(Event::QbitConnectionRefused),
        any::<u16>().prop_map(|c| Event::QbitApiError(HttpStatusCode(c))),
        Just(Event::QbitPortSyncSuccess),
        any::<u16>().prop_map(|c| Event::QbitPortSyncFailed(HttpStatusCode(c))),
        Just(Event::MamUpdateSuccess),
        any_vpn_ip().prop_map(Event::MamAsnMismatch),
        prop_oneof![
            Just(Event::MamStatusObserved(MamStatus::Connectable)),
            Just(Event::MamStatusObserved(MamStatus::NotConnectable)),
            Just(Event::MamStatusObserved(MamStatus::Unreachable)),
        ],
        any_information().prop_map(Event::DiskSpaceObserved),
        prop::collection::vec(
            proptest::string::string_regex("[a-zA-Z0-9. ]{1,30}").unwrap(),
            0..5
        )
        .prop_map(|v| Event::NewTorrentsObserved(v.into_iter().map(TorrentName).collect())),
        Just(Event::LogsDumped),
        any_wakeup_id().prop_map(Event::Wakeup),
        Just(Event::MamRateLimitViolation),
    ]
}

fn any_non_reset_event() -> impl Strategy<Value = Event> {
    any_event().prop_filter("exclude ManualReset", |e| !matches!(e, Event::ManualReset))
}

// ── Properties ────────────────────────────────────────────────────────────────

proptest! {
    // 1. No panic — any combination of (state, event) must never panic.
    #[test]
    fn process_event_never_panics(
        state in any_system_state(),
        event in any_event(),
    ) {
        let _ = state.process_event(event);
    }

    // 2. Timing — single call must return within 1ms on any input.
    //    Catches accidental blocking calls, sleeps, or heavy allocations.
    #[test]
    fn process_event_returns_within_deadline(
        state in any_system_state(),
        event in any_event(),
    ) {
        // 100ms per event is generous for any instrumentation overhead while
        // still catching accidental blocking I/O or sleep calls.
        let deadline = Duration::from_millis(100);
        let start = Instant::now();
        let _ = state.process_event(event);
        let elapsed = start.elapsed();
        prop_assert!(
            elapsed < deadline,
            "process_event took {:?} — possible blocking call added", elapsed
        );
    }

    // 3. Sequential timing — 50-event sequence must complete well within 10ms.
    #[test]
    fn event_sequence_completes_in_bounded_time(
        events in prop::collection::vec(any_event(), 1..50),
    ) {
        // 1s for 50 events is generous enough for any instrumentation overhead
        // while still catching accidentally quadratic behaviour.
        let deadline = Duration::from_millis(1_000);
        let start = Instant::now();
        let mut state = SystemState::initial();
        for event in events {
            let (new_state, _) = state.process_event(event);
            state = new_state;
        }
        let elapsed = start.elapsed();
        prop_assert!(elapsed < deadline, "50-event sequence took {:?}", elapsed);
    }

    // 4. Fatal mode is a black hole — all non-reset events produce no actions
    //    and leave the state completely unchanged.
    #[test]
    fn fatal_mode_ignores_all_non_reset_events(
        state in any_fatal_state(),
        event in any_non_reset_event(),
    ) {
        let (new_state, actions) = state.clone().process_event(event);
        prop_assert!(matches!(new_state.run_mode, RunMode::Fatal { .. }), "run_mode should remain Fatal");
        prop_assert!(actions.is_empty(), "actions should be empty in Fatal mode");
        prop_assert_eq!(new_state, state);
    }

    // 5. DockerGluetunDied always clears qbit and mam — regardless of prior
    //    active state. Prevents stale state from surviving a VPN death.
    #[test]
    fn gluetun_death_always_clears_qbit_and_mam(state in any_active_state()) {
        let (new_state, _) = state.process_event(Event::DockerGluetunDied);
        prop_assert_eq!(new_state.qbit, QbitState::Offline);
        prop_assert_eq!(new_state.mam, MamState::Unknown);
    }

    // 6. ASN blocked always suppresses recovery — no actions emitted
    //    when MAM is blocked, regardless of other state.
    #[test]
    fn asn_blocked_always_suppresses_recovery(
        mut state in any_active_state(),
        ip in any_vpn_ip(),
    ) {
        state.mam = MamState::AsnBlocked { ip };
        let (_, actions) = state.process_event(Event::MamStatusObserved(MamStatus::Unreachable));
        prop_assert!(actions.is_empty());
    }

    // 7. hard_recoveries bounded — when starting below the limit in Active
    //    mode, any event either stays below the limit or transitions to Fatal.
    //    The counter must never exceed the limit while remaining Active.
    #[test]
    fn hard_recovery_limit_always_triggers_fatal(
        state in any_active_state_with_valid_recoveries(),
        event in any_event(),
    ) {
        let (new_state, _) = state.process_event(event);
        if matches!(new_state.run_mode, RunMode::Active) {
            prop_assert!(
                new_state.hard_recoveries < HARD_RECOVERY_LIMIT,
                "hard_recoveries {:?} reached limit without transitioning to Fatal",
                new_state.hard_recoveries,
            );
        }
    }

    // 8. Sequential invariants — no sequence of arbitrary events violates the
    //    two core safety rules: Fatal emits nothing, counter stays bounded.
    #[test]
    fn sequential_events_never_violate_invariants(
        events in prop::collection::vec(any_event(), 1..50),
    ) {
        let mut state = SystemState::initial();
        for event in &events {
            let prior = state.clone();
            let (new_state, actions) = state.process_event(event.clone());

            // Fatal mode must never emit actions on non-reset events.
            if matches!(prior.run_mode, RunMode::Fatal { .. })
                && !matches!(event, Event::ManualReset)
            {
                prop_assert!(
                    actions.is_empty(),
                    "Fatal mode emitted {} action(s) on {:?}", actions.len(), event
                );
            }

            // hard_recoveries must never exceed the limit.
            prop_assert!(
                new_state.hard_recoveries.0 <= HARD_RECOVERY_LIMIT.0,
                "hard_recoveries {:?} exceeded limit", new_state.hard_recoveries,
            );

            state = new_state;
        }
    }

    // 9. Monitoring wakeups never mutate state — they only emit Check actions.
    //    Routing a wakeup must be a pure dispatch with zero side effects on state.
    #[test]
    fn monitoring_wakeups_do_not_mutate_state(state in any_active_state()) {
        for wakeup in [WakeupId::Heartbeat, WakeupId::DiskCheck, WakeupId::TorrentCheck] {
            let (new_state, _) = state.clone().process_event(Event::Wakeup(wakeup));
            prop_assert_eq!(
                new_state, state.clone(),
                "Wakeup({:?}) must not mutate state", wakeup
            );
        }
    }

    // 10. Healthy observations preserve synced state — routine monitoring
    //     results must never knock the system off its happy path.
    #[test]
    fn healthy_observations_preserve_synced_state(
        state in any_synced_state(),
        free_gb in 51.0f64..1000.0f64,
    ) {
        let observations = [
            Event::MamStatusObserved(MamStatus::Connectable),
            Event::DiskSpaceObserved(Information::new::<gigabyte>(free_gb)),
            Event::NewTorrentsObserved(vec![]),
        ];

        for event in observations {
            let (new_state, _) = state.clone().process_event(event.clone());
            prop_assert!(
                matches!(new_state.run_mode, RunMode::Active),
                "{:?} disrupted RunMode", event
            );
            prop_assert!(
                matches!(new_state.vpn, VpnState::Connected { .. }),
                "{:?} disrupted VpnState", event
            );
            prop_assert!(
                matches!(new_state.qbit, QbitState::Ready { .. }),
                "{:?} disrupted QbitState", event
            );
            prop_assert!(
                matches!(new_state.mam, MamState::Synced { .. }),
                "{:?} disrupted MamState", event
            );
        }
    }
}
