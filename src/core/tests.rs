use super::{actions::Action, events::Event, process_event, types::*};
use crate::types::{AlertPriority, AuthCookie, RetryCount, VpnIp, VpnPort, WakeupId};
use std::net::Ipv4Addr;
use std::time::Duration;
use uom::si::f64::Information;
use uom::si::information::gigabyte;

fn ip() -> VpnIp {
    VpnIp(Ipv4Addr::new(10, 8, 0, 1))
}

fn port() -> VpnPort {
    VpnPort::try_new(51820).unwrap()
}

fn cookie() -> AuthCookie {
    AuthCookie("sid=abc".into())
}

fn connected_state() -> SystemState {
    SystemState {
        vpn: VpnState::Connected { ip: ip(), port: port() },
        qbit: QbitState::Ready { port: port() },
        mam: MamState::Synced { port: port(), ip: ip() },
        ..SystemState::initial()
    }
}

// ── Init ──────────────────────────────────────────────────────────────────────

#[test]
fn init_healthy_with_files_fast_forwards_to_connected_and_auths() {
    let (new_state, actions) = process_event(
        SystemState::initial(),
        Event::Init { is_gluetun_healthy: true, port_files: Ok((ip(), port())) },
    );
    assert!(matches!(new_state.vpn, VpnState::Connected { .. }));
    assert!(matches!(new_state.qbit, QbitState::Authenticating { attempt: RetryCount(0) }));
    assert!(actions.iter().any(|a| matches!(a, Action::AuthenticateQbit)));
    let wakeup_ids: Vec<_> = actions
        .iter()
        .filter_map(|a| match a {
            Action::ScheduleWakeup(id, _) => Some(id),
            _ => None,
        })
        .collect();
    assert!(wakeup_ids.contains(&&WakeupId::Heartbeat));
    assert!(wakeup_ids.contains(&&WakeupId::DiskCheck));
    assert!(wakeup_ids.contains(&&WakeupId::TorrentCheck));
}

#[test]
fn init_healthy_without_files_waits_for_watcher() {
    let (new_state, actions) = process_event(
        SystemState::initial(),
        Event::Init { is_gluetun_healthy: true, port_files: Err("not ready".into()) },
    );
    assert_eq!(new_state.vpn, VpnState::AwaitingTunnel);
    assert!(!actions.iter().any(|a| matches!(a, Action::AuthenticateQbit)));
}

#[test]
fn init_unhealthy_triggers_workflow_a() {
    let (new_state, actions) = process_event(
        SystemState::initial(),
        Event::Init { is_gluetun_healthy: false, port_files: Err("n/a".into()) },
    );
    assert_eq!(new_state.vpn, VpnState::DumpingLogs);
    assert!(actions.iter().any(|a| matches!(a, Action::FetchAndDumpAllLogs)));
}

// ── ManualReset ───────────────────────────────────────────────────────────────

#[test]
fn manual_reset_in_active_mode_clears_recovery_counter() {
    let mut state = SystemState::initial();
    state.hard_recoveries = RetryCount(2);
    let (new_state, actions) = process_event(state, Event::ManualReset);
    assert_eq!(new_state.hard_recoveries, RetryCount(0));
    assert!(actions.is_empty());
}

// ── Workflow A ────────────────────────────────────────────────────────────────

#[test]
fn unexpected_vpn_death_dumps_logs() {
    let state = connected_state();
    let (new_state, actions) = process_event(state, Event::DockerGluetunDied);
    assert_eq!(new_state.vpn, VpnState::DumpingLogs);
    assert!(actions.iter().any(|a| matches!(a, Action::FetchAndDumpAllLogs)));
}

#[test]
fn death_from_awaiting_tunnel_dumps_logs() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::AwaitingTunnel;
    let (new_state, actions) = process_event(state, Event::DockerGluetunDied);
    assert_eq!(new_state.vpn, VpnState::DumpingLogs);
    assert!(actions.iter().any(|a| matches!(a, Action::FetchAndDumpAllLogs)));
}

