//! The status page's MAM panels: the session and seedbox registration, the
//! account as MAM reports it, and bookmark acquisition.

use std::time::SystemTime;

use maud::{Markup, html};

use crate::{
    core::{
        Attention, ClientRefusal, Connectability, Connectable, DownloadWatch, LastAttempt, Mam,
        MamSession, Partials, Points, PurchaseOutcome, PurchaseRecord, RegistrationAllowance,
        RegistrationFailure, SeedboxStatus, SettingsState, SettledBy, UPLOAD_BUFFER,
    },
    shell::describe::{client_standing, findings, gib, hold, mam_failure, purchase, rejection},
};

use super::pages::time;

/// The MAM panels.
pub(super) fn panels(mam: &Mam, settings: &SettingsState, now: SystemTime) -> Markup {
    html! {
        (access_panel(mam, settings, now))
        (account_panel(mam, settings, now))
        (bookmarks_panel(mam))
    }
}

fn access_panel(mam: &Mam, settings: &SettingsState, now: SystemTime) -> Markup {
    let cookie = settings
        .loaded()
        .is_some_and(|settings| settings.api_cookie.saved.is_some());
    let (tag, tone) = match (&mam.session, mam.enabled, mam.available, cookie) {
        (_, false, _, _) => ("Not configured", "neutral"),
        (_, _, _, false) => ("No API cookie", "warning"),
        (MamSession::Rejected { .. }, ..) => ("Session refused", "error"),
        (_, _, false, _) => ("Waiting", "neutral"),
        (MamSession::Verified { .. }, ..) => ("Working", "ok"),
        (MamSession::Unverified, ..) => ("Checking…", "pending"),
    };
    html! {
        section.panel aria-labelledby="mam" {
            div.panel-head {
                h2 #mam { "MAM access" }
                span class={ "tag " (tone) } { (tag) }
            }
            dl {
                dt { "Session" }
                dd {
                    @match &mam.session {
                        MamSession::Unverified => "Not verified since startup, a pause, or a change",
                        MamSession::Verified { at } => { "Accepted, last at " (time(*at, "full")) }
                        MamSession::Rejected { rejection: reason, at, .. } => {
                            "Refused at " (time(*at, "full")) ": " (rejection(*reason))
                        }
                    }
                }
                dt { "IP MAM sees" }
                dd {
                    @match &mam.network.observed {
                        Some(observed) => {
                            span.mono { (observed.observation.ip) }
                            " in " (observed.observation.asn)
                            @if let Some(name) = &observed.observation.provider { " (" (name) ")" }
                            ", "
                            (time(observed.at, "full"))
                            @if !observed.current { " · unconfirmed" }
                        }
                        None => "Not observed yet",
                    }
                }
                @if let Some(failure) = &mam.network.last_failure {
                    dt { "Failed lookups" } dd { (mam.network.failures) ": " (mam_failure(failure)) }
                }
                dt { "Seedbox registration" }
                dd {
                    @match &mam.seedbox.status {
                        SeedboxStatus::Unconfirmed => "Not confirmed",
                        SeedboxStatus::Confirmed { ip, at } => {
                            "Confirmed for " span.mono { (ip) } " at " (time(*at, "full"))
                        }
                        SeedboxStatus::Failed { failure, at } => {
                            "Failed at " (time(*at, "full")) ": "
                            @match failure {
                                RegistrationFailure::Mam(failure) => (mam_failure(failure)),
                                RegistrationFailure::NotSaved => "the attempt could not be recorded",
                            }
                        }
                    }
                }
                dt { "Next registration" }
                dd {
                    @match (mam.seedbox.last_attempt, mam.seedbox.allowance(now)) {
                        (LastAttempt::ReadFailed, _) => "Blocked: the last attempt could not be read",
                        (_, RegistrationAllowance::Unknown) => "Waiting for the last attempt to be read",
                        (_, RegistrationAllowance::Now) => "Allowed when needed",
                        (_, RegistrationAllowance::After(at)) => {
                            "Not before " (time(at, "full")) ", as MAM allows one an hour"
                        }
                    }
                }
            }
        }
    }
}

