//! MAM's API over HTTPS: send the request the core chose, and translate the
//! answer into core terms. Every request names Windlass and its version.

use std::time::{Duration, SystemTime};

use reqwest::{StatusCode, Url, header};
use serde_json::{Map, Value, json};
use time::{PrimitiveDateTime, macros::format_description};

use crate::core::{
    ApiCookie, Bookmark, BookmarkPage, ByteSize, ClientVersion, Connectable, FetchedTorrent,
    IpObservation, ListedClient, MamFailure, MamNotices, MamReply, MamRequest, MamTorrentId,
    PersonalFreeleech, Points, Purchase, RedCategory, Registration, SessionRejection,
    SnatchSummary, UserData, WedgeSpent,
};

use super::torrent_file;

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
        MamRequest::LoadUserData { client_stats } => {
            let mut url = client.user_data.clone();
            url.query_pairs_mut()
                .append_pair("snatch_summary", "true")
                .append_pair("notif", "true");
            if *client_stats {
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
            if *wedge {
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
    let json = || serde_json::from_slice::<Value>(body).map_err(|_| MamFailure::Malformed);
    let understood = |reply: Option<MamReply>| reply.ok_or(MamFailure::Malformed);
    let result = match request {
        _ if signed_out(body) => Err(MamFailure::Session(rejection(&String::from_utf8_lossy(
            body,
        )))),
        _ if status == StatusCode::TOO_MANY_REQUESTS => Err(MamFailure::TooSoon { retry_after }),
        MamRequest::RegisterSeedbox => registration(status, body),
        _ if !status.is_success() => Err(MamFailure::Rejected {
            status: status.as_u16(),
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
        MamRequest::LookUpIp => {
            json().and_then(|value| understood(ip_observation(&value).map(MamReply::Ip)))
        }
        MamRequest::LoadUserData { .. } => json().and_then(|value| {
            understood(user_data(&value).map(|data| MamReply::UserData(Box::new(data))))
        }),
        MamRequest::ListBookmarks { .. } => {
            json().and_then(|value| understood(bookmarks(&value).map(MamReply::Bookmarks)))
        }
        MamRequest::Buy(_) => json().and_then(|value| purchase(&value)),
        MamRequest::LookUpTorrent { id } => json().and_then(|value| {
            understood(bookmarks(&value).map(|page| {
                MamReply::Torrent(
                    page.bookmarks
                        .into_iter()
                        .find(|listing| listing.id == *id)
                        .map(Box::new),
                )
            }))
        }),
        MamRequest::WedgeHistory => {
            json().and_then(|value| understood(wedge_history(&value).map(MamReply::WedgeHistory)))
        }
    };
    (result, None)
}

/// Whether the body is MAM's plain-text answer to a refused session, such as
/// `Error, you are not signed in <br />Session ASN mismatch`.
fn signed_out(body: &[u8]) -> bool {
    body.trim_ascii_start()
        .starts_with(b"Error, you are not signed in")
}

fn rejection(message: &str) -> SessionRejection {
    let message = message.to_ascii_lowercase();
    if message.contains("asn mismatch") {
        SessionRejection::AsnMismatch
    } else if message.contains("ip mismatch") {
        SessionRejection::IpMismatch
    } else if message.contains("session type") {
        SessionRejection::NotAllowed
    } else if message.contains("cookie") {
        SessionRejection::InvalidCookie
    } else {
        SessionRejection::Other
    }
}

/// The seedbox registration's answer, which says what MAM did in `msg`.
fn registration(status: StatusCode, body: &[u8]) -> Result<MamReply, MamFailure> {
    let value: Value = serde_json::from_slice(body).unwrap_or_default();
    let message = text(&value, "msg").unwrap_or_default();
    let registration = match (status, message.to_ascii_lowercase().as_str()) {
        (StatusCode::FORBIDDEN, _) => return Err(MamFailure::Session(rejection(&message))),
        (status, _) if !status.is_success() => {
            return Err(MamFailure::Rejected {
                status: status.as_u16(),
            });
        }
        (_, "completed") => Registration::Completed,
        (_, "no change") => Registration::NoChange,
        _ => return Err(MamFailure::Malformed),
    };
    let ip = text(&value, "ip").and_then(|ip| ip.parse().ok());
    ip.map(|ip| MamReply::Registered { registration, ip })
        .ok_or(MamFailure::Malformed)
}

/// The store's answer: the balance after a sale, or why it refused.
fn purchase(value: &Value) -> Result<MamReply, MamFailure> {
    let balance = value.get("seedbonus").and_then(float);
    match (value.get("success").and_then(Value::as_bool), balance) {
        (Some(true), Some(balance)) => Ok(MamReply::Purchased {
            points: Points::from_balance(balance),
        }),
        (Some(false), _) => Err(MamFailure::Refused {
            message: text(value, "error"),
        }),
        _ => Err(MamFailure::Malformed),
    }
}

/// User Bonus History: a list, newest first. Only personal freeleech wedges
/// that name a torrent are kept.
fn wedge_history(value: &Value) -> Option<Vec<WedgeSpent>> {
    let entries = value.as_array()?;
    Some(
        entries
            .iter()
            .filter(|entry| text(entry, "type").as_deref() == Some("wedgePF"))
            .filter_map(|entry| {
                Some(WedgeSpent {
                    torrent: MamTorrentId::new(number(entry.get("tid")?)?)?,
                    at: SystemTime::UNIX_EPOCH.checked_add(
                        Duration::try_from_secs_f64(float(entry.get("timestamp")?)?).ok()?,
                    )?,
                })
            })
            .collect(),
    )
}

fn ip_observation(value: &Value) -> Option<IpObservation> {
    Some(IpObservation {
        ip: text(value, "ip")?.parse().ok()?,
        asn: crate::core::Asn(u32::try_from(number(value.get("ASN")?)?).ok()?),
        provider: text(value, "AS").unwrap_or_default(),
    })
}

fn user_data(value: &Value) -> Option<UserData> {
    let count = |name: &str| number(value.get(name)?);
    Some(UserData {
        uploaded: ByteSize(count("uploaded_bytes")?),
        downloaded: ByteSize(count("downloaded_bytes")?),
        points: Points::from_balance(float(value.get("seedbonus")?)?),
        wedges: u32::try_from(count("wedges")?).ok()?,
        class: text(value, "classname")?,
        vip_until: text(value, "vip_until").and_then(|until| utc(&until)),
        summary: value
            .get("snatch_summary")
            .and_then(Value::as_object)
            .map(snatch_summary),
        notices: value.get("notifs").and_then(Value::as_object).map(notices),
        clients: value.get("clientStats").and_then(clients),
    })
}

/// MAM's client list: a list, or an object of entries. An entry's shape is not
/// yet captured, so each is searched for what Windlass needs.
fn clients(value: &Value) -> Option<Vec<ListedClient>> {
    let entries: Vec<&Value> = match value {
        Value::Array(entries) => entries.iter().collect(),
        Value::Object(entries) => entries.values().collect(),
        _ => return None,
    };
    Some(entries.into_iter().map(listed_client).collect())
}

fn listed_client(entry: &Value) -> ListedClient {
    let fields: Vec<(&str, &Value)> = match entry {
        Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| (key.as_str(), value))
            .collect(),
        other => vec![("", other)],
    };
    let qbittorrent = fields
        .iter()
        .filter_map(|(_, value)| value.as_str())
        .find_map(qbittorrent_version);
    let connectable = fields
        .iter()
        .find(|(key, _)| key.to_ascii_lowercase().contains("connect"))
        .and_then(|(_, value)| match value {
            Value::String(text) => Some(Connectable::parse(text)),
            Value::Bool(true) => Some(Connectable::Yes),
            Value::Bool(false) => Some(Connectable::No),
            Value::Number(number) => Some(if number.as_u64() == Some(0) {
                Connectable::No
            } else {
                Connectable::Yes
            }),
            _ => None,
        });
    let refused = fields.iter().any(|(key, value)| {
        let key = key.to_ascii_lowercase();
        match value {
            Value::String(text) => refusal(text),
            Value::Bool(flag) => {
                (*flag && refusal(&key))
                    || (!*flag && ["allowed", "approved", "authorized"].contains(&key.as_str()))
            }
            _ => false,
        }
    });
    ListedClient {
        qbittorrent,
        connectable,
        refused,
    }
}

/// Whether text says a client is not allowed.
fn refusal(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "not allowed",
        "disallowed",
        "unauthori",
        "banned",
        "blocked",
    ]
    .iter()
    .any(|word| text.contains(word))
}

/// A qBittorrent version named in text, such as `qBittorrent 5.1.2`,
/// `qBittorrent/v5.1.2`, or the peer ID prefix `-qB5120-`.
fn qbittorrent_version(text: &str) -> Option<ClientVersion> {
    let lower = text.to_ascii_lowercase();
    if let Some(at) = lower.find("qbittorrent") {
        let rest = lower.get(at + "qbittorrent".len()..)?;
        let version: String = rest
            .trim_start_matches([' ', '/', 'v', '-', ':'])
            .chars()
            .take_while(|character| character.is_ascii_digit() || *character == '.')
            .collect();
        return ClientVersion::parse(version.trim_end_matches('.'));
    }
    let at = text.find("-qB")?;
    let digits = text.get(at + 3..at + 7)?;
    let mut parts = digits.chars().map(|digit| digit.to_digit(10));
    let (Some(Some(major)), Some(Some(minor)), Some(Some(patch))) =
        (parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    ClientVersion::parse(&format!("{major}.{minor}.{patch}"))
}

fn snatch_summary(summary: &Map<String, Value>) -> SnatchSummary {
    let field = |category: &str, name: &str| {
        summary
            .get(category)
            .and_then(|category| category.get(name))
            .and_then(number)
            .and_then(|count| u32::try_from(count).ok())
            .unwrap_or_default()
    };
    let red = summary
        .values()
        .filter_map(|category| {
            let red = category.get("red").and_then(Value::as_bool)?;
            let count = category
                .get("count")
                .and_then(number)
                .and_then(|count| u32::try_from(count).ok())?;
            (red && count > 0).then(|| RedCategory {
                name: text(category, "name").unwrap_or_default(),
                count,
            })
        })
        .collect();
    SnatchSummary {
        unsatisfied: field("unsat", "count"),
        unsatisfied_limit: field("unsat", "limit"),
        unsatisfied_not_seeding: field("inactUnsat", "count"),
        hit_and_run_not_seeding: field("inactHnr", "count"),
        partial: summary
            .get("partial")
            .and_then(Value::as_bool)
            .unwrap_or_default(),
        duplicates: field("duplicates", "count"),
        tracker_errors: field("ite", "count"),
        connectable: summary
            .get("connectable")
            .and_then(Value::as_str)
            .map(Connectable::parse)
            .unwrap_or_default(),
        red,
    }
}

fn notices(notices: &Map<String, Value>) -> MamNotices {
    let count = |name: &str| {
        notices
            .get(name)
            .and_then(number)
            .and_then(|count| u32::try_from(count).ok())
            .unwrap_or_default()
    };
    MamNotices {
        messages: count("pms"),
        tickets: count("tickets"),
        waiting_tickets: count("waiting_tickets"),
        requests: count("requests"),
        topics: count("topics"),
        about_to_drop_client: count("aboutToDropClient"),
    }
}

fn bookmarks(value: &Value) -> Option<BookmarkPage> {
    let found = u32::try_from(number(value.get("found")?)?).ok()?;
    let bookmarks = value
        .get("data")
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter_map(bookmark).collect())
        .unwrap_or_default();
    Some(BookmarkPage { found, bookmarks })
}

fn bookmark(value: &Value) -> Option<Bookmark> {
    let flag = |name: &str| {
        value
            .get(name)
            .and_then(number)
            .is_some_and(|flag| flag != 0)
    };
    let time = |name: &str| {
        value
            .get(name)
            .and_then(number)
            .filter(|seconds| *seconds > 0)
            .and_then(|seconds| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds)))
    };
    Some(Bookmark {
        id: MamTorrentId::new(number(value.get("id")?)?)?,
        title: text(value, "title").unwrap_or_default(),
        vip: flag("vip"),
        free: flag("free"),
        personal: PersonalFreeleech::from_flags(flag("personal_freeleech"), flag("fl_vip")),
        snatched: flag("my_snatched"),
        vip_expires: time("vip_expire"),
        bookmarked_at: time("bookmarked"),
    })
}