#[test]
fn death_from_stopped_is_noop_for_vpn() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Stopped;
    let (new_state, actions) = process_event(state, Event::DockerGluetunDied);
    assert_eq!(new_state.vpn, VpnState::Stopped);
    assert!(!actions.iter().any(|a| matches!(a, Action::FetchAndDumpAllLogs)));
    assert!(!actions.iter().any(|a| matches!(a, Action::StopDependentContainers)));
}

#[test]
fn unexpected_death_resets_qbit_and_mam() {
    let state = connected_state();
    let (new_state, _) = process_event(state, Event::DockerGluetunDied);
    assert_eq!(new_state.qbit, QbitState::Offline);
    assert_eq!(new_state.mam, MamState::Unknown);
}

#[test]
fn logs_dumped_stops_containers_and_restarts_gluetun() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::DumpingLogs;
    let (new_state, actions) = process_event(state, Event::LogsDumped);
    assert_eq!(new_state.vpn, VpnState::Starting);
    assert!(actions.iter().any(|a| matches!(a, Action::StopDependentContainers)));
    assert!(actions.iter().any(|a| matches!(a, Action::RestartGluetun)));
}

#[test]
fn double_dump_guard_skips_dump_when_starting() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Starting;
    let (_, actions) = process_event(state, Event::DockerGluetunDied);
    assert!(!actions.iter().any(|a| matches!(a, Action::FetchAndDumpAllLogs)));
    assert!(actions.iter().any(|a| matches!(a, Action::StopDependentContainers)));
}

#[test]
fn double_dump_guard_skips_dump_when_dumping_logs() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::DumpingLogs;
    let (_, actions) = process_event(state, Event::DockerGluetunDied);
    assert!(!actions.iter().any(|a| matches!(a, Action::FetchAndDumpAllLogs)));
}

#[test]
fn gluetun_healthy_starts_containers() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Starting;
    let (new_state, actions) = process_event(state, Event::DockerGluetunHealthy);
    assert_eq!(new_state.vpn, VpnState::AwaitingTunnel);
    assert!(actions.iter().any(|a| matches!(a, Action::StartDependentContainers)));
}

// ── Workflow B ────────────────────────────────────────────────────────────────

// ── Workflow B ────────────────────────────────────────────────────────────────

#[test]
fn port_file_read_ok_authenticates_qbit() {
    let (new_state, actions) =
        process_event(SystemState::initial(), Event::PortFileReadResult(Ok((ip(), port()))));
    assert!(matches!(new_state.qbit, QbitState::Authenticating { attempt: RetryCount(0) }));
    assert!(actions.iter().any(|a| matches!(a, Action::AuthenticateQbit)));
}

#[test]
fn port_file_read_same_values_is_noop() {
    // Core ignores no-change reads — debouncer may fire even when content is unchanged.
    let (_, actions) = process_event(
        connected_state(),
        Event::PortFileReadResult(Ok((ip(), port()))),
    );
    assert!(actions.is_empty(), "identical ip+port must produce no actions");
}

#[test]
fn port_file_read_new_port_triggers_reauth() {
    let new_port = VpnPort::try_new(51821).unwrap();
    let (new_state, actions) = process_event(
        connected_state(),
        Event::PortFileReadResult(Ok((ip(), new_port))),
    );
    assert!(matches!(new_state.qbit, QbitState::Authenticating { .. }));
    assert!(actions.iter().any(|a| matches!(a, Action::AuthenticateQbit)));
}

#[test]
fn port_file_read_err_schedules_retry() {
    let (_, actions) =
        process_event(SystemState::initial(), Event::PortFileReadResult(Err("partial".into())));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::RetryPortRead, _))));
}

#[test]
fn connection_refused_is_ignored_when_not_authenticating() {
    let (_, actions) = process_event(connected_state(), Event::QbitConnectionRefused);
    assert!(actions.is_empty(), "stale QbitConnectionRefused must produce no actions");
}