fn account_panel(mam: &Mam, settings: &SettingsState, now: SystemTime) -> Markup {
    let account = &mam.account;
    let reserve = settings.points_reserve();
    let Some(reading) = &account.reading else {
        return html! {
            section.panel aria-labelledby="mam-account" {
                div.panel-head {
                    h2 #mam-account { "MAM account" }
                    span.tag.neutral { "Not read yet" }
                }
                @if let Some(failure) = &account.last_failure {
                    p { "Reading failed " (account.failures) " times: " (mam_failure(failure)) }
                } @else {
                    p { "Windlass reads the account from MAM every 15 minutes once requests run." }
                }
            }
        };
    };
    let data = &reading.data;
    let (tag, tone) = if account.ratio_low().is_some() {
        ("Ratio low", "error")
    } else if account.attention().is_some_and(|attention| attention.any())
        || account.unsatisfied_near_limit().is_some()
    {
        ("Needs attention", "warning")
    } else {
        ("In good standing", "ok")
    };
    html! {
        section.panel aria-labelledby="mam-account" {
            div.panel-head {
                h2 #mam-account { "MAM account" }
                span class={ "tag " (tone) } { (tag) }
            }
            dl {
                dt { "Read" } dd { (time(reading.at, "full")) }
                dt { "Class" }
                dd {
                    (data.class)
                    @if let Some(until) = data.vip_until { " · VIP until " (time(until, "full")) }
                }
                dt { "Bonus points" }
                dd {
                    (data.points)
                    @if let Some(reserve) = reserve { " · reserve " (reserve) }
                }
                dt { "Wedges" } dd { (data.wedges) }
                dt { "Upload beyond downloads" }
                dd { (gib(data.buffer())) " · kept above " (gib(UPLOAD_BUFFER)) }
                dt { "Ratio" }
                dd {
                    @match account.ratio() {
                        Some(ratio) => (format!("{}.{:02}", ratio / 1000, ratio % 1000 / 10)),
                        None => "Unlimited: nothing downloaded",
                    }
                }
                @if let Some(summary) = &data.summary {
                    dt { "Unsatisfied" }
                    dd {
                        (summary.unsatisfied) " of " (summary.unsatisfied_limit)
                        @let not_seeding = summary.unsatisfied_not_seeding.plus(summary.hit_and_run_not_seeding);
                        @if !not_seeding.is_zero() {
                            " · " (not_seeding) " not seeding"
                        }
                    }
                    dt { "Connectable" }
                    dd {
                        @match &summary.connectable {
                            Connectable::Yes => "Yes",
                            Connectable::No => "No",
                            Connectable::Offline => "Offline: nothing is seeding",
                            Connectable::Unrecognised(value) => { "MAM says " (value) }
                        }
                    }
                }
                (client_list(mam))
                (repair(mam))
                @if let Some(attention) = account.attention().filter(Attention::any) {
                    dt { "Flagged by MAM" }
                    dd {
                        @if attention.partial == Partials::Seen { "A partial download. " }
                        @if !attention.tracker_errors.is_zero() { (attention.tracker_errors) " tracker errors. " }
                        @if !attention.flagged.is_zero() { (attention.flagged) " torrents in red categories." }
                    }
                }
                @if let Some(notices) = data.notices.filter(|notices| !notices.total().is_zero()) {
                    dt { "MAM notifications" } dd { (notices.total()) ", not read by Windlass" }
                }
                @if let Some(cost) = account.vip_cost(data, now) {
                    dt { "VIP renewal" } dd { "Due; about " (Points(cost)) " points" }
                }
                @if let Some(record) = &account.last_purchase { (last_purchase(record)) }
            }
        }
    }
}

