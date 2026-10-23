//! Scenario tests for tunnel checks, VPN protection, and server failover, with
//! supplied time.

use std::{
    net::{IpAddr, Ipv4Addr},
    time::{Duration, Instant, SystemTime},
};

use windlass::core::{
    Action, CHECK_INTERVAL, Check, ClockReading, Core, Event, ExitIpFailure,
    FAILURES_BEFORE_SWITCH, Failover, LEASE_LIFETIME, LastSwitch, MappingFailure, Port,
    PortForwardingEvent, Protection, RETRY_AFTER, SWITCH_SPACING, StartupEvent, StartupMode,
    SwitchAllowance, TimerId, TimerName, TunnelEvent, VpnServer, VpnSwitch, Watch,
};

type TestResult = Result<(), &'static str>;

const EXIT_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7));

/// A core with a clock that only moves when told to.
struct Harness {
    core: Core,
    start: Instant,
    elapsed: Duration,
}

impl Harness {
    /// A started core whose storage reports this last switch, checking a
    /// tunnel connected to `current` among `servers`.
    fn new(last_switch: Option<VpnSwitch>, servers: &[&str], current: Option<&str>) -> Self {
        let mut harness = Self {
            core: Core::new(),
            start: Instant::now(),
            elapsed: Duration::from_hours(2),
        };
        let _ = harness.send(Event::Startup(StartupEvent::Start {
            mode: StartupMode::Resume,
        }));
        let opened = harness.send(Event::Startup(StartupEvent::StorageOpened));
        assert!(opened.contains(&Action::ReadLastVpnSwitch));
        assert_eq!(harness.core.tunnel().last_switch, LastSwitch::Reading);
        assert!(
            harness
                .tunnel(TunnelEvent::LastSwitchRead { last_switch })
                .is_empty()
        );
        assert_eq!(
            harness.tunnel(TunnelEvent::Enable {
                servers: servers.iter().map(|name| server(name)).collect(),
                current: current.map(server),
            }),
            vec![Action::CheckExitIp],
            "the tunnel is checked at startup"
        );
        harness
    }

    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + self.elapsed
    }

    fn advance(&mut self, by: Duration) {
        self.elapsed += by;
    }

    fn send(&mut self, event: Event) -> Vec<Action> {
        let now = ClockReading {
            monotonic: self.start + self.elapsed,
            wall: self.now(),
        };
        self.core.handle(now, event)
    }

    fn tunnel(&mut self, event: TunnelEvent) -> Vec<Action> {
        self.send(Event::Tunnel(event))
    }

    fn watch(&self) -> &Watch {
        self.core
            .tunnel()
            .watch
            .as_ref()
            .unwrap_or_else(|| unreachable!("tunnel checks are not enabled"))
    }

    fn fail(&mut self) -> Vec<Action> {
        self.tunnel(TunnelEvent::ExitIpCheckFailed {
            reason: ExitIpFailure::Unreachable,
        })
    }

    /// Fail checks until a switch would be considered, waiting out each retry.
    fn fail_repeatedly(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        for _ in 0..FAILURES_BEFORE_SWITCH {
            if let Some(id) = timer(&actions, RETRY_AFTER) {
                self.advance(RETRY_AFTER);
                assert_eq!(self.expire(id), vec![Action::CheckExitIp]);
            }
            actions = self.fail();
        }
        actions
    }

    fn expire(&mut self, id: TimerId) -> Vec<Action> {
        self.send(Event::TimerExpired {
            name: TimerName::TunnelCheck,
            id,
        })
    }
}

fn server(name: &str) -> VpnServer {
    VpnServer::try_new(name).unwrap_or_else(|_| unreachable!("valid server name"))
}

/// The tunnel-check timer an action list starts after this delay.
fn timer(actions: &[Action], after: Duration) -> Option<TimerId> {
    actions.iter().find_map(|action| match action {
        Action::StartTimer {
            name: TimerName::TunnelCheck,
            id,
            deadline,
        } if deadline.after == after => Some(*id),
        _ => None,
    })
}