#[test]
fn qbit_auth_retry_wakeup_is_ignored_when_not_authenticating() {
    let (_, actions) = process_event(connected_state(), Event::Wakeup(WakeupId::QbitAuthRetry));
    assert!(!actions.iter().any(|a| matches!(a, Action::AuthenticateQbit)),
        "QbitAuthRetry wakeup must be ignored when qBit is already Ready");
}

#[test]
fn qbit_auth_success_starts_port_sync_when_connected() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected { ip: ip(), port: port() };
    state.qbit = QbitState::Authenticating { attempt: RetryCount(0) };
    let (new_state, actions) = process_event(state, Event::QbitAuthSuccess(cookie()));
    assert!(matches!(new_state.qbit, QbitState::SyncingPort { attempt: RetryCount(0), .. }));
    assert!(actions.iter().any(|a| matches!(a, Action::SyncQbitPort(_, _))));
}

#[test]
fn qbit_auth_success_stores_cookie_when_vpn_not_yet_connected() {
    // Auth completes before the port file is read (race condition edge case).
    let mut state = SystemState::initial();
    state.vpn = VpnState::AwaitingTunnel;
    let (new_state, actions) = process_event(state, Event::QbitAuthSuccess(cookie()));
    assert!(matches!(new_state.qbit, QbitState::Authenticated { .. }));
    assert!(!actions.iter().any(|a| matches!(a, Action::SyncQbitPort(_, _))));
}

#[test]
fn qbit_auth_failed_emits_critical_alert_and_schedules_retry() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::Authenticating { attempt: RetryCount(0) };
    let (new_state, actions) = process_event(state, Event::QbitAuthFailed);
    // Credentials rejected: alert immediately, reset to attempt 0.
    assert!(matches!(new_state.qbit, QbitState::Authenticating { attempt: RetryCount(0) }));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::SendGotifyAlert(AlertPriority::Critical, _))));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::QbitAuthRetry, _))));
}

#[test]
fn qbit_connection_refused_schedules_silent_retry() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::Authenticating { attempt: RetryCount(0) };
    let (new_state, actions) = process_event(state, Event::QbitConnectionRefused);
    // Connection refused is normal startup — no alert, no attempt increment.
    assert!(matches!(new_state.qbit, QbitState::Authenticating { attempt: RetryCount(0) }));
    assert!(!actions.iter().any(|a| matches!(a, Action::SendGotifyAlert(_, _))));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::QbitAuthRetry, _))));
    // Fixed short delay, not exponential.
    let delay = actions.iter().find_map(|a| match a {
        Action::ScheduleWakeup(WakeupId::QbitAuthRetry, d) => Some(*d),
        _ => None,
    });
    assert_eq!(delay, Some(Duration::from_secs(5)));
}

#[test]
fn qbit_api_error_schedules_exponential_backoff_retry() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::Authenticating { attempt: RetryCount(0) };
    let (new_state, actions) = process_event(state, Event::QbitApiError(403));
    assert!(matches!(new_state.qbit, QbitState::Authenticating { attempt: RetryCount(1) }));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::QbitAuthRetry, _))));
}

#[test]
fn qbit_auth_failed_when_not_authenticating_stays_at_attempt_zero() {
    // Stale response arrives after state machine moved on — doesn't increment.
    let mut state = SystemState::initial();
    state.qbit = QbitState::Offline;
    let (new_state, actions) = process_event(state, Event::QbitAuthFailed);
    assert!(matches!(new_state.qbit, QbitState::Authenticating { attempt: RetryCount(0) }));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::QbitAuthRetry, _))));
}