/// What Windlass did when MAM could not reach qBittorrent.
fn repair(mam: &Mam) -> Markup {
    html! {
        @match &mam.connectability {
            Connectability::Fine => {}
            Connectability::Repaired { wall, findings: found, .. } => {
                dt { "Connectability repair" }
                dd {
                    "Made at " (time(*wall, "full"))
                    "; a reading 30 minutes later shows whether it worked. " (findings(found)) "."
                }
            }
            Connectability::Unresolved { repaired, findings: found } => {
                dt { "Connectability repair" }
                dd.attention {
                    "Still not connectable after the repair at " (time(*repaired, "full")) ". "
                    (findings(found)) "."
                }
            }
        }
    }
}

/// MAM's client list and what it says about the running qBittorrent.
fn client_list(mam: &Mam) -> Markup {
    html! {
        @if let Some(list) = &mam.account.client_list {
            dt { "Clients MAM lists" }
            dd {
                (client_standing(mam.client)) " · read " (time(list.at, "full"))
                @for client in &list.clients {
                    br;
                    @match client.qbittorrent {
                        Some(version) => { "qBittorrent " (version) }
                        None => "An entry Windlass cannot read",
                    }
                    @match &client.connectable {
                        Some(Connectable::Yes) => " · connectable",
                        Some(Connectable::No) => " · not connectable",
                        _ => "",
                    }
                    @if client.refusal == ClientRefusal::Refused { " · marked not allowed" }
                }
            }
        }
    }
}

fn last_purchase(record: &PurchaseRecord) -> Markup {
    html! {
        dt { "Last purchase" }
        dd {
            (purchase(record.purchase)) " at " (time(record.at, "full")) ": "
            (outcome(record.outcome.as_ref()))
        }
    }
}

fn outcome(outcome: Option<&PurchaseOutcome>) -> Markup {
    html! {
        @match outcome {
            None => "waiting for MAM",
            Some(PurchaseOutcome::Made(points)) => { "made; " (points) " points left" }
            Some(PurchaseOutcome::Refused(Some(message))) => { "refused: " (message) }
            Some(PurchaseOutcome::Refused(None)) => "refused",
            Some(PurchaseOutcome::Uncertain) => "uncertain; nothing more is bought until the next reading",
            Some(PurchaseOutcome::Settled { made, by }) => {
                @if *made { "made" } @else { "not made" }
                @match by {
                    SettledBy::Reading => ", as the next reading showed",
                    SettledBy::Owner => ", as you checked on MAM",
                }
            }
            Some(PurchaseOutcome::Unresolved) => {
                "the next reading could not tell whether it went through. Check your bonus points on MAM, then say:"
                form.inline method="post" action="/mam/purchase/checked" {
                    button.secondary type="submit" name="made" value="yes" { "It went through" }
                    " "
                    button.secondary type="submit" name="made" value="no" { "It did not" }
                }
            }
        }
    }
}

fn bookmarks_panel(mam: &Mam) -> Markup {
    let acquisition = &mam.acquisition;
    let (tag, tone) = match (acquisition.hold, acquisition.refused) {
        (_, Some(_)) => ("Downloads refused", "error"),
        (Some(_), None) => ("Waiting", "warning"),
        (None, None) => ("Ready", "ok"),
    };
    html! {
        section.panel aria-labelledby="bookmarks" {
            div.panel-head {
                h2 #bookmarks { "Bookmarks" }
                span class={ "tag " (tone) } { (tag) }
            }
            dl {
                dt { "Listed" }
                dd {
                    @match &acquisition.bookmarks.latest {
                        Some(listed) => { (listed.found) " bookmarks at " (time(listed.at, "full")) }
                        None => "Not yet",
                    }
                }
                @if let Some(failure) = &acquisition.bookmarks.last_failure {
                    dt { "Failed listings" } dd { (acquisition.bookmarks.failures) ": " (mam_failure(failure)) }
                }
                @if let Some(reason) = acquisition.hold {
                    dt { "Next download waits" } dd { (hold(reason)) }
                }
                @if let Some((id, wedge)) = acquisition.fetching {
                    dt { "Downloading" }
                    dd { "MAM torrent " (id.get()) @if wedge { ", spending a wedge" } }
                }
                @if !acquisition.watched.is_empty() {
                    dt { "Running downloads" }
                    dd {
                        @for (index, (download, watch)) in acquisition.watched.iter().enumerate() {
                            @if index > 0 { br; }
                            "MAM torrent " (download.mam.get()) ": " (download_watch(*watch))
                        }
                    }
                }
            }
        }
    }
}