#[test]
fn working_tunnels_are_checked_every_fifteen_minutes() -> TestResult {
    let mut harness = Harness::new(None, &["one", "two"], Some("one"));
    assert_eq!(harness.watch().protection, Protection::Unknown);
    let actions = harness.tunnel(TunnelEvent::ExitIpObserved { ip: EXIT_IP });
    let next = timer(&actions, CHECK_INTERVAL).ok_or("no check timer")?;
    assert_eq!(
        harness.watch().protection,
        Protection::Available {
            exit_ip: EXIT_IP,
            checked_at: harness.now()
        }
    );
    assert!(
        harness
            .tunnel(TunnelEvent::ExitIpObserved { ip: EXIT_IP })
            .is_empty(),
        "an answer nobody waits for changes nothing"
    );
    harness.advance(CHECK_INTERVAL);
    assert_eq!(harness.expire(next), vec![Action::CheckExitIp]);
    assert!(harness.expire(next).is_empty(), "a used timer is obsolete");
    Ok(())
}

#[test]
fn repeated_failures_switch_to_the_next_server() -> TestResult {
    let mut harness = Harness::new(None, &["one", "two", "three"], Some("two"));
    let _ = harness.send(Event::PortForwarding(PortForwardingEvent::Enable));
    let forwarded = Port::new(41000).ok_or("zero port")?;
    let _ = harness.send(Event::PortForwarding(PortForwardingEvent::Mapped {
        tcp: forwarded,
        udp: forwarded,
        lifetime: LEASE_LIFETIME,
    }));
    let first = harness.fail();
    assert!(timer(&first, RETRY_AFTER).is_some());
    let since = harness.now();
    assert_eq!(
        harness.watch().protection,
        Protection::Unavailable { since }
    );
    assert_eq!(
        harness.core.tunnel().switch_allowance(harness.now()),
        SwitchAllowance::Now
    );

    harness.advance(RETRY_AFTER);
    let _ = harness.expire(timer(&first, RETRY_AFTER).ok_or("no retry")?);
    let second = harness.fail();
    assert_eq!(
        harness.watch().protection,
        Protection::Unavailable { since },
        "protection was lost at the first failure"
    );
    harness.advance(RETRY_AFTER);
    let _ = harness.expire(timer(&second, RETRY_AFTER).ok_or("no retry")?);
    let switch = VpnSwitch {
        server: server("three"),
        at: harness.now(),
    };
    assert_eq!(
        harness.fail(),
        vec![Action::SaveVpnSwitch {
            switch: switch.clone()
        }],
        "the switch is recorded before it starts"
    );
    assert_eq!(harness.watch().check, Check::Switching);
    assert!(
        harness
            .send(Event::PortForwarding(PortForwardingEvent::MappingFailed {
                reason: MappingFailure::NoResponse,
            }))
            .iter()
            .all(|action| *action != Action::CheckExitIp),
        "no check while switching"
    );
    assert!(
        harness.tunnel(TunnelEvent::Switched).is_empty(),
        "not started yet"
    );
    assert_eq!(
        harness.tunnel(TunnelEvent::SwitchSaved),
        vec![Action::SwitchVpnServer {
            server: server("three")
        }]
    );
    assert_eq!(
        harness.core.tunnel().last_switch,
        LastSwitch::Known(Some(switch.clone()))
    );
    assert_eq!(
        harness.watch().failover,
        Some(Failover::Switching(server("three")))
    );
    assert!(harness.tunnel(TunnelEvent::SwitchSaved).is_empty());
    let switched = harness.tunnel(TunnelEvent::Switched);
    assert!(switched.contains(&Action::CheckExitIp));
    assert_eq!(harness.watch().current, Some(server("three")));
    assert_eq!(harness.watch().protection, Protection::Unknown);
    assert_eq!(harness.watch().failures, 0);
    assert_eq!(harness.core.port_forwarding().forwarded_port(), None);
    assert!(
        switched.contains(&Action::RequestPortMapping {
            lifetime: LEASE_LIFETIME
        }),
        "the new server's gateway is asked for a port at once"
    );
    assert_eq!(
        harness.core.tunnel().switch_allowance(harness.now()),
        SwitchAllowance::After(switch.at + SWITCH_SPACING)
    );
    Ok(())
}