#[test]
fn qbit_api_error_backoff_is_exponential() {
    // attempt 0 → base * 2^0 = 2s; attempt 1 → 4s; attempt 2 → 8s
    for (attempt, expected_secs) in [(0u8, 2u64), (1, 4), (2, 8), (3, 16)] {
        let mut state = SystemState::initial();
        state.qbit = QbitState::Authenticating { attempt: RetryCount(attempt) };
        let (_, actions) = process_event(state, Event::QbitApiError(500));
        let backoff = actions.iter().find_map(|a| match a {
            Action::ScheduleWakeup(WakeupId::QbitAuthRetry, d) => Some(*d),
            _ => None,
        });
        assert_eq!(
            backoff,
            Some(Duration::from_secs(expected_secs)),
            "attempt {attempt} should have backoff {expected_secs}s"
        );
    }
}

#[test]
fn qbit_port_sync_success_updates_mam() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected { ip: ip(), port: port() };
    state.qbit = QbitState::SyncingPort { attempt: RetryCount(0), cookie: cookie(), target: port() };
    let (new_state, actions) = process_event(state, Event::QbitPortSyncSuccess);
    assert!(matches!(new_state.qbit, QbitState::Ready { .. }));
    assert!(matches!(new_state.mam, MamState::SyncPending { .. }), "mam should be SyncPending while UpdateMam is in flight");
    assert!(actions.iter().any(|a| matches!(a, Action::UpdateMam(_))));
}

#[test]
fn qbit_port_sync_success_is_noop_when_vpn_not_connected() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Stopped;
    state.qbit = QbitState::SyncingPort { attempt: RetryCount(0), cookie: cookie(), target: port() };
    let (new_state, actions) = process_event(state, Event::QbitPortSyncSuccess);
    // Should not transition to Ready or emit UpdateMam
    assert!(!matches!(new_state.qbit, QbitState::Ready { .. }));
    assert!(!actions.iter().any(|a| matches!(a, Action::UpdateMam(_))));
}

#[test]
fn qbit_port_sync_success_is_noop_when_not_syncing() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected { ip: ip(), port: port() };
    state.qbit = QbitState::Authenticated { cookie: cookie() };
    let (_, actions) = process_event(state, Event::QbitPortSyncSuccess);
    assert!(!actions.iter().any(|a| matches!(a, Action::UpdateMam(_))));
}

#[test]
fn qbit_port_sync_failed_retries_under_limit() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::SyncingPort { attempt: RetryCount(0), cookie: cookie(), target: port() };
    let (new_state, actions) = process_event(state, Event::QbitPortSyncFailed(500));
    assert!(matches!(new_state.qbit, QbitState::SyncingPort { attempt: RetryCount(1), .. }));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::QbitSyncRetry, _))));
}

#[test]
fn qbit_port_sync_failed_falls_back_at_limit() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::SyncingPort { attempt: RetryCount(3), cookie: cookie(), target: port() };
    let (new_state, actions) = process_event(state, Event::QbitPortSyncFailed(500));
    assert!(matches!(new_state.qbit, QbitState::Authenticating { attempt: RetryCount(0) }));
    assert!(actions.iter().any(|a| matches!(a, Action::AuthenticateQbit)));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::SendGotifyAlert(AlertPriority::Warning, _))));
}

#[test]
fn qbit_port_sync_failed_is_noop_when_not_syncing() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::Offline;
    let (new_state, actions) = process_event(state, Event::QbitPortSyncFailed(503));
    assert_eq!(new_state.qbit, QbitState::Offline);
    assert!(actions.is_empty());
}

// ── MAM ───────────────────────────────────────────────────────────────────────

#[test]
fn mam_update_success_sends_ok_alert() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected { ip: ip(), port: port() };
    let (new_state, actions) = process_event(state, Event::MamUpdateSuccess);
    assert!(matches!(new_state.mam, MamState::Synced { .. }));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::SendGotifyAlert(AlertPriority::Info, _))));
}

#[test]
fn mam_update_success_is_noop_when_vpn_not_connected() {
    let (new_state, actions) = process_event(SystemState::initial(), Event::MamUpdateSuccess);
    assert_eq!(new_state.mam, MamState::Unknown);
    assert!(actions.is_empty());
}

