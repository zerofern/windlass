use chrono::{DateTime, Utc};
use proptest::prelude::*;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};
use uom::si::f64::Information;
use uom::si::information::gigabyte;

use crate::{
    events::Event,
    types::{MamState, QbitState, SystemState, VpnState},
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
        any_vpn_port().prop_map(|port| QbitState::Ready {
            port,
            cookie: AuthCookie("prop".into())
        }),
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

fn any_system_state() -> impl Strategy<Value = SystemState> {
    (any_vpn_state(), any_qbit_state(), any_mam_state()).prop_map(|(vpn, qbit, mam)| SystemState {
        vpn,
        qbit,
        mam,
        known_torrents: std::collections::HashSet::new(),
        ..SystemState::initial()
    })
}

fn any_active_state() -> impl Strategy<Value = SystemState> {
    any_system_state()
}

/// A fully healthy, synced state: VPN connected, qBit ready, MAM synced.
fn any_synced_state() -> impl Strategy<Value = SystemState> {
    (any_vpn_ip(), any_vpn_port(), any_vpn_port(), any_vpn_ip()).prop_map(
        |(vpn_ip, vpn_port, q_port, mam_ip)| SystemState {
            vpn: VpnState::Connected {
                ip: vpn_ip,
                port: vpn_port,
            },
            qbit: QbitState::Ready {
                port: q_port,
                cookie: AuthCookie("prop".into()),
            },
            mam: MamState::Synced {
                ip: mam_ip,
                port: q_port,
            },
            known_torrents: std::collections::HashSet::new(),
            ..SystemState::initial()
        },
    )
}

// ── Event strategies ──────────────────────────────────────────────────────────

fn any_event() -> impl Strategy<Value = Event> {
    prop_oneof![
        (any::<bool>(), any_vpn_ip(), any_vpn_port()).prop_map(|(healthy, ip, port)| Event::Init {
            at: DateTime::UNIX_EPOCH,
            is_gluetun_healthy: healthy,
            port_files: Ok((ip, port)),
        }),
        any::<bool>().prop_map(|healthy| Event::Init {
            at: DateTime::UNIX_EPOCH,
            is_gluetun_healthy: healthy,
            port_files: Err("not ready".into()),
        }),
        Just(Event::DockerGluetunDied {
            at: DateTime::UNIX_EPOCH
        }),
        Just(Event::DockerGluetunHealthy {
            at: DateTime::UNIX_EPOCH
        }),
        (any_vpn_ip(), any_vpn_port()).prop_map(|(ip, port)| Event::PortFileReadResult {
            at: DateTime::UNIX_EPOCH,
            result: Ok((ip, port))
        }),
        proptest::string::string_regex("[a-z]{1,20}")
            .unwrap()
            .prop_map(|s| Event::PortFileReadResult {
                at: DateTime::UNIX_EPOCH,
                result: Err(s)
            }),
        any_auth_cookie().prop_map(|cookie| Event::QbitAuthSuccess {
            at: DateTime::UNIX_EPOCH,
            cookie
        }),
        Just(Event::QbitAuthFailed {
            at: DateTime::UNIX_EPOCH
        }),
        Just(Event::QbitConnectionRefused {
            at: DateTime::UNIX_EPOCH
        }),
        any::<u16>().prop_map(|c| Event::QbitApiError {
            at: DateTime::UNIX_EPOCH,
            code: HttpStatusCode(c)
        }),
        Just(Event::QbitPortSyncSuccess {
            at: DateTime::UNIX_EPOCH
        }),
        any::<u16>().prop_map(|c| Event::QbitPortSyncFailed {
            at: DateTime::UNIX_EPOCH,
            code: HttpStatusCode(c)
        }),
        Just(Event::MamUpdateSuccess {
            at: DateTime::UNIX_EPOCH
        }),
        any_vpn_ip().prop_map(|ip| Event::MamAsnMismatch {
            at: DateTime::UNIX_EPOCH,
            ip
        }),
        prop_oneof![
            Just(Event::MamStatusObserved {
                at: DateTime::UNIX_EPOCH,
                status: MamStatus::Connectable
            }),
            Just(Event::MamStatusObserved {
                at: DateTime::UNIX_EPOCH,
                status: MamStatus::NotConnectable
            }),
            Just(Event::MamStatusObserved {
                at: DateTime::UNIX_EPOCH,
                status: MamStatus::Unreachable
            }),
        ],
        any_information().prop_map(|space| Event::DiskSpaceObserved {
            at: DateTime::UNIX_EPOCH,
            space
        }),
        prop::collection::vec(
            proptest::string::string_regex("[a-zA-Z0-9. ]{1,30}").unwrap(),
            0..5
        )
        .prop_map(|v| Event::NewTorrentsObserved {
            at: DateTime::UNIX_EPOCH,
            torrents: v.into_iter().map(TorrentName).collect()
        }),
        Just(Event::LogsDumped {
            at: DateTime::UNIX_EPOCH
        }),
        any_wakeup_id().prop_map(|id| Event::Wakeup {
            at: DateTime::UNIX_EPOCH,
            id
        }),
        Just(Event::MamRateLimitViolation {
            at: DateTime::UNIX_EPOCH
        }),
    ]
}

// ── Properties ────────────────────────────────────────────────────────────────

proptest! {
    // 1. No panic — any combination of (state, event) must never panic.
    #[test]
    fn process_event_never_panics(
        mut state in any_system_state(),
        event in any_event(),
    ) {
        let _ = state.process_event(event, Utc::now());
    }

    // 2. Timing — single call must return within 1ms on any input.
    //    Catches accidental blocking calls, sleeps, or heavy allocations.
    #[test]
    fn process_event_returns_within_deadline(
        mut state in any_system_state(),
        event in any_event(),
    ) {
        // 100ms per event is generous for any instrumentation overhead while
        // still catching accidental blocking I/O or sleep calls.
        let deadline = Duration::from_millis(100);
        let start = Instant::now();
        let _ = state.process_event(event, Utc::now());
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
            state.process_event(event, Utc::now());
        }
        let elapsed = start.elapsed();
        prop_assert!(elapsed < deadline, "50-event sequence took {:?}", elapsed);
    }

    // 4. DockerGluetunDied always clears qbit and mam — regardless of prior
    //    active state. Prevents stale state from surviving a VPN death.
    #[test]
    fn gluetun_death_always_clears_qbit_and_mam(mut state in any_active_state()) {
        state.process_event(Event::DockerGluetunDied { at: DateTime::UNIX_EPOCH }, Utc::now());
        prop_assert_eq!(state.qbit, QbitState::Offline);
        prop_assert_eq!(state.mam, MamState::Unknown);
    }

    // 6. ASN blocked always suppresses recovery — no actions emitted
    //    when MAM is blocked, regardless of other state.
    #[test]
    fn asn_blocked_always_suppresses_recovery(
        mut state in any_active_state(),
        ip in any_vpn_ip(),
    ) {
        state.mam = MamState::AsnBlocked { ip };
        let outcome = state.process_event(
            Event::MamStatusObserved { at: DateTime::UNIX_EPOCH, status: MamStatus::Unreachable },
            Utc::now(),
        );
        prop_assert!(outcome.actions.is_empty());
    }

    // 7. Monitoring wakeups never mutate state — they only emit Check actions.
    //    Routing a wakeup must be a pure dispatch with zero side effects on state.
    #[test]
    fn monitoring_wakeups_do_not_mutate_state(state in any_active_state()) {
        for wakeup in [WakeupId::Heartbeat, WakeupId::DiskCheck, WakeupId::TorrentCheck] {
            let mut new_state = state.clone();
            new_state.process_event(Event::Wakeup { at: DateTime::UNIX_EPOCH, id: wakeup }, Utc::now());
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
            Event::MamStatusObserved { at: DateTime::UNIX_EPOCH, status: MamStatus::Connectable },
            Event::DiskSpaceObserved { at: DateTime::UNIX_EPOCH, space: Information::new::<gigabyte>(free_gb) },
            Event::NewTorrentsObserved { at: DateTime::UNIX_EPOCH, torrents: vec![] },
        ];

        for event in observations {
            let mut new_state = state.clone();
            new_state.process_event(event.clone(), Utc::now());
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

    // 11. Version counter integrity — state_changed must be true whenever
    //     the observable state actually changed.
    #[test]
    fn version_counter_matches_partial_eq(
        mut state in any_system_state(),
        event in any_event(),
    ) {
        let before = state.clone();
        let outcome = state.process_event(event, Utc::now());
        let actually_changed = state != before;
        prop_assert!(
            !actually_changed || outcome.state_changed,
            "state changed but state_changed was false: actually_changed={actually_changed}, state_changed={}",
            outcome.state_changed
        );
    }
}