#[test]
fn switches_wait_an_hour_after_the_last_one() -> TestResult {
    let start = SystemTime::UNIX_EPOCH + Duration::from_hours(2);
    let last = VpnSwitch {
        server: server("one"),
        at: start - Duration::from_mins(30),
    };
    let mut harness = Harness::new(Some(last), &["one", "two"], Some("one"));
    // A lease request is outstanding when the switch completes.
    let _ = harness.send(Event::PortForwarding(PortForwardingEvent::Enable));
    let blocked = harness.fail_repeatedly();
    assert!(
        timer(&blocked, RETRY_AFTER).is_some(),
        "checks continue instead of a switch"
    );
    assert_eq!(
        harness.core.tunnel().switch_allowance(harness.now()),
        SwitchAllowance::After(start + Duration::from_mins(30))
    );
    harness.advance(Duration::from_mins(30));
    assert_eq!(
        harness.core.tunnel().switch_allowance(harness.now()),
        SwitchAllowance::Now,
        "an hour has passed since the last switch"
    );
    let _ = harness.expire(timer(&blocked, RETRY_AFTER).ok_or("no retry")?);
    assert!(matches!(
        harness.fail().as_slice(),
        [Action::SaveVpnSwitch { switch }] if switch.server == server("two")
    ));
    let _ = harness.tunnel(TunnelEvent::SwitchSaved);
    assert!(matches!(
        harness.core.port_forwarding(),
        windlass::core::PortForwarding::Enabled {
            request: windlass::core::LeaseRequest::Outstanding,
            ..
        }
    ));
    assert_eq!(
        harness.tunnel(TunnelEvent::Switched),
        vec![Action::CheckExitIp],
        "the outstanding lease request already reaches the new gateway"
    );
    Ok(())
}

#[test]
fn failed_switches_are_retried_after_the_spacing() -> TestResult {
    let mut harness = Harness::new(None, &["one", "two"], None);
    assert!(matches!(
        harness.fail_repeatedly().as_slice(),
        [Action::SaveVpnSwitch { switch }] if switch.server == server("one")
    ));
    let retry = harness.tunnel(TunnelEvent::SwitchSaveFailed);
    let id = timer(&retry, RETRY_AFTER).ok_or("no retry after a failed save")?;
    assert_eq!(
        harness.core.tunnel().switch_allowance(harness.now()),
        SwitchAllowance::Now
    );
    harness.advance(RETRY_AFTER);
    let _ = harness.expire(id);
    let _ = harness.fail();
    assert!(
        harness.tunnel(TunnelEvent::SwitchSaved).len() == 1,
        "the switch is attempted again"
    );
    let retry = harness.tunnel(TunnelEvent::SwitchFailed);
    assert!(timer(&retry, RETRY_AFTER).is_some());
    assert_eq!(harness.watch().current, None, "the server did not change");
    assert!(matches!(
        harness.core.tunnel().switch_allowance(harness.now()),
        SwitchAllowance::After(_)
    ));
    assert!(
        harness.tunnel(TunnelEvent::SwitchFailed).is_empty(),
        "a result without a switch changes nothing"
    );
    harness.advance(SWITCH_SPACING);
    let _ = harness.expire(timer(&retry, RETRY_AFTER).ok_or("no retry")?);
    let _ = harness.fail();
    let _ = harness.tunnel(TunnelEvent::SwitchSaved);
    assert_eq!(
        harness.tunnel(TunnelEvent::Switched),
        vec![Action::CheckExitIp],
        "without port forwarding there is no lease to renew"
    );
    Ok(())
}