#[test]
fn mam_asn_mismatch_blocks_and_alerts_with_ip() {
    let (new_state, actions) = process_event(connected_state(), Event::MamAsnMismatch(ip()));
    assert!(matches!(new_state.mam, MamState::AsnBlocked { .. }));
    let alert = actions.iter().find_map(|a| match a {
        Action::SendGotifyAlert(AlertPriority::Critical, msg) => Some(msg.clone()),
        _ => None,
    });
    assert!(alert.is_some(), "expected a Critical alert");
    assert!(
        alert.unwrap().contains("10.8.0.1"),
        "alert should include the mismatched IP"
    );
}

// ── Workflow C ────────────────────────────────────────────────────────────────

#[test]
fn connectable_resets_hard_recoveries() {
    let mut state = connected_state();
    state.hard_recoveries = RetryCount(2);
    let (new_state, _) = process_event(state, Event::MamConnectabilityObserved(true));
    assert_eq!(new_state.hard_recoveries, RetryCount(0));
}

#[test]
fn connectable_rearms_heartbeat() {
    let (_, actions) = process_event(connected_state(), Event::MamConnectabilityObserved(true));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::Heartbeat, _))));
}

#[test]
fn soft_recovery_from_ready_re_auths_qbit() {
    let state = connected_state(); // qbit is Ready
    let (new_state, actions) = process_event(state, Event::MamConnectabilityObserved(false));
    assert!(matches!(new_state.qbit, QbitState::Authenticating { attempt: RetryCount(0) }));
    assert!(actions.iter().any(|a| matches!(a, Action::AuthenticateQbit)));
}

#[test]
fn soft_recovery_from_authenticated_re_auths_qbit() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected { ip: ip(), port: port() };
    state.qbit = QbitState::Authenticated { cookie: cookie() };
    let (new_state, actions) = process_event(state, Event::MamConnectabilityObserved(false));
    assert!(matches!(new_state.qbit, QbitState::Authenticating { attempt: RetryCount(0) }));
    assert!(actions.iter().any(|a| matches!(a, Action::AuthenticateQbit)));
}

#[test]
fn soft_recovery_rearms_heartbeat() {
    let state = connected_state();
    let (_, actions) = process_event(state, Event::MamConnectabilityObserved(false));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::Heartbeat, _))));
}

#[test]
fn hard_recovery_increments_counter_and_dumps_logs() {
    let mut state = SystemState::initial();
    state.vpn = VpnState::Connected { ip: ip(), port: port() };
    state.qbit = QbitState::Offline;
    let (new_state, actions) = process_event(state, Event::MamConnectabilityObserved(false));
    assert_eq!(new_state.hard_recoveries, RetryCount(1));
    assert_eq!(new_state.vpn, VpnState::DumpingLogs);
    assert!(actions.iter().any(|a| matches!(a, Action::FetchAndDumpAllLogs)));
}

#[test]
fn hard_recovery_escalates_from_authenticating_in_flight() {
    // If auth is already in flight, soft recovery is considered attempted.
    let mut state = SystemState::initial();
    state.qbit = QbitState::Authenticating { attempt: RetryCount(1) };
    let (new_state, actions) = process_event(state, Event::MamConnectabilityObserved(false));
    assert_eq!(new_state.hard_recoveries, RetryCount(1));
    assert!(actions.iter().any(|a| matches!(a, Action::FetchAndDumpAllLogs)));
}

#[test]
fn hard_recovery_escalates_from_syncing_in_flight() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::SyncingPort { attempt: RetryCount(0), cookie: cookie(), target: port() };
    let (new_state, _) = process_event(state, Event::MamConnectabilityObserved(false));
    assert_eq!(new_state.hard_recoveries, RetryCount(1));
}

#[test]
fn death_loop_prevention_transitions_to_fatal() {
    let mut state = SystemState::initial();
    state.hard_recoveries = RetryCount(2);
    state.qbit = QbitState::Offline;
    let (new_state, actions) = process_event(state, Event::MamConnectabilityObserved(false));
    assert!(matches!(new_state.run_mode, RunMode::Fatal { .. }));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::SendGotifyAlert(AlertPriority::Critical, _))));
}

