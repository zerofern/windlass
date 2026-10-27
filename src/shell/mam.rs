//! MAM's API over HTTPS: send the request the core chose, and translate the
//! answer into core terms. Every request names Windlass and its version.

use std::time::Duration;

use reqwest::{StatusCode, Url, header};
use serde_json::json;

use crate::core::{
    ApiCookie, ClientListing, FetchedTorrent, MamFailure, MamReply, MamRequest, Purchase,
    SessionRejection, Wedge,
};

use super::{mam_reply as reply, torrent_file};

/// MAM's User-Agent asks for the tool and its version, so MAM can block a
/// faulty release.
pub(super) const USER_AGENT: &str = concat!("Windlass/", env!("CARGO_PKG_VERSION"));
/// Bookmarks are listed this many to a page.
pub(super) const BOOKMARKS_PER_PAGE: u32 = 100;
const TIMEOUT: Duration = Duration::from_secs(30);

/// MAM's two hosts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MamHosts {
    /// The site, `https://www.myanonamouse.net/`.
    pub site: Url,
    /// The tracker host for IP lookups and seedbox registration,
    /// `https://t.myanonamouse.net/`.
    pub tracker: Url,
}

/// A configured MAM client, with every endpoint it calls.
#[derive(Debug, Clone)]
pub(super) struct Client {
    http: reqwest::Client,
    ip: Url,
    seedbox: Url,
    user_data: Url,
    search: Url,
    download: Url,
    store: Url,
    bonus_history: Url,
}

impl Client {
    /// A client for these hosts; `None` if they cannot carry MAM's paths.
    pub(super) fn new(hosts: &MamHosts) -> Option<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(TIMEOUT)
            .build()
            .unwrap_or_default();
        let site = |path: &str| hosts.site.join(path).ok();
        let tracker = |path: &str| hosts.tracker.join(path).ok();
        Some(Self {
            http,
            ip: tracker("json/jsonIp.php")?,
            seedbox: tracker("json/dynamicSeedbox.php")?,
            user_data: site("jsonLoad.php")?,
            search: site("tor/js/loadSearchJSONbasic.php")?,
            download: site("tor/download.php")?,
            store: site("json/bonusBuy.php")?,
            bonus_history: site("json/userBonusHistory.php")?,
        })
    }
}

/// The result of a request, the torrent file it fetched, and a replacement
/// cookie if MAM issued one.
#[derive(Debug)]
pub(super) struct Answer {
    pub result: Result<MamReply, MamFailure>,
    pub file: Option<Vec<u8>>,
    pub rotated: Option<ApiCookie>,
}

impl Answer {
    const fn failed(failure: MamFailure) -> Self {
        Self {
            result: Err(failure),
            file: None,
            rotated: None,
        }
    }
}

/// Send a request; without a client or a needed cookie it fails.
pub(super) async fn perform(
    client: Option<&Client>,
    request: &MamRequest,
    cookie: Option<&ApiCookie>,
) -> Answer {
    let Some(client) = client else {
        return Answer::failed(MamFailure::Unreachable);
    };
    let builder = match (request.needs_cookie(), cookie) {
        (false, _) => build(client, request),
        (true, Some(cookie)) => build(client, request)
            .header(header::COOKIE, format!("mam_id={}", cookie.expose_secret())),
        (true, None) => {
            return Answer::failed(MamFailure::Session(SessionRejection::InvalidCookie));
        }
    };
    let Ok(response) = builder.send().await else {
        return Answer::failed(MamFailure::Unreachable);
    };
    let status = response.status();
    let rotated = rotated_cookie(response.headers(), cookie);
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
        .map(Duration::from_secs);
    // A body that cannot be read is no answer, so it is not understood.
    let body = response.bytes().await.unwrap_or_default();
    let (result, file) = interpret(request, status, retry_after, &body);
    Answer {
        result,
        file,
        rotated,
    }
}