/// What Windlass knows about a running download it added.
const fn download_watch(watch: DownloadWatch) -> &'static str {
    match watch {
        DownloadWatch::Wedged => "a wedge makes it free for you",
        DownloadWatch::FreeForNow => {
            "free for you now; checked again with the next bookmark listing"
        }
        DownloadWatch::NeedLookup | DownloadWatch::NeedHistory => "being checked with MAM",
        DownloadWatch::NotFree {
            wedge_missing: true,
            ..
        } => "stopped: MAM's bonus history shows no wedge spent on it",
        DownloadWatch::NotFree { listed: false, .. } => "stopped: MAM no longer lists it",
        DownloadWatch::NotFree { .. } => "not free for you; a wedge is spent if one is left",
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::IpAddr,
        time::{Duration, Instant, SystemTime},
    };

    use crate::core::{
        ApiCookie, Asn, ByteSize, Connectable, Hold, IpObservation, LastAttempt, Listed, Mam,
        MamFailure, MamNotices, MamSession, MamTorrentId, Observed, Points, PointsReserve, Reading,
        RedCategory, RegistrationFailure, SeedboxStatus, SessionRejection, Setting,
        SettingPersistence, Settings, SettingsState, SnatchSummary, UserData,
    };
    use crate::core::{
        BookmarkCount, CategoryName, ClientRefusal, MamMessage, NetworkName, NoticeCount, Partials,
        RedKind, TorrentCount, UnsatisfiedCount, UnsatisfiedLimit, UserClass, WedgeCount,
    };

    use super::{outcome, panels};

    #[test]
    fn every_purchase_outcome_has_words() {
        use crate::core::PurchaseOutcome;

        for (state, text) in [
            (None, "waiting for MAM"),
            (
                Some(PurchaseOutcome::Made(Points(5))),
                "made; 5 points left",
            ),
            (
                Some(PurchaseOutcome::Refused(MamMessage::try_new("No").ok())),
                "refused: No",
            ),
            (Some(PurchaseOutcome::Refused(None)), "refused"),
            (Some(PurchaseOutcome::Uncertain), "uncertain"),
            (
                Some(PurchaseOutcome::Settled {
                    made: true,
                    by: crate::core::SettledBy::Reading,
                }),
                "made, as the next reading showed",
            ),
            (
                Some(PurchaseOutcome::Settled {
                    made: false,
                    by: crate::core::SettledBy::Owner,
                }),
                "not made, as you checked on MAM",
            ),
            (
                Some(PurchaseOutcome::Unresolved),
                "action=\"/mam/purchase/checked\"",
            ),
        ] {
            assert!(
                outcome(state.as_ref()).into_string().contains(text),
                "{text}"
            );
        }
    }

    fn settings(cookie: bool) -> SettingsState {
        let cookie = cookie.then(|| ApiCookie::parse("value").ok()).flatten();
        SettingsState::Loaded(Settings {
            history_limit: Setting {
                saved: crate::core::HistoryLimit::default(),
                requested: crate::core::HistoryLimit::default(),
                persistence: SettingPersistence::Idle,
            },
            api_cookie: Setting {
                saved: cookie.clone(),
                requested: cookie,
                persistence: SettingPersistence::Idle,
            },
            ntfy_topic: Setting {
                saved: None,
                requested: None,
                persistence: SettingPersistence::Idle,
            },
            points_reserve: Setting {
                saved: PointsReserve::default(),
                requested: PointsReserve::default(),
                persistence: SettingPersistence::Idle,
            },
            torrent_limits: Setting {
                saved: crate::core::TorrentLimits::default(),
                requested: crate::core::TorrentLimits::default(),
                persistence: SettingPersistence::Idle,
            },
        })
    }

    fn render(mam: &Mam, cookie: bool) -> String {
        panels(mam, &settings(cookie), SystemTime::UNIX_EPOCH).into_string()
    }

    /// A summary with nothing flagged, for a VIP account's limit of 150.
    fn summary() -> SnatchSummary {
        SnatchSummary {
            unsatisfied: UnsatisfiedCount::default(),
            unsatisfied_limit: UnsatisfiedLimit::new(150),
            unsatisfied_not_seeding: TorrentCount::default(),
            hit_and_run_not_seeding: TorrentCount::default(),
            partial: Partials::NotSeen,
            duplicates: TorrentCount::default(),
            tracker_errors: TorrentCount::default(),
            connectable: Connectable::Offline,
            red: Vec::new(),
        }
    }

    fn data() -> UserData {
        UserData {
            uploaded: ByteSize(1 << 30),
            downloaded: ByteSize(1 << 30),
            points: Points(100),
            wedges: WedgeCount::new(3),
            class: UserClass::PowerUser,
            vip_until: Some(SystemTime::UNIX_EPOCH + Duration::from_hours(24)),
            summary: Some(SnatchSummary {
                unsatisfied: UnsatisfiedCount::new(140),
                unsatisfied_not_seeding: TorrentCount::new(1),
                hit_and_run_not_seeding: TorrentCount::new(1),
                partial: Partials::Seen,
                duplicates: TorrentCount::new(1),
                tracker_errors: TorrentCount::new(2),
                connectable: Connectable::No,
                red: CategoryName::try_new("Duplicate peer entries")
                    .map(|name| RedCategory {
                        name,
                        kind: RedKind::Other,
                        count: TorrentCount::new(1),
                    })
                    .into_iter()
                    .collect(),
                ..summary()
            }),
            notices: Some(MamNotices {
                messages: NoticeCount::new(2),
                ..MamNotices::default()
            }),
            clients: Some(Vec::new()),
        }
    }

    #[test]
    fn running_downloads_are_shown_with_what_windlass_knows() {
        use crate::core::{Download, DownloadWatch, TorrentHash};

        let mut mam = Mam::default();
        let hash = TorrentHash::parse(&"a".repeat(40));
        let watches = [
            DownloadWatch::Wedged,
            DownloadWatch::FreeForNow,
            DownloadWatch::NeedLookup,
            DownloadWatch::NotFree {
                wedge_missing: true,
                listed: true,
            },
            DownloadWatch::NotFree {
                wedge_missing: false,
                listed: false,
            },
            DownloadWatch::NotFree {
                wedge_missing: false,
                listed: true,
            },
        ];
        mam.acquisition.watched = MamTorrentId::new(5)
            .zip(hash)
            .into_iter()
            .flat_map(|(id, hash)| {
                watches.map(|watch| {
                    let download = Download {
                        mam: id,
                        hash: hash.clone(),
                        held: false,
                        finished: false,
                        added_at: None,
                    };
                    (download, watch)
                })
            })
            .collect();
        let html = render(&mam, true);
        for text in [
            "Running downloads",
            "a wedge makes it free for you",
            "checked again with the next bookmark listing",
            "being checked with MAM",
            "shows no wedge spent",
            "no longer lists it",
            "a wedge is spent if one is left",
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }
    }

    #[test]
    fn connectability_repairs_are_shown() {
        use crate::core::{Connectability, Findings, IpFinding, PortFinding};

        let findings = Findings {
            listen_port: PortFinding::NoForwardedPort,
            lease_renewed: false,
            exit_ip: IpFinding::Unknown,
            registered: false,
            interface: None,
        };
        let mut mam = Mam::default();
        mam.account.reading = Some(Reading {
            data: data(),
            at: SystemTime::UNIX_EPOCH,
            taken: Instant::now(),
        });
        mam.connectability = Connectability::Repaired {
            at: Instant::now(),
            wall: SystemTime::UNIX_EPOCH,
            findings: findings.clone(),
        };
        let html = render(&mam, true);
        assert!(html.contains("shows whether it worked"), "{html}");
        assert!(html.contains("the VPN forwards no port"), "{html}");
        mam.connectability = Connectability::Unresolved {
            repaired: SystemTime::UNIX_EPOCH,
            findings,
        };
        let html = render(&mam, true);
        assert!(html.contains("Still not connectable"), "{html}");
    }

    #[test]
    fn the_client_list_is_shown_with_what_it_means() {
        use crate::core::{ClientList, ClientStanding, ClientVersion, ListedClient};

        let mut mam = Mam::default();
        mam.account.reading = Some(Reading {
            data: data(),
            at: SystemTime::UNIX_EPOCH,
            taken: Instant::now(),
        });
        mam.account.client_list = Some(ClientList {
            clients: vec![
                ListedClient {
                    qbittorrent: ClientVersion::parse("5.1.0"),
                    connectable: Some(Connectable::Yes),
                    refusal: ClientRefusal::Refused,
                },
                ListedClient {
                    qbittorrent: None,
                    connectable: Some(Connectable::No),
                    refusal: ClientRefusal::NotRefused,
                },
                ListedClient {
                    qbittorrent: None,
                    connectable: None,
                    refusal: ClientRefusal::NotRefused,
                },
            ],
            at: SystemTime::UNIX_EPOCH,
        });
        mam.client = ClientStanding::NotListed;
        let html = render(&mam, true);
        for text in [
            "Clients MAM lists",
            "not the running version",
            "qBittorrent 5.1.0 · connectable · marked not allowed",
            "An entry Windlass cannot read · not connectable",
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one walk through every state the MAM panels describe"
    )]
    fn every_mam_state_has_words() {
        let mut mam = Mam::default();
        assert!(render(&mam, true).contains("Not configured"));
        mam.enabled = true;
        assert!(render(&mam, false).contains("No API cookie"));
        assert!(render(&mam, true).contains("Waiting"));
        mam.available = true;
        assert!(render(&mam, true).contains("Checking…"));
        mam.session = MamSession::Verified {
            at: SystemTime::UNIX_EPOCH,
        };
        assert!(render(&mam, true).contains("Working"));
        mam.session = MamSession::Rejected {
            rejection: SessionRejection::AsnMismatch,
            seen: None,
            at: SystemTime::UNIX_EPOCH,
        };
        let observation = IpObservation {
            ip: IpAddr::from([192, 0, 2, 10]),
            asn: Asn(209_103),
            provider: NetworkName::try_new("ProtonVPN Proton AG").ok(),
        };
        mam.network.observed = Some(Observed {
            observation,
            at: SystemTime::UNIX_EPOCH,
            current: false,
        });
        mam.network.failures = 2;
        mam.network.last_failure = Some(MamFailure::Unreachable);
        mam.seedbox.last_attempt = LastAttempt::ReadFailed;
        mam.seedbox.status = SeedboxStatus::Failed {
            failure: RegistrationFailure::NotSaved,
            at: SystemTime::UNIX_EPOCH,
        };
        mam.account.failures = 1;
        mam.account.last_failure = Some(MamFailure::Malformed);
        mam.acquisition.refused = Some(SystemTime::UNIX_EPOCH);
        mam.acquisition.hold = Some(Hold::DownloadsRefused);
        mam.acquisition.fetching = MamTorrentId::new(4).map(|id| (id, true));
        mam.acquisition.bookmarks.failures = 1;
        mam.acquisition.bookmarks.last_failure = Some(MamFailure::Unreachable);
        let html = render(&mam, true);
        for text in [
            "Session refused",
            "another network",
            "unconfirmed",
            "Failed lookups",
            "could not be recorded",
            "Blocked: the last attempt could not be read",
            "Reading failed 1 times",
            "Downloads refused",
            "Failed listings",
            "spending a wedge",
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }

        mam.seedbox.last_attempt = LastAttempt::Known(Some(SystemTime::UNIX_EPOCH));
        mam.seedbox.status = SeedboxStatus::Failed {
            failure: RegistrationFailure::Mam(MamFailure::Unreachable),
            at: SystemTime::UNIX_EPOCH,
        };
        mam.acquisition.refused = None;
        mam.acquisition.hold = Some(Hold::NoWedge);
        mam.acquisition.bookmarks.latest = Some(Listed {
            at: SystemTime::UNIX_EPOCH,
            taken: std::time::Instant::now(),
            found: BookmarkCount::new(7),
            bookmarks: Vec::new(),
        });
        mam.account.reading = Some(Reading {
            data: data(),
            at: SystemTime::UNIX_EPOCH,
            taken: Instant::now(),
        });
        let html = render(&mam, true);
        for text in [
            "MAM did not answer",
            "Not before",
            "Ratio low",
            "1.00",
            "140 of 150",
            "2 not seeding",
            "Connectable</dt><dd>No",
            "A partial download.",
            "2 tracker errors.",
            "1 torrents in red categories.",
            "2, not read by Windlass",
            "VIP renewal",
            "7 bookmarks",
            "none is left",
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }

        let mut quiet = data();
        quiet.uploaded = ByteSize(4 << 30);
        quiet.downloaded = ByteSize(0);
        quiet.vip_until = None;
        quiet.notices = None;
        quiet.summary = Some(SnatchSummary {
            connectable: Connectable::Yes,
            ..summary()
        });
        mam.account.reading = Some(Reading {
            data: quiet.clone(),
            at: SystemTime::UNIX_EPOCH,
            taken: Instant::now(),
        });
        mam.seedbox.status = SeedboxStatus::Confirmed {
            ip: IpAddr::from([192, 0, 2, 10]),
            at: SystemTime::UNIX_EPOCH,
        };
        mam.seedbox.last_attempt = LastAttempt::Known(None);
        mam.acquisition.hold = None;
        let html = render(&mam, true);
        for text in [
            "In good standing",
            "Unlimited: nothing downloaded",
            "Connectable</dt><dd>Yes",
            "Confirmed for",
            "Allowed when needed",
            "Ready",
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }
        for (connectable, text) in [
            (Connectable::Offline, "Offline: nothing is seeding"),
            (
                Connectable::Unrecognised(String::from("perhaps")),
                "MAM says perhaps",
            ),
        ] {
            quiet.summary = Some(SnatchSummary {
                connectable,
                ..summary()
            });
            mam.account.reading = Some(Reading {
                data: quiet.clone(),
                at: SystemTime::UNIX_EPOCH,
                taken: Instant::now(),
            });
            assert!(render(&mam, true).contains(text), "{text}");
        }
        let mut flagged = quiet;
        flagged.summary = Some(SnatchSummary {
            tracker_errors: TorrentCount::new(1),
            ..summary()
        });
        mam.account.reading = Some(Reading {
            data: flagged,
            at: SystemTime::UNIX_EPOCH,
            taken: Instant::now(),
        });
        assert!(render(&mam, true).contains("Needs attention"));
        mam.seedbox.last_attempt = LastAttempt::Reading;
        assert!(render(&mam, true).contains("Waiting for the last attempt to be read"));
        mam.account.reading = None;
        mam.account.last_failure = None;
        assert!(render(&mam, true).contains("every 15 minutes"));
    }
}