#[test]
fn death_loop_does_not_dump_logs_on_fatal_transition() {
    // When we hit the limit, we halt — no further recovery actions.
    let mut state = SystemState::initial();
    state.hard_recoveries = RetryCount(2);
    state.qbit = QbitState::Offline;
    let (_, actions) = process_event(state, Event::MamConnectabilityObserved(false));
    assert!(!actions.iter().any(|a| matches!(a, Action::FetchAndDumpAllLogs)));
    assert!(!actions.iter().any(|a| matches!(a, Action::RestartGluetun)));
}

#[test]
fn asn_blocked_suppresses_recovery() {
    let mut state = connected_state();
    state.mam = MamState::AsnBlocked { ip: ip() };
    let (new_state, actions) =
        process_event(state.clone(), Event::MamConnectabilityObserved(false));
    assert_eq!(new_state.qbit, state.qbit);
    assert!(actions.is_empty());
}

#[test]
fn fatal_mode_ignores_all_events_except_reset() {
    let mut state = SystemState::initial();
    state.run_mode = RunMode::Fatal { reason: "test".into() };
    let (new_state, actions) = process_event(state.clone(), Event::DockerGluetunDied);
    assert!(matches!(new_state.run_mode, RunMode::Fatal { .. }));
    assert!(actions.is_empty());
}

#[test]
fn manual_reset_clears_fatal_mode_and_restarts_gluetun() {
    let mut state = SystemState::initial();
    state.run_mode = RunMode::Fatal { reason: "test".into() };
    state.hard_recoveries = RetryCount(3);
    let (new_state, actions) = process_event(state, Event::ManualReset);
    assert_eq!(new_state.run_mode, RunMode::Active);
    assert_eq!(new_state.hard_recoveries, RetryCount(0));
    assert_eq!(new_state.vpn, VpnState::Starting);
    assert!(actions.iter().any(|a| matches!(a, Action::RestartGluetun)));
}

// ── Monitoring ────────────────────────────────────────────────────────────────

#[test]
fn low_disk_space_sends_alert() {
    let space = Information::new::<gigabyte>(20.0);
    let (_, actions) = process_event(SystemState::initial(), Event::DiskSpaceObserved(space));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::SendGotifyAlert(AlertPriority::Warning, _))));
}

#[test]
fn sufficient_disk_space_sends_no_alert() {
    let space = Information::new::<gigabyte>(200.0);
    let (_, actions) = process_event(SystemState::initial(), Event::DiskSpaceObserved(space));
    assert!(!actions.iter().any(|a| matches!(a, Action::SendGotifyAlert(_, _))));
}

#[test]
fn disk_check_always_reschedules() {
    for gb in [20.0_f64, 200.0] {
        let space = Information::new::<gigabyte>(gb);
        let (_, actions) = process_event(SystemState::initial(), Event::DiskSpaceObserved(space));
        assert!(
            actions.iter().any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::DiskCheck, _))),
            "DiskCheck wakeup not rescheduled for {gb} GB"
        );
    }
}

#[test]
fn new_torrents_sends_alert_for_unseen_names() {
    // known_torrents is empty — all names are new.
    let names = vec!["Ubuntu.iso".into(), "Fedora.iso".into()];
    let (new_state, actions) =
        process_event(SystemState::initial(), Event::NewTorrentsObserved(names));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::SendGotifyAlert(AlertPriority::Info, _))));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::TorrentCheck, _))));
    // Core remembers them for next time.
    assert!(new_state.known_torrents.contains("Ubuntu.iso"));
    assert!(new_state.known_torrents.contains("Fedora.iso"));
}