fn build(client: &Client, request: &MamRequest) -> reqwest::RequestBuilder {
    match request {
        MamRequest::LookUpIp => client.http.get(client.ip.clone()),
        MamRequest::RegisterSeedbox => client.http.get(client.seedbox.clone()),
        MamRequest::LoadUserData { clients } => {
            let mut url = client.user_data.clone();
            url.query_pairs_mut()
                .append_pair("snatch_summary", "true")
                .append_pair("notif", "true");
            if *clients == ClientListing::Include {
                url.query_pairs_mut().append_pair("clientStats", "true");
            }
            client.http.get(url)
        }
        MamRequest::ListBookmarks { start } => {
            client.http.post(client.search.clone()).json(&json!({
                "perpage": BOOKMARKS_PER_PAGE,
                "tor": {
                    "searchIn": "bookmarks",
                    "sortType": "",
                    "startNumber": start.to_string(),
                },
            }))
        }
        MamRequest::FetchTorrent { id, wedge } => {
            let mut url = client.download.clone();
            url.query_pairs_mut()
                .append_pair("tid", &id.get().to_string());
            if *wedge == Wedge::Spend {
                url.query_pairs_mut().append_pair("fl", "1");
            }
            client.http.get(url)
        }
        MamRequest::Buy(purchase) => {
            let mut url = client.store.clone();
            match purchase {
                Purchase::Upload(amount) => url
                    .query_pairs_mut()
                    .append_pair("spendtype", "upload")
                    .append_pair("amount", &amount.to_string()),
                Purchase::Vip => url
                    .query_pairs_mut()
                    .append_pair("spendtype", "VIP")
                    .append_pair("duration", "max"),
            };
            client.http.get(url)
        }
        MamRequest::LookUpTorrent { id } => {
            let mut url = client.search.clone();
            url.query_pairs_mut()
                .append_pair("tor[id]", &id.get().to_string());
            client.http.get(url)
        }
        MamRequest::WedgeHistory => {
            let mut url = client.bonus_history.clone();
            url.query_pairs_mut().append_pair("type[]", "wedgePF");
            client.http.get(url)
        }
    }
}

/// A new `mam_id` value from the response, if it differs from the one sent.
fn rotated_cookie(headers: &header::HeaderMap, sent: Option<&ApiCookie>) -> Option<ApiCookie> {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.strip_prefix("mam_id="))
        .filter_map(|value| value.split(';').next())
        .filter_map(|value| ApiCookie::parse(value).ok())
        .find(|cookie| Some(cookie) != sent)
}