#[test]
fn switches_need_a_known_last_switch_and_another_server() {
    let mut core = Core::new();
    let now = ClockReading {
        monotonic: Instant::now(),
        wall: SystemTime::UNIX_EPOCH,
    };
    assert_eq!(
        core.tunnel().switch_allowance(now.wall),
        SwitchAllowance::NoAlternative
    );
    let _ = core.handle(
        now,
        Event::Tunnel(TunnelEvent::Enable {
            servers: vec![server("one"), server("two")],
            current: Some(server("one")),
        }),
    );
    assert!(
        core.handle(
            now,
            Event::Tunnel(TunnelEvent::Enable {
                servers: Vec::new(),
                current: None,
            })
        )
        .is_empty(),
        "enabling twice changes nothing"
    );
    assert_eq!(
        core.tunnel().switch_allowance(now.wall),
        SwitchAllowance::Unknown
    );
    let _ = core.handle(now, Event::Tunnel(TunnelEvent::LastSwitchReadFailed));
    let _ = core.handle(
        now,
        Event::Tunnel(TunnelEvent::LastSwitchRead { last_switch: None }),
    );
    assert_eq!(
        core.tunnel().last_switch,
        LastSwitch::Unread,
        "results nobody asked for are ignored"
    );
    let _ = core.handle(
        now,
        Event::Startup(StartupEvent::Start {
            mode: StartupMode::Resume,
        }),
    );
    let _ = core.handle(now, Event::Startup(StartupEvent::StorageOpened));
    let _ = core.handle(now, Event::Tunnel(TunnelEvent::LastSwitchReadFailed));
    assert_eq!(core.tunnel().last_switch, LastSwitch::ReadFailed);
    for _ in 0..FAILURES_BEFORE_SWITCH {
        let actions = core.handle(
            now,
            Event::Tunnel(TunnelEvent::ExitIpCheckFailed {
                reason: ExitIpFailure::Malformed,
            }),
        );
        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, Action::SaveVpnSwitch { .. })),
            "an unknown last switch blocks switches"
        );
        if let Some(id) = timer(&actions, RETRY_AFTER) {
            let _ = core.handle(
                now,
                Event::TimerExpired {
                    name: TimerName::TunnelCheck,
                    id,
                },
            );
        }
    }

    let mut alone = Harness::new(None, &["one"], Some("one"));
    assert_eq!(
        alone.core.tunnel().switch_allowance(alone.now()),
        SwitchAllowance::NoAlternative
    );
    assert!(timer(&alone.fail_repeatedly(), RETRY_AFTER).is_some());
}

#[test]
fn suspected_problems_are_checked_at_once_but_not_too_often() {
    let mut harness = Harness::new(None, &["one", "two"], Some("one"));
    let _ = harness.send(Event::PortForwarding(PortForwardingEvent::Enable));
    let failed = Event::PortForwarding(PortForwardingEvent::MappingFailed {
        reason: MappingFailure::NoResponse,
    });
    assert!(
        !harness.send(failed.clone()).contains(&Action::CheckExitIp),
        "a check is already running"
    );
    let _ = harness.tunnel(TunnelEvent::ExitIpObserved { ip: EXIT_IP });
    assert!(
        !harness.send(failed.clone()).contains(&Action::CheckExitIp),
        "a check started moments ago"
    );
    harness.advance(Duration::from_secs(30));
    assert!(harness.send(failed).contains(&Action::CheckExitIp));
    assert_eq!(harness.watch().check, Check::Outstanding);
}

#[test]
fn server_names_are_unit_instance_names() {
    for valid in ["one", "nl-free_42", &"a".repeat(64)] {
        assert!(VpnServer::try_new(valid).is_ok(), "{valid}");
    }
    for invalid in ["", "two words", "a/b", "x.service", &"a".repeat(65)] {
        assert!(VpnServer::try_new(invalid).is_err(), "{invalid}");
    }
}