#[test]
fn already_known_torrents_send_no_alert() {
    let mut state = SystemState::initial();
    state.known_torrents.insert("Ubuntu.iso".into());
    state.known_torrents.insert("Fedora.iso".into());
    // Same list, nothing new — no alert.
    let names = vec!["Ubuntu.iso".into(), "Fedora.iso".into()];
    let (_, actions) = process_event(state, Event::NewTorrentsObserved(names));
    assert!(!actions.iter().any(|a| matches!(a, Action::SendGotifyAlert(_, _))));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::TorrentCheck, _))));
}

#[test]
fn mixed_known_and_new_torrents_alerts_only_for_new() {
    let mut state = SystemState::initial();
    state.known_torrents.insert("Ubuntu.iso".into());
    let names = vec!["Ubuntu.iso".into(), "Debian.iso".into()];
    let (new_state, actions) = process_event(state, Event::NewTorrentsObserved(names));
    // Only Debian.iso is new — alert should mention it.
    let alert = actions.iter().find_map(|a| match a {
        Action::SendGotifyAlert(AlertPriority::Info, msg) => Some(msg.clone()),
        _ => None,
    });
    assert!(alert.is_some(), "Expected an alert for the new torrent");
    assert!(alert.unwrap().contains("Debian.iso"));
    assert!(new_state.known_torrents.contains("Ubuntu.iso"));
    assert!(new_state.known_torrents.contains("Debian.iso"));
}

#[test]
fn empty_torrent_list_sends_no_alert_but_reschedules() {
    let (_, actions) =
        process_event(SystemState::initial(), Event::NewTorrentsObserved(vec![]));
    assert!(!actions.iter().any(|a| matches!(a, Action::SendGotifyAlert(_, _))));
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::ScheduleWakeup(WakeupId::TorrentCheck, _))));
}

// ── Wakeup dispatch ───────────────────────────────────────────────────────────

#[test]
fn wakeup_heartbeat_checks_mam_connectability() {
    let (_, actions) =
        process_event(SystemState::initial(), Event::Wakeup(WakeupId::Heartbeat));
    assert!(actions.iter().any(|a| matches!(a, Action::CheckMamConnectability)));
}

#[test]
fn wakeup_disk_check_checks_disk_space() {
    let (_, actions) =
        process_event(SystemState::initial(), Event::Wakeup(WakeupId::DiskCheck));
    assert!(actions.iter().any(|a| matches!(a, Action::CheckDiskSpace)));
}

#[test]
fn wakeup_torrent_check_checks_new_torrents() {
    let (_, actions) =
        process_event(SystemState::initial(), Event::Wakeup(WakeupId::TorrentCheck));
    assert!(actions.iter().any(|a| matches!(a, Action::CheckNewTorrents)));
}

#[test]
fn wakeup_qbit_auth_retry_authenticates() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::Authenticating { attempt: RetryCount(0) };
    let (_, actions) = process_event(state, Event::Wakeup(WakeupId::QbitAuthRetry));
    assert!(actions.iter().any(|a| matches!(a, Action::AuthenticateQbit)));
}

#[test]
fn wakeup_qbit_sync_retry_syncs_when_in_syncing_state() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::SyncingPort { attempt: RetryCount(1), cookie: cookie(), target: port() };
    let (_, actions) = process_event(state, Event::Wakeup(WakeupId::QbitSyncRetry));
    assert!(actions.iter().any(|a| matches!(a, Action::SyncQbitPort(_, _))));
}

#[test]
fn wakeup_qbit_sync_retry_is_noop_when_not_syncing() {
    let mut state = SystemState::initial();
    state.qbit = QbitState::Offline;
    let (_, actions) = process_event(state, Event::Wakeup(WakeupId::QbitSyncRetry));
    assert!(actions.is_empty());
}

#[test]
fn wakeup_retry_port_read_reads_port_files() {
    let (_, actions) =
        process_event(SystemState::initial(), Event::Wakeup(WakeupId::RetryPortRead));
    assert!(actions.iter().any(|a| matches!(a, Action::ReadPortFiles)));
}