fn interpret(
    request: &MamRequest,
    status: StatusCode,
    retry_after: Option<Duration>,
    body: &[u8],
) -> (Result<MamReply, MamFailure>, Option<Vec<u8>>) {
    let result = match request {
        _ if reply::signed_out(body) => Err(MamFailure::Session(reply::rejection(
            &String::from_utf8_lossy(body),
        ))),
        _ if status == StatusCode::TOO_MANY_REQUESTS => Err(MamFailure::TooSoon { retry_after }),
        MamRequest::RegisterSeedbox => reply::registration(status, body),
        _ if !status.is_success() => Err(MamFailure::Rejected {
            status: crate::core::HttpStatus::new(status),
        }),
        MamRequest::FetchTorrent { .. } => {
            return torrent_file::read(body).map_or(
                (Err(MamFailure::Refused { message: None }), None),
                |file| {
                    let fetched = FetchedTorrent {
                        hash: file.hash,
                        size: file.size,
                    };
                    (Ok(MamReply::Fetched(fetched)), Some(body.to_vec()))
                },
            );
        }
        MamRequest::LookUpIp => reply::ip_observation(body)
            .map(MamReply::Ip)
            .map_err(MamFailure::from),
        MamRequest::LoadUserData { .. } => reply::user_data(body)
            .map(|data| MamReply::UserData(Box::new(data)))
            .map_err(MamFailure::from),
        MamRequest::ListBookmarks { .. } => reply::bookmark_page(body)
            .map(MamReply::Bookmarks)
            .map_err(MamFailure::from),
        MamRequest::Buy(_) => reply::purchase(body),
        MamRequest::LookUpTorrent { id } => reply::bookmark_page(body)
            .map(|page| {
                MamReply::Torrent(
                    page.bookmarks
                        .into_iter()
                        .find(|listing| listing.id == *id)
                        .map(Box::new),
                )
            })
            .map_err(MamFailure::from),
        MamRequest::WedgeHistory => reply::wedge_history(body)
            .map(MamReply::WedgeHistory)
            .map_err(MamFailure::from),
    };
    (result, None)
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        net::IpAddr,
        time::{Duration, SystemTime},
    };

    use crate::core::{
        ApiCookie, Asn, BookTitle, Bookmark, BookmarkCount, BookmarkPage, ByteSize, CategoryName,
        Connectable, FetchedTorrent, Freeleech, IpObservation, MamFailure, MamMessage, MamNotices,
        MamReply, MamRequest, MamTorrentId, NetworkName, Partials, PersonalFreeleech, Points,
        Purchase, RedCategory, RedKind, Registration, SessionRejection, SnatchSummary, Snatched,
        TorrentCount, TorrentHash, UnsatisfiedCount, UnsatisfiedLimit, UploadCredit, UserClass,
        UserData, VipTorrent, WedgeCount,
    };

    use super::super::fake_mam::{FakeMam, Fault, Host, Torrent};
    use super::{Answer, Client, MamHosts, USER_AGENT, perform};

    type TestResult = Result<(), Box<dyn Error>>;

    const BOOK: u64 = 4242;
    const VIP_UNTIL: Duration = Duration::from_secs(1_796_811_253);
    const BOOKMARKED: Duration = Duration::from_secs(1_790_000_000);

    /// Compile-time torrent IDs: their unreachable branches are not run code.
    macro_rules! id {
        ($name:ident = $id:expr) => {
            const $name: MamTorrentId = match MamTorrentId::new($id) {
                Some(id) => id,
                None => unreachable!(),
            };
        };
    }
    id!(BOOK_ID = BOOK);
    id!(VIP_ID = BOOK + 2);
    id!(WEDGED_ID = BOOK + 3);
    id!(UNKNOWN_ID = 1);

    async fn fake() -> Result<(FakeMam, Client, ApiCookie), Box<dyn Error>> {
        let fake = FakeMam::start().await?;
        fake.with(|world| {
            world.account.vip_until = Some(SystemTime::UNIX_EPOCH + VIP_UNTIL);
            world.account.duplicates = 1;
            world.account.not_seeding_unsatisfied = 2;
            world.account.clients = vec![super::super::fake_mam::client("5.2.2", "yes")];
            world
                .torrents
                .insert(BOOK, Torrent::bookmarked("Synthetic Book", 1_048_576));
            let mut unbookmarked = Torrent::bookmarked("Another", 2048);
            unbookmarked.bookmarked = None;
            world.torrents.insert(BOOK + 1, unbookmarked);
            let mut vip = Torrent::bookmarked("A VIP Book", 4096);
            vip.vip = true;
            vip.free = true;
            vip.vip_expire = Some(SystemTime::UNIX_EPOCH + VIP_UNTIL);
            world.torrents.insert(BOOK + 2, vip);
            let mut wedged = Torrent::bookmarked("A Wedged Book", 8192);
            wedged.wedged = true;
            wedged.wedged_visible = true;
            wedged.snatched = true;
            world.torrents.insert(BOOK + 3, wedged);
        });
        let client = Client::new(&MamHosts {
            site: fake.site.clone(),
            tracker: fake.tracker.clone(),
        })
        .ok_or("client")?;
        Ok((fake, client, ApiCookie::parse("fake-session")?))
    }

    async fn ask(client: &Client, request: MamRequest, cookie: &ApiCookie) -> Answer {
        perform(Some(client), &request, Some(cookie)).await
    }

    fn bookmark(id: MamTorrentId, title: &str) -> Result<Bookmark, Box<dyn Error>> {
        Ok(Bookmark {
            id,
            title: BookTitle::try_new(title)?,
            vip: VipTorrent::No,
            freeleech: Freeleech::No,
            personal: PersonalFreeleech::No,
            snatched: Snatched::Never,
            bookmarked_at: Some(SystemTime::UNIX_EPOCH + BOOKMARKED),
        })
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "one scenario walks every endpoint in the order Windlass uses them"
    )]
    async fn every_request_reaches_mam_and_is_understood() -> TestResult {
        let (fake, client, cookie) = fake().await?;
        let ip = IpAddr::from([192, 0, 2, 10]);

        let answer = ask(&client, MamRequest::LookUpIp, &cookie).await;
        assert_eq!(
            answer.result,
            Ok(MamReply::Ip(IpObservation {
                ip,
                asn: Asn::new(209_103),
                provider: NetworkName::try_new("ProtonVPN Proton AG").ok(),
            }))
        );
        assert_eq!(answer.rotated, None, "the IP lookup sends no cookie");

        for registration in [Registration::Completed, Registration::NoChange] {
            let answer = ask(&client, MamRequest::RegisterSeedbox, &cookie).await;
            assert_eq!(answer.result, Ok(MamReply::Registered { registration, ip }));
            fake.with(|world| world.clock = SystemTime::now().checked_add(Duration::from_hours(2)));
        }
        fake.with(|world| world.clock = None);

        let red = |name: &str, kind, count| -> Result<RedCategory, Box<dyn Error>> {
            Ok(RedCategory {
                name: CategoryName::try_new(name)?,
                kind,
                count: TorrentCount::new(count),
            })
        };
        let red = vec![
            red("Duplicate peer entries", RedKind::Other, 1)?,
            red("Not Seeding - Not Yet Satisfied", RedKind::NotSeeding, 2)?,
        ];
        let user = |clients| UserData {
            uploaded: ByteSize::new(80 << 30),
            downloaded: ByteSize::new(0),
            points: Points::new(60_000),
            wedges: WedgeCount::new(200),
            class: UserClass::Vip,
            vip_until: Some(SystemTime::UNIX_EPOCH + VIP_UNTIL),
            summary: Some(SnatchSummary {
                unsatisfied: UnsatisfiedCount::new(0),
                unsatisfied_limit: UnsatisfiedLimit::new(150),
                unsatisfied_not_seeding: TorrentCount::new(2),
                hit_and_run_not_seeding: TorrentCount::new(0),
                partial: Partials::NotSeen,
                duplicates: TorrentCount::new(1),
                tracker_errors: TorrentCount::new(0),
                connectable: Connectable::Offline,
                red: red.clone(),
            }),
            notices: Some(MamNotices::default()),
            clients,
        };
        let answer = ask(
            &client,
            MamRequest::LoadUserData {
                clients: crate::core::ClientListing::Include,
            },
            &cookie,
        )
        .await;
        assert_eq!(
            answer.result,
            Ok(MamReply::UserData(Box::new(user(Some(vec![
                crate::core::ListedClient {
                    qbittorrent: crate::core::ClientVersion::parse("5.2.2"),
                    connectable: Some(Connectable::Yes),
                    refusal: crate::core::ClientRefusal::NotRefused,
                }
            ])))))
        );
        let rotated = answer.rotated.ok_or("MAM issued no new cookie")?;
        assert_ne!(rotated, cookie);
        let answer = ask(
            &client,
            MamRequest::LoadUserData {
                clients: crate::core::ClientListing::Omit,
            },
            &rotated,
        )
        .await;
        assert_eq!(answer.result, Ok(MamReply::UserData(Box::new(user(None)))));

        let answer = ask(
            &client,
            MamRequest::ListBookmarks {
                start: crate::core::BookmarkCount::new(0),
            },
            &cookie,
        )
        .await;
        let mut vip = bookmark(VIP_ID, "A VIP Book")?;
        vip.vip = VipTorrent::Until(SystemTime::UNIX_EPOCH + VIP_UNTIL);
        vip.freeleech = Freeleech::ForEveryone;
        vip.personal = PersonalFreeleech::ThroughVip;
        let mut wedged = bookmark(WEDGED_ID, "A Wedged Book")?;
        wedged.personal = PersonalFreeleech::ThroughWedge;
        wedged.snatched = Snatched::Before;
        assert_eq!(
            answer.result,
            Ok(MamReply::Bookmarks(BookmarkPage {
                found: BookmarkCount::new(3),
                bookmarks: vec![wedged, vip, bookmark(BOOK_ID, "Synthetic Book")?],
            }))
        );

        // One torrent is looked up by its ID, as MAM lists it now.
        let answer = ask(&client, MamRequest::LookUpTorrent { id: BOOK_ID }, &cookie).await;
        assert_eq!(
            answer.result,
            Ok(MamReply::Torrent(Some(Box::new(bookmark(
                BOOK_ID,
                "Synthetic Book"
            )?))))
        );
        let answer = ask(
            &client,
            MamRequest::LookUpTorrent { id: UNKNOWN_ID },
            &cookie,
        )
        .await;
        assert_eq!(answer.result, Ok(MamReply::Torrent(None)));

        let request = MamRequest::FetchTorrent {
            id: BOOK_ID,
            wedge: crate::core::Wedge::Spend,
        };
        let answer = ask(&client, request, &cookie).await;
        let (hash, file) = fake
            .with(|world| {
                world
                    .torrents
                    .get(&BOOK)
                    .map(|torrent| (torrent.hash(), torrent.file()))
            })
            .flatten()
            .ok_or("no torrent")?;
        assert_eq!(
            answer.result,
            Ok(MamReply::Fetched(FetchedTorrent {
                hash: TorrentHash::parse(&hash).ok_or("hash")?,
                size: ByteSize::new(1_048_576),
            }))
        );
        assert_eq!(answer.file, Some(file));
        assert_eq!(fake.with(|world| world.account.wedges), Some(199));
        // The bonus history lists the wedge, for the torrent fetched.
        let answer = ask(&client, MamRequest::WedgeHistory, &cookie).await;
        assert!(
            matches!(
                &answer.result,
                Ok(MamReply::WedgeHistory(spent)) if spent.iter().any(|entry| entry.torrent == BOOK_ID)
            ),
            "{:?}",
            answer.result
        );

        let upload = MamRequest::Buy(Purchase::Upload(UploadCredit::try_new(50)?));
        let answer = ask(&client, upload, &cookie).await;
        assert_eq!(
            answer.result,
            Ok(MamReply::Purchased {
                points: Points::new(35_000)
            })
        );
        let answer = ask(&client, MamRequest::Buy(Purchase::Vip), &cookie).await;
        assert!(matches!(answer.result, Ok(MamReply::Purchased { .. })));

        let journal = fake.journal();
        assert!(fake.violations().is_empty(), "{:?}", fake.violations());
        assert!(
            journal
                .iter()
                .all(|request| request.user_agent.as_deref() == Some(USER_AGENT))
        );
        assert!(USER_AGENT.starts_with("Windlass/"));
        let paths: Vec<(Host, &str, &str, &str)> = journal
            .iter()
            .map(|request| {
                (
                    request.host,
                    request.method.as_str(),
                    request.path.as_str(),
                    request.query.as_str(),
                )
            })
            .collect();
        assert_eq!(
            paths,
            [
                (Host::Tracker, "GET", "/json/jsonIp.php", ""),
                (Host::Tracker, "GET", "/json/dynamicSeedbox.php", ""),
                (Host::Tracker, "GET", "/json/dynamicSeedbox.php", ""),
                (
                    Host::Site,
                    "GET",
                    "/jsonLoad.php",
                    "snatch_summary=true&notif=true&clientStats=true"
                ),
                (
                    Host::Site,
                    "GET",
                    "/jsonLoad.php",
                    "snatch_summary=true&notif=true"
                ),
                (Host::Site, "POST", "/tor/js/loadSearchJSONbasic.php", ""),
                (
                    Host::Site,
                    "GET",
                    "/tor/js/loadSearchJSONbasic.php",
                    "tor%5Bid%5D=4242"
                ),
                (
                    Host::Site,
                    "GET",
                    "/tor/js/loadSearchJSONbasic.php",
                    "tor%5Bid%5D=1"
                ),
                (Host::Site, "GET", "/tor/download.php", "tid=4242&fl=1"),
                (
                    Host::Site,
                    "GET",
                    "/json/userBonusHistory.php",
                    "type%5B%5D=wedgePF"
                ),
                (
                    Host::Site,
                    "GET",
                    "/json/bonusBuy.php",
                    "spendtype=upload&amount=50"
                ),
                (
                    Host::Site,
                    "GET",
                    "/json/bonusBuy.php",
                    "spendtype=VIP&duration=max"
                ),
            ]
        );
        assert_eq!(
            journal.first().map(|request| request.cookie.clone()),
            Some(None),
            "the IP lookup sends no cookie"
        );
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "one scenario walks every refusal and fault the client tells apart"
    )]
    async fn refusals_and_faults_are_told_apart() -> TestResult {
        let (fake, client, cookie) = fake().await?;
        let load = MamRequest::LoadUserData {
            clients: crate::core::ClientListing::Omit,
        };
        let failure = |answer: Answer| answer.result.err();

        assert_eq!(
            failure(perform(None, &load, Some(&cookie)).await),
            Some(MamFailure::Unreachable)
        );
        assert_eq!(
            failure(perform(Some(&client), &load, None).await),
            Some(MamFailure::Session(SessionRejection::InvalidCookie))
        );
        let stranger = ApiCookie::parse("unknown")?;
        for request in [load.clone(), MamRequest::RegisterSeedbox] {
            assert_eq!(
                failure(ask(&client, request, &stranger).await),
                Some(MamFailure::Session(SessionRejection::InvalidCookie))
            );
        }
        let unknown = MamRequest::FetchTorrent {
            id: UNKNOWN_ID,
            wedge: crate::core::Wedge::Keep,
        };
        assert_eq!(
            failure(ask(&client, unknown, &cookie).await),
            Some(MamFailure::Refused { message: None })
        );
        fake.with(|world| world.account.points = 10.0);
        let upload = MamRequest::Buy(Purchase::Upload(UploadCredit::try_new(50)?));
        assert_eq!(
            failure(ask(&client, upload, &cookie).await),
            Some(MamFailure::Refused {
                message: MamMessage::try_new("Not enough points").ok()
            })
        );

        assert!(
            ask(&client, MamRequest::RegisterSeedbox, &cookie)
                .await
                .result
                .is_ok()
        );
        assert_eq!(
            failure(ask(&client, MamRequest::RegisterSeedbox, &cookie).await),
            Some(MamFailure::TooSoon { retry_after: None })
        );
        fake.with(|world| {
            world.clock = SystemTime::now().checked_add(Duration::from_hours(2));
            for session in &mut world.sessions {
                session.may_register = false;
            }
        });
        assert_eq!(
            failure(ask(&client, MamRequest::RegisterSeedbox, &cookie).await),
            Some(MamFailure::Session(SessionRejection::NotAllowed))
        );

        let body = |text: &str| Fault::Body(200, text.to_owned());
        // Accounts missing each required field in turn, then a bookmark page
        // with an entry that has no ID: not understood as a whole.
        let accounts = [
            "{\"uploaded_bytes\":1}",
            "{\"uploaded_bytes\":1,\"downloaded_bytes\":0}",
            "{\"uploaded_bytes\":1,\"downloaded_bytes\":0,\"seedbonus\":\"many\"}",
            "{\"uploaded_bytes\":1,\"downloaded_bytes\":0,\"seedbonus\":1}",
            "{\"uploaded_bytes\":1,\"downloaded_bytes\":0,\"seedbonus\":1,\"wedges\":5000000000}",
            "{\"uploaded_bytes\":1,\"downloaded_bytes\":0,\"seedbonus\":1,\"wedges\":1}",
        ];
        fake.with(|world| {
            for account in accounts {
                world.fail("/jsonLoad.php", body(account), 1);
            }
            world.fail("/json/jsonIp.php", body("{\"ASN\":1}"), 1);
            world.fail(
                "/tor/js/loadSearchJSONbasic.php",
                body("{\"found\":2,\"data\":[{\"id\":0},{\"title\":\"no id\"}]}"),
                1,
            );
        });
        for _ in accounts {
            assert_eq!(
                failure(ask(&client, load.clone(), &cookie).await),
                Some(MamFailure::Malformed)
            );
        }
        assert_eq!(
            failure(ask(&client, MamRequest::LookUpIp, &cookie).await),
            Some(MamFailure::Malformed)
        );
        assert_eq!(
            failure(
                ask(
                    &client,
                    MamRequest::ListBookmarks {
                        start: crate::core::BookmarkCount::new(0)
                    },
                    &cookie
                )
                .await
            ),
            Some(MamFailure::Malformed)
        );
        fake.with(|world| {
            world.fail("/jsonLoad.php", Fault::TooSoon, 1);
            world.fail("/jsonLoad.php", Fault::Status(500), 1);
            world.fail("/json/dynamicSeedbox.php", Fault::Status(502), 1);
            for (path, fault) in [
                ("/jsonLoad.php", Fault::Malformed),
                ("/jsonLoad.php", body("{}")),
                ("/json/jsonIp.php", body("{\"ip\":\"192.0.2.1\"}")),
                (
                    "/json/dynamicSeedbox.php",
                    body("{\"msg\":\"Something else\"}"),
                ),
                (
                    "/json/dynamicSeedbox.php",
                    body("{\"msg\":\"Completed\",\"ip\":\"not an address\"}"),
                ),
                ("/json/bonusBuy.php", body("{}")),
                ("/tor/js/loadSearchJSONbasic.php", body("{\"data\":[]}")),
            ] {
                world.fail(path, fault, 1);
            }
        });
        assert_eq!(
            failure(ask(&client, load.clone(), &cookie).await),
            Some(MamFailure::TooSoon {
                retry_after: Some(Duration::from_mins(2))
            })
        );
        assert_eq!(
            failure(ask(&client, load.clone(), &cookie).await),
            Some(MamFailure::Rejected {
                status: crate::core::HttpStatus::new(http::StatusCode::INTERNAL_SERVER_ERROR)
            })
        );
        assert_eq!(
            failure(ask(&client, MamRequest::RegisterSeedbox, &cookie).await),
            Some(MamFailure::Rejected {
                status: crate::core::HttpStatus::new(http::StatusCode::BAD_GATEWAY)
            })
        );
        for request in [
            load.clone(),
            load.clone(),
            MamRequest::LookUpIp,
            MamRequest::RegisterSeedbox,
            MamRequest::RegisterSeedbox,
            MamRequest::Buy(Purchase::Vip),
            MamRequest::ListBookmarks {
                start: crate::core::BookmarkCount::new(0),
            },
        ] {
            assert_eq!(
                failure(ask(&client, request.clone(), &cookie).await),
                Some(MamFailure::Malformed),
                "{request:?}"
            );
        }

        // The session is locked to an ASN the VPN no longer exits from.
        fake.with(|world| world.asn = 64_500);
        for request in [load, MamRequest::RegisterSeedbox] {
            assert_eq!(
                failure(ask(&client, request, &cookie).await),
                Some(MamFailure::Session(SessionRejection::AsnMismatch))
            );
        }

        let nowhere = Client::new(&MamHosts {
            site: "http://127.0.0.1:9/".parse()?,
            tracker: "http://127.0.0.1:9/".parse()?,
        })
        .ok_or("client")?;
        assert_eq!(
            failure(ask(&nowhere, MamRequest::LookUpIp, &cookie).await),
            Some(MamFailure::Unreachable)
        );
        assert_eq!(
            fake.violations(),
            ["seedbox registration more than once an hour"]
        );
        Ok(())
    }

    #[test]
    fn hosts_that_cannot_carry_mams_paths_make_no_client() -> TestResult {
        let unusable = MamHosts {
            site: "mailto:owner@example.org".parse()?,
            tracker: "mailto:owner@example.org".parse()?,
        };
        assert!(Client::new(&unusable).is_none());
        Ok(())
    }
}