/// A whole number MAM sends as a number or as text.
fn number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

/// A number with a possible fraction, sent as a number or as text.
fn float(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

fn text(value: &Value, name: &str) -> Option<String> {
    value.get(name).and_then(Value::as_str).map(str::to_owned)
}

/// MAM's `YYYY-MM-DD HH:MM:SS`, which is UTC.
fn utc(value: &str) -> Option<SystemTime> {
    let format = format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    let time = PrimitiveDateTime::parse(value.trim(), format).ok()?;
    Some(time.assume_utc().into())
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        net::IpAddr,
        time::{Duration, SystemTime},
    };

    use crate::core::{
        ApiCookie, Asn, Bookmark, BookmarkPage, ByteSize, Connectable, FetchedTorrent,
        IpObservation, MamFailure, MamNotices, MamReply, MamRequest, MamTorrentId,
        PersonalFreeleech, Points, Purchase, RedCategory, Registration, SessionRejection,
        SnatchSummary, TorrentHash, UploadCredit, UserData,
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

    #[test]
    fn bonus_history_keeps_wedges_spent_on_torrents() {
        use serde_json::json;

        let history = json!([
            {"timestamp": 1_790_000_000.5, "amount": -1, "type": "wedgePF", "tid": 330_896},
            {"timestamp": 1_641_914_230.1, "amount": -1, "type": "giftWedge", "tid": null},
            {"timestamp": 1_610_486_584.1, "amount": -10, "type": "wedgeGFL", "tid": 220_870},
            {"timestamp": "not a time", "type": "wedgePF", "tid": 5},
            {"timestamp": 1.0, "type": "wedgePF", "tid": 0},
            {"timestamp": 1.0e19, "type": "wedgePF", "tid": 6},
        ]);
        let spent = super::wedge_history(&history).unwrap_or_default();
        assert_eq!(spent.len(), 1);
        assert_eq!(
            spent.first().map(|entry| entry.torrent.get()),
            Some(330_896)
        );
        assert_eq!(super::wedge_history(&json!({"error": "no"})), None);
    }

    #[test]
    fn client_entries_are_searched_for_what_windlass_needs() {
        use serde_json::json;

        use crate::core::{ClientVersion, Connectable, ListedClient};

        let version = |text| ClientVersion::parse(text);
        let list = json!([
            {"client": "qBittorrent 5.1.2", "connectable": "yes", "torrents": 3},
            {"agent": "qBittorrent/v5.0.4", "connectable": false, "status": "Not allowed"},
            {"peer_id": "-qB5200-abcdefghijkl", "connectable": 0},
            {"client": "Transmission 4.0", "isConnectable": 1, "banned": true},
            {"client": "qbittorrent", "allowed": false},
            "qBittorrent 5.1.1",
            {"client": "-qB52x0-"},
            {"client": "qBittorrent 5.1.3", "connectable": true},
        ]);
        let clients = super::clients(&list).unwrap_or_default();
        assert_eq!(
            clients,
            [
                ListedClient {
                    qbittorrent: version("5.1.2"),
                    connectable: Some(Connectable::Yes),
                    refused: false,
                },
                ListedClient {
                    qbittorrent: version("5.0.4"),
                    connectable: Some(Connectable::No),
                    refused: true,
                },
                ListedClient {
                    qbittorrent: version("5.2.0"),
                    connectable: Some(Connectable::No),
                    refused: false,
                },
                ListedClient {
                    qbittorrent: None,
                    connectable: Some(Connectable::Yes),
                    refused: true,
                },
                ListedClient {
                    qbittorrent: None,
                    connectable: None,
                    refused: true,
                },
                ListedClient {
                    qbittorrent: version("5.1.1"),
                    connectable: None,
                    refused: false,
                },
                ListedClient {
                    qbittorrent: None,
                    connectable: None,
                    refused: false,
                },
                ListedClient {
                    qbittorrent: version("5.1.3"),
                    connectable: Some(Connectable::Yes),
                    refused: false,
                },
            ]
        );
        let keyed = json!({"1": {"client": "qBittorrent 5.1.2", "connectable": null}});
        assert_eq!(super::clients(&keyed).map(|clients| clients.len()), Some(1));
        assert_eq!(super::clients(&json!("none")), None);
    }

    async fn ask(client: &Client, request: MamRequest, cookie: &ApiCookie) -> Answer {
        perform(Some(client), &request, Some(cookie)).await
    }

    fn bookmark(id: MamTorrentId, title: &str) -> Bookmark {
        Bookmark {
            id,
            title: title.to_owned(),
            vip: false,
            free: false,
            personal: PersonalFreeleech::No,
            snatched: false,
            vip_expires: None,
            bookmarked_at: Some(SystemTime::UNIX_EPOCH + BOOKMARKED),
        }
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
                asn: Asn(209_103),
                provider: String::from("ProtonVPN Proton AG"),
            }))
        );
        assert_eq!(answer.rotated, None, "the IP lookup sends no cookie");

        for registration in [Registration::Completed, Registration::NoChange] {
            let answer = ask(&client, MamRequest::RegisterSeedbox, &cookie).await;
            assert_eq!(answer.result, Ok(MamReply::Registered { registration, ip }));
            fake.with(|world| world.clock = SystemTime::now().checked_add(Duration::from_hours(2)));
        }
        fake.with(|world| world.clock = None);

        let user = |clients| UserData {
            uploaded: ByteSize(80 << 30),
            downloaded: ByteSize(0),
            points: Points(60_000),
            wedges: 200,
            class: String::from("VIP"),
            vip_until: Some(SystemTime::UNIX_EPOCH + VIP_UNTIL),
            summary: Some(SnatchSummary {
                unsatisfied: 0,
                unsatisfied_limit: 150,
                unsatisfied_not_seeding: 2,
                hit_and_run_not_seeding: 0,
                partial: false,
                duplicates: 1,
                tracker_errors: 0,
                connectable: Connectable::Offline,
                red: vec![
                    RedCategory {
                        name: String::from("Duplicate peer entries"),
                        count: 1,
                    },
                    RedCategory {
                        name: String::from("Not Seeding - Not Yet Satisfied"),
                        count: 2,
                    },
                ],
            }),
            notices: Some(MamNotices::default()),
            clients,
        };
        let answer = ask(
            &client,
            MamRequest::LoadUserData { client_stats: true },
            &cookie,
        )
        .await;
        assert_eq!(
            answer.result,
            Ok(MamReply::UserData(Box::new(user(Some(vec![
                crate::core::ListedClient {
                    qbittorrent: crate::core::ClientVersion::parse("5.2.2"),
                    connectable: Some(Connectable::Yes),
                    refused: false,
                }
            ])))))
        );
        let rotated = answer.rotated.ok_or("MAM issued no new cookie")?;
        assert_ne!(rotated, cookie);
        let answer = ask(
            &client,
            MamRequest::LoadUserData {
                client_stats: false,
            },
            &rotated,
        )
        .await;
        assert_eq!(answer.result, Ok(MamReply::UserData(Box::new(user(None)))));

        let answer = ask(&client, MamRequest::ListBookmarks { start: 0 }, &cookie).await;
        let mut vip = bookmark(VIP_ID, "A VIP Book");
        vip.vip = true;
        vip.free = true;
        vip.personal = PersonalFreeleech::ThroughVip;
        vip.vip_expires = Some(SystemTime::UNIX_EPOCH + VIP_UNTIL);
        let mut wedged = bookmark(WEDGED_ID, "A Wedged Book");
        wedged.personal = PersonalFreeleech::ThroughWedge;
        wedged.snatched = true;
        assert_eq!(
            answer.result,
            Ok(MamReply::Bookmarks(BookmarkPage {
                found: 3,
                bookmarks: vec![wedged, vip, bookmark(BOOK_ID, "Synthetic Book")],
            }))
        );

        // One torrent is looked up by its ID, as MAM lists it now.
        let answer = ask(&client, MamRequest::LookUpTorrent { id: BOOK_ID }, &cookie).await;
        assert_eq!(
            answer.result,
            Ok(MamReply::Torrent(Some(Box::new(bookmark(
                BOOK_ID,
                "Synthetic Book"
            )))))
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
            wedge: true,
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
                size: ByteSize(1_048_576),
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
                points: Points(35_000)
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
            client_stats: false,
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
            wedge: false,
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
                message: Some(String::from("Not enough points"))
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
        // whose entries are skipped for want of an ID.
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
            ask(&client, MamRequest::ListBookmarks { start: 0 }, &cookie)
                .await
                .result,
            Ok(MamReply::Bookmarks(BookmarkPage {
                found: 2,
                bookmarks: Vec::new()
            }))
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
            Some(MamFailure::Rejected { status: 500 })
        );
        assert_eq!(
            failure(ask(&client, MamRequest::RegisterSeedbox, &cookie).await),
            Some(MamFailure::Rejected { status: 502 })
        );
        for request in [
            load.clone(),
            load.clone(),
            MamRequest::LookUpIp,
            MamRequest::RegisterSeedbox,
            MamRequest::RegisterSeedbox,
            MamRequest::Buy(Purchase::Vip),
            MamRequest::ListBookmarks { start: 0 },
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
    fn mam_texts_are_read_as_mam_means_them() -> TestResult {
        use super::{float, number, rejection, signed_out, utc};

        // Some numbers arrive as text.
        assert_eq!(number(&serde_json::json!(" 12 ")), Some(12));
        assert_eq!(float(&serde_json::json!("1.5")), Some(1.5));
        assert_eq!(number(&serde_json::json!(null)), None);

        for (message, expected) in [
            (
                "Invalid session - IP mismatch",
                SessionRejection::IpMismatch,
            ),
            (
                "Invalid session - ASN mismatch",
                SessionRejection::AsnMismatch,
            ),
            (
                "Invalid session - Invalid Cookie",
                SessionRejection::InvalidCookie,
            ),
            ("No Session Cookie", SessionRejection::InvalidCookie),
            (
                "Incorrect session type - non-API session",
                SessionRejection::NotAllowed,
            ),
            ("Invalid session - Other", SessionRejection::Other),
        ] {
            assert_eq!(rejection(message), expected, "{message}");
        }
        assert!(signed_out(
            b"  Error, you are not signed in <br />Invalid/missing cookie"
        ));
        assert!(!signed_out(b"{\"ok\":true}"));
        assert_eq!(
            utc("2026-12-09 10:14:13"),
            Some(SystemTime::UNIX_EPOCH + VIP_UNTIL)
        );
        assert_eq!(utc("soon"), None);
        for (text, expected) in [
            ("yes", Connectable::Yes),
            ("No", Connectable::No),
            (" offline ", Connectable::Offline),
            ("maybe", Connectable::Unrecognised(String::from("maybe"))),
        ] {
            assert_eq!(Connectable::parse(text), expected);
        }
        let unusable = MamHosts {
            site: "mailto:owner@example.org".parse()?,
            tracker: "mailto:owner@example.org".parse()?,
        };
        assert!(Client::new(&unusable).is_none());
        Ok(())
    }
}
