//! Parse MAM's answers into the core's types.
//!
//! Each JSON answer is read into structs that mirror MAM's fields, then
//! translated into core types once. Every field Windlass relies on is
//! required: an answer that lacks one, or holds a value outside its type, is
//! not understood as a whole, so the core never acts on a guess such as an
//! unsatisfied count of zero. The client list is the one exception: its shape
//! is not captured yet, so each entry is searched for what Windlass needs.

use std::{
    net::IpAddr,
    time::{Duration, SystemTime},
};

use reqwest::StatusCode;
use serde::{Deserialize, Deserializer, de, de::DeserializeOwned};
use serde_json::{Map, Value};
use time::{PrimitiveDateTime, macros::format_description};

use crate::core::{
    Asn, BookTitle, Bookmark, BookmarkCount, BookmarkPage, ByteSize, CategoryName, ClientRefusal,
    ClientVersion, Connectable, Freeleech, IpObservation, ListedClient, MamFailure, MamMessage,
    MamNotices, MamReply, MamTorrentId, NetworkName, NoticeCount, Partials, PersonalFreeleech,
    Points, RedCategory, RedKind, Registration, SessionRejection, SnatchSummary, Snatched,
    TorrentCount, UnsatisfiedCount, UnsatisfiedLimit, UserClass, UserData, VipTorrent, WedgeCount,
    WedgeSpent,
};

/// An answer that could not be understood.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Malformed;

impl From<Malformed> for MamFailure {
    fn from(_: Malformed) -> Self {
        Self::Malformed
    }
}

type Parsed<T> = Result<T, Malformed>;

/// A whole number MAM sends as a JSON number or as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Whole(u64);

impl<'de> Deserialize<'de> for Whole {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Sent {
            Number(u64),
            Text(String),
        }
        match Sent::deserialize(deserializer)? {
            Sent::Number(number) => Ok(Self(number)),
            Sent::Text(text) => text.trim().parse().map(Self).map_err(de::Error::custom),
        }
    }
}

impl Whole {
    fn count(self) -> Parsed<u32> {
        u32::try_from(self.0).map_err(|_| Malformed)
    }
}

/// A number with a possible fraction, sent as a JSON number or as text.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Decimal(f64);

impl<'de> Deserialize<'de> for Decimal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Sent {
            Number(f64),
            Text(String),
        }
        match Sent::deserialize(deserializer)? {
            Sent::Number(number) => Ok(Self(number)),
            Sent::Text(text) => text.trim().parse().map(Self).map_err(de::Error::custom),
        }
    }
}

/// A yes or no MAM sends as `0` or `1`, as a number or text, or as a boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Flag(bool);

impl<'de> Deserialize<'de> for Flag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Sent {
            Boolean(bool),
            Number(u64),
            Text(String),
        }
        match Sent::deserialize(deserializer)? {
            Sent::Boolean(flag) => Ok(Self(flag)),
            Sent::Number(0) => Ok(Self(false)),
            Sent::Number(1) => Ok(Self(true)),
            Sent::Text(text) if text.trim() == "0" => Ok(Self(false)),
            Sent::Text(text) if text.trim() == "1" => Ok(Self(true)),
            Sent::Number(_) | Sent::Text(_) => Err(de::Error::custom("not 0 or 1")),
        }
    }
}

fn json<T: DeserializeOwned>(body: &[u8]) -> Parsed<T> {
    serde_json::from_slice(body).map_err(|_| Malformed)
}

/// A required entry of a JSON object.
fn entry<T: DeserializeOwned>(object: &Map<String, Value>, name: &str) -> Parsed<T> {
    let value = object.get(name).ok_or(Malformed)?;
    T::deserialize(value).map_err(|_| Malformed)
}

/// A Unix time in whole seconds.
fn unix(seconds: u64) -> Parsed<SystemTime> {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(seconds))
        .ok_or(Malformed)
}

/// MAM's `YYYY-MM-DD HH:MM:SS`, which is UTC.
pub(super) fn utc(value: &str) -> Parsed<SystemTime> {
    let format = format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    let time = PrimitiveDateTime::parse(value.trim(), format).map_err(|_| Malformed)?;
    Ok(time.assume_utc().into())
}

/// Whether the body is MAM's plain-text answer to a refused session, such as
/// `Error, you are not signed in <br />Session ASN mismatch`.
pub(super) fn signed_out(body: &[u8]) -> bool {
    body.trim_ascii_start()
        .starts_with(b"Error, you are not signed in")
}

/// Why MAM refused a session, from its message.
pub(super) fn rejection(message: &str) -> SessionRejection {
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

#[derive(Deserialize)]
struct IpAnswer {
    ip: IpAddr,
    #[serde(rename = "ASN")]
    asn: Whole,
    /// The network's name; empty when MAM has none.
    #[serde(rename = "AS")]
    network: Option<String>,
}

/// The IP lookup.
pub(super) fn ip_observation(body: &[u8]) -> Parsed<IpObservation> {
    let answer: IpAnswer = json(body)?;
    Ok(IpObservation {
        ip: answer.ip,
        asn: Asn::new(answer.asn.count()?),
        provider: answer
            .network
            .and_then(|name| NetworkName::try_new(name).ok()),
    })
}

#[derive(Deserialize)]
struct SeedboxAnswer {
    msg: Option<String>,
    ip: Option<IpAddr>,
}

/// The seedbox registration's answer, which says what MAM did in `msg`. A
/// refusal may come with a body that is not JSON.
pub(super) fn registration(status: StatusCode, body: &[u8]) -> Result<MamReply, MamFailure> {
    let answer: Option<SeedboxAnswer> = json(body).ok();
    let message = answer
        .as_ref()
        .and_then(|answer| answer.msg.as_deref())
        .unwrap_or_default();
    if status == StatusCode::FORBIDDEN {
        return Err(MamFailure::Session(rejection(message)));
    }
    if !status.is_success() {
        return Err(MamFailure::Rejected {
            status: crate::core::HttpStatus::new(status),
        });
    }
    let registration = match message.to_ascii_lowercase().as_str() {
        "completed" => Registration::Completed,
        "no change" => Registration::NoChange,
        _ => return Err(MamFailure::Malformed),
    };
    let ip = answer
        .and_then(|answer| answer.ip)
        .ok_or(MamFailure::Malformed)?;
    Ok(MamReply::Registered { registration, ip })
}

#[derive(Deserialize)]
struct AccountAnswer {
    uploaded_bytes: Whole,
    downloaded_bytes: Whole,
    seedbonus: Decimal,
    wedges: Whole,
    classname: String,
    /// Absent, or null, when the account is not VIP.
    vip_until: Option<String>,
    snatch_summary: Option<Map<String, Value>>,
    notifs: Option<NoticesAnswer>,
    #[serde(rename = "clientStats")]
    client_stats: Option<Value>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct NoticesAnswer {
    pms: Whole,
    tickets: Whole,
    waiting_tickets: Whole,
    requests: Whole,
    topics: Whole,
    #[serde(rename = "aboutToDropClient")]
    about_to_drop_client: Whole,
}

/// Load User Data.
pub(super) fn user_data(body: &[u8]) -> Parsed<UserData> {
    let answer: AccountAnswer = json(body)?;
    Ok(UserData {
        uploaded: ByteSize::new(answer.uploaded_bytes.0),
        downloaded: ByteSize::new(answer.downloaded_bytes.0),
        points: Points::from_balance(answer.seedbonus.0).ok_or(Malformed)?,
        wedges: WedgeCount::new(answer.wedges.count()?),
        class: UserClass::parse(&answer.classname).ok_or(Malformed)?,
        vip_until: answer.vip_until.as_deref().map(utc).transpose()?,
        summary: answer
            .snatch_summary
            .as_ref()
            .map(snatch_summary)
            .transpose()?,
        notices: answer.notifs.map(notices).transpose()?,
        clients: answer.client_stats.as_ref().map(clients).transpose()?,
    })
}

#[derive(Deserialize)]
struct Counted {
    count: Whole,
}

impl Counted {
    fn torrents(&self) -> Parsed<TorrentCount> {
        self.count.count().map(TorrentCount::new)
    }
}

#[derive(Deserialize)]
struct Limited {
    count: Whole,
    limit: Whole,
}

#[derive(Deserialize)]
struct RedEntry {
    name: String,
    count: Whole,
    red: Flag,
}

/// MAM's snatch summary. Every category Windlass watches must be present.
fn snatch_summary(summary: &Map<String, Value>) -> Parsed<SnatchSummary> {
    let unsatisfied: Limited = entry(summary, "unsat")?;
    let not_seeding: Counted = entry(summary, "inactUnsat")?;
    let hit_and_run: Counted = entry(summary, "inactHnr")?;
    let duplicates: Counted = entry(summary, "duplicates")?;
    let tracker_errors: Counted = entry(summary, "ite")?;
    let partial: Flag = entry(summary, "partial")?;
    let connectable: String = entry(summary, "connectable")?;
    Ok(SnatchSummary {
        unsatisfied: UnsatisfiedCount::new(unsatisfied.count.count()?),
        unsatisfied_limit: UnsatisfiedLimit::new(unsatisfied.limit.count()?),
        unsatisfied_not_seeding: not_seeding.torrents()?,
        hit_and_run_not_seeding: hit_and_run.torrents()?,
        partial: if partial.0 {
            Partials::Seen
        } else {
            Partials::NotSeen
        },
        duplicates: duplicates.torrents()?,
        tracker_errors: tracker_errors.torrents()?,
        connectable: Connectable::parse(&connectable),
        red: red_categories(summary)?,
    })
}

/// Categories marked red that hold torrents. A category that carries a `red`
/// mark must also carry its name and count.
fn red_categories(summary: &Map<String, Value>) -> Parsed<Vec<RedCategory>> {
    let mut red = Vec::new();
    for (key, value) in summary {
        if value.get("red").is_none() {
            continue;
        }
        let category = RedEntry::deserialize(value).map_err(|_| Malformed)?;
        let count = TorrentCount::new(category.count.count()?);
        if !category.red.0 || count.is_zero() {
            continue;
        }
        red.push(RedCategory {
            name: CategoryName::try_new(category.name).map_err(|_| Malformed)?,
            kind: match key.as_str() {
                "inactUnsat" | "inactHnr" => RedKind::NotSeeding,
                _ => RedKind::Other,
            },
            count,
        });
    }
    Ok(red)
}

fn notices(notices: NoticesAnswer) -> Parsed<MamNotices> {
    let count = |count: Whole| count.count().map(NoticeCount::new);
    Ok(MamNotices {
        messages: count(notices.pms)?,
        tickets: count(notices.tickets)?,
        waiting_tickets: count(notices.waiting_tickets)?,
        requests: count(notices.requests)?,
        topics: count(notices.topics)?,
        about_to_drop_client: count(notices.about_to_drop_client)?,
    })
}

/// MAM's client list: a list, or an object of entries. An entry's shape is not
/// yet captured, so each is searched for what Windlass needs.
pub(super) fn clients(value: &Value) -> Parsed<Vec<ListedClient>> {
    let entries: Vec<&Value> = match value {
        Value::Array(entries) => entries.iter().collect(),
        Value::Object(entries) => entries.values().collect(),
        _ => return Err(Malformed),
    };
    Ok(entries.into_iter().map(listed_client).collect())
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
        refusal: if refused {
            ClientRefusal::Refused
        } else {
            ClientRefusal::NotRefused
        },
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

#[derive(Deserialize)]
struct SearchAnswer {
    found: Whole,
    /// Absent when nothing is found.
    data: Option<Vec<SearchResult>>,
}

#[derive(Deserialize)]
struct SearchResult {
    id: Whole,
    title: String,
    vip: Flag,
    free: Flag,
    fl_vip: Flag,
    personal_freeleech: Flag,
    my_snatched: Flag,
    /// When VIP ends; 0 when it does not.
    vip_expire: Whole,
    /// When the owner bookmarked it; null, or 0, when not bookmarked.
    bookmarked: Option<Whole>,
}

/// A page of Torrent Search results.
pub(super) fn bookmark_page(body: &[u8]) -> Parsed<BookmarkPage> {
    let answer: SearchAnswer = json(body)?;
    let found = BookmarkCount::new(answer.found.count()?);
    let results = match (answer.data, answer.found) {
        (Some(results), _) => results,
        (None, Whole(0)) => Vec::new(),
        (None, _) => return Err(Malformed),
    };
    Ok(BookmarkPage {
        found,
        bookmarks: results.into_iter().map(bookmark).collect::<Parsed<_>>()?,
    })
}

fn bookmark(result: SearchResult) -> Parsed<Bookmark> {
    let vip = match (result.vip.0, result.vip_expire) {
        (false, _) => VipTorrent::No,
        (true, Whole(0)) => VipTorrent::Indefinitely,
        (true, Whole(seconds)) => VipTorrent::Until(unix(seconds)?),
    };
    Ok(Bookmark {
        id: MamTorrentId::new(result.id.0).ok_or(Malformed)?,
        title: BookTitle::try_new(result.title).map_err(|_| Malformed)?,
        vip,
        freeleech: if result.free.0 {
            Freeleech::ForEveryone
        } else {
            Freeleech::No
        },
        personal: PersonalFreeleech::from_flags(result.personal_freeleech.0, result.fl_vip.0),
        snatched: if result.my_snatched.0 {
            Snatched::Before
        } else {
            Snatched::Never
        },
        bookmarked_at: result
            .bookmarked
            .filter(|seconds| *seconds != Whole(0))
            .map(|Whole(seconds)| unix(seconds))
            .transpose()?,
    })
}

#[derive(Deserialize)]
struct StoreAnswer {
    success: bool,
    seedbonus: Option<Decimal>,
    error: Option<String>,
}

/// The store's answer: the balance after a sale, or why it refused.
pub(super) fn purchase(body: &[u8]) -> Result<MamReply, MamFailure> {
    let answer: StoreAnswer = json(body)?;
    if !answer.success {
        return Err(MamFailure::Refused {
            message: answer
                .error
                .and_then(|message| MamMessage::try_new(message).ok()),
        });
    }
    let balance = answer.seedbonus.ok_or(Malformed)?;
    Ok(MamReply::Purchased {
        points: Points::from_balance(balance.0).ok_or(Malformed)?,
    })
}

#[derive(Deserialize)]
struct BonusEntry {
    #[serde(rename = "type")]
    kind: String,
    /// The torrent; null for entries about no torrent.
    tid: Option<Whole>,
    timestamp: Decimal,
}

/// User Bonus History: a list, newest first. Personal freeleech wedges are
/// kept, and each must name its torrent; other entries are skipped.
pub(super) fn wedge_history(body: &[u8]) -> Parsed<Vec<WedgeSpent>> {
    let entries: Vec<BonusEntry> = json(body)?;
    entries
        .into_iter()
        .filter(|entry| entry.kind == "wedgePF")
        .map(|entry| {
            let since = Duration::try_from_secs_f64(entry.timestamp.0).map_err(|_| Malformed)?;
            Ok(WedgeSpent {
                torrent: entry
                    .tid
                    .and_then(|Whole(id)| MamTorrentId::new(id))
                    .ok_or(Malformed)?,
                at: SystemTime::UNIX_EPOCH.checked_add(since).ok_or(Malformed)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use reqwest::StatusCode;
    use serde_json::{Value, json};

    use super::{
        Decimal, Flag, Malformed, Whole, bookmark_page, clients, ip_observation, purchase,
        registration, rejection, signed_out, user_data, utc, wedge_history,
    };
    use crate::core::{
        ClientRefusal, ClientVersion, Connectable, Freeleech, ListedClient, MamFailure, MamReply,
        Registration, SessionRejection, Snatched, UserClass, VipTorrent,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn bytes(value: &Value) -> Vec<u8> {
        value.to_string().into_bytes()
    }

    /// An account as MAM sends it, with everything Windlass reads.
    fn account() -> Value {
        json!({
            "uploaded_bytes": 1024, "downloaded_bytes": "512", "seedbonus": 1500.75,
            "wedges": 3, "classname": "VIP", "vip_until": "2026-12-09 10:14:13",
            "snatch_summary": {
                "unsat": {"name": "Unsatisfied", "count": 4, "limit": 150, "red": false},
                "inactUnsat": {"name": "Not Seeding - Not Yet Satisfied", "count": 1, "red": true},
                "inactHnr": {"name": "Not Seeding - H&R", "count": 0, "red": true},
                "duplicates": {"name": "Duplicate peer entries", "count": 2, "red": true},
                "ite": {"name": "Important Tracker Errors", "count": 0},
                "partial": false,
                "connectable": "yes",
                "created": 1_790_000_000,
            },
            "notifs": {"pms": 1, "tickets": 0, "waiting_tickets": 0, "requests": 0,
                       "topics": 0, "aboutToDropClient": 0},
        })
    }

    fn without(mut value: Value, path: &[&str]) -> Value {
        let (last, parents) = path.split_last().unwrap_or((&"", &[]));
        let mut object = &mut value;
        for parent in parents {
            object = &mut object[*parent];
        }
        if let Some(object) = object.as_object_mut() {
            object.remove(*last);
        }
        value
    }

    fn with(mut value: Value, path: &[&str], replacement: Value) -> Value {
        let mut object = &mut value;
        for part in path {
            object = &mut object[*part];
        }
        *object = replacement;
        value
    }

    #[test]
    fn numbers_flags_and_times_are_read_as_mam_sends_them() {
        let whole = |value: Value| serde_json::from_value::<Whole>(value).ok();
        assert_eq!(whole(json!(12)), Some(Whole(12)));
        assert_eq!(whole(json!(" 12 ")), Some(Whole(12)));
        assert_eq!(whole(json!("twelve")), None);
        assert_eq!(whole(json!(null)), None);
        let decimal = |value: Value| serde_json::from_value::<Decimal>(value).ok();
        assert_eq!(decimal(json!("1.5")), Some(Decimal(1.5)));
        assert_eq!(decimal(json!(2)), Some(Decimal(2.0)));
        assert_eq!(decimal(json!("many")), None);
        let flag = |value: Value| serde_json::from_value::<Flag>(value).ok();
        for (value, expected) in [
            (json!(0), Some(Flag(false))),
            (json!(1), Some(Flag(true))),
            (json!("1"), Some(Flag(true))),
            (json!(" 0"), Some(Flag(false))),
            (json!(true), Some(Flag(true))),
            (json!(2), None),
            (json!("yes"), None),
        ] {
            assert_eq!(flag(value.clone()), expected, "{value}");
        }
        assert_eq!(
            utc("2026-12-09 10:14:13"),
            Ok(SystemTime::UNIX_EPOCH + Duration::from_secs(1_796_811_253))
        );
        assert_eq!(utc("soon"), Err(Malformed));
        assert_eq!(MamFailure::from(Malformed), MamFailure::Malformed);
    }

    #[test]
    fn session_refusals_are_read_from_mams_text() {
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
        for (text, expected) in [
            ("yes", Connectable::Yes),
            ("No", Connectable::No),
            (" offline ", Connectable::Offline),
            ("maybe", Connectable::Unrecognised(String::from("maybe"))),
        ] {
            assert_eq!(Connectable::parse(text), expected);
        }
    }

    #[test]
    fn an_account_is_read_whole_or_not_at_all() -> TestResult {
        let data = user_data(&bytes(&account())).map_err(|_| "not understood")?;
        assert_eq!(data.class, UserClass::Vip);
        assert_eq!(data.wedges.to_string(), "3");
        assert_eq!(data.points.to_string(), "1500");
        let summary = data.summary.ok_or("no summary")?;
        assert_eq!(summary.unsatisfied.get(), 4);
        assert_eq!(summary.unsatisfied_limit.get(), 150);
        assert_eq!(
            summary
                .red
                .iter()
                .map(|red| (red.name.to_string(), red.count.get()))
                .collect::<Vec<_>>(),
            [
                (String::from("Duplicate peer entries"), 2),
                (String::from("Not Seeding - Not Yet Satisfied"), 1),
            ],
            "red categories with torrents, by MAM's key"
        );
        assert_eq!(
            data.notices.map(|notices| notices.total().to_string()),
            Some(String::from("1"))
        );

        // Without a summary or notices requested, they are absent, not zero.
        let bare = without(without(account(), &["snatch_summary"]), &["notifs"]);
        let bare = user_data(&bytes(&bare)).map_err(|_| "not understood")?;
        assert_eq!((bare.summary, bare.notices), (None, None));
        let not_vip = with(account(), &["vip_until"], Value::Null);
        assert_eq!(
            user_data(&bytes(&not_vip)).map(|data| data.vip_until),
            Ok(None)
        );
        let staff = with(account(), &["classname"], json!("Torrent Uploader"));
        assert_eq!(
            user_data(&bytes(&staff)).map(|data| data.class.to_string()),
            Ok(String::from("Torrent Uploader"))
        );

        // Each value Windlass relies on, missing or wrong, fails the whole answer.
        let missing: &[&[&str]] = &[
            &["uploaded_bytes"],
            &["seedbonus"],
            &["wedges"],
            &["classname"],
            &["snatch_summary", "unsat", "count"],
            &["snatch_summary", "unsat", "limit"],
            &["snatch_summary", "inactUnsat"],
            &["snatch_summary", "inactHnr"],
            &["snatch_summary", "duplicates"],
            &["snatch_summary", "ite"],
            &["snatch_summary", "partial"],
            &["snatch_summary", "connectable"],
            &["snatch_summary", "duplicates", "name"],
            &["notifs", "aboutToDropClient"],
        ];
        for path in missing {
            let answer = without(account(), path);
            assert_eq!(user_data(&bytes(&answer)), Err(Malformed), "{path:?}");
        }
        let wrong: &[(&[&str], Value)] = &[
            (&["seedbonus"], json!(-5)),
            (&["wedges"], json!(5_000_000_000_u64)),
            (&["classname"], json!(" ")),
            (&["vip_until"], json!("soon")),
            (&["snatch_summary", "unsat", "count"], json!("many")),
            (&["snatch_summary", "partial"], json!(2)),
            (&["snatch_summary", "duplicates", "name"], json!("")),
            (&["clientStats"], json!("none")),
        ];
        for (path, value) in wrong {
            let answer = with(account(), path, value.clone());
            assert_eq!(user_data(&bytes(&answer)), Err(Malformed), "{path:?}");
        }
        assert_eq!(user_data(b"not json"), Err(Malformed));
        Ok(())
    }

    #[test]
    fn account_counts_and_classes_are_checked() {
        let too_many = json!(5_000_000_000_u64);
        let counts: &[&[&str]] = &[
            &["wedges"],
            &["snatch_summary", "unsat", "count"],
            &["snatch_summary", "unsat", "limit"],
            &["snatch_summary", "inactUnsat", "count"],
            &["snatch_summary", "inactHnr", "count"],
            &["snatch_summary", "duplicates", "count"],
            &["snatch_summary", "ite", "count"],
            &["notifs", "pms"],
            &["notifs", "tickets"],
            &["notifs", "waiting_tickets"],
            &["notifs", "requests"],
            &["notifs", "topics"],
            &["notifs", "aboutToDropClient"],
        ];
        for path in counts {
            let answer = with(account(), path, too_many.clone());
            assert_eq!(user_data(&bytes(&answer)), Err(Malformed), "{path:?}");
        }
        for (name, class) in [
            ("Mouse", UserClass::Mouse),
            ("User", UserClass::User),
            ("Power User", UserClass::PowerUser),
            ("Elite VIP", UserClass::EliteVip),
        ] {
            let answer = with(account(), &["classname"], json!(name));
            assert_eq!(user_data(&bytes(&answer)).map(|data| data.class), Ok(class));
        }
        let partial = with(account(), &["snatch_summary", "partial"], json!(1));
        assert_eq!(
            user_data(&bytes(&partial)).map(|data| data.summary.map(|summary| summary.partial)),
            Ok(Some(crate::core::Partials::Seen))
        );
    }

    /// A search result as MAM sends it.
    fn result(id: u64) -> Value {
        json!({
            "id": id.to_string(), "title": "Synthetic", "vip": "0", "free": 0, "fl_vip": 0,
            "personal_freeleech": 0, "my_snatched": 0, "vip_expire": 0, "bookmarked": null,
        })
    }

    #[test]
    fn search_results_are_read_whole_or_not_at_all() -> TestResult {
        let vip = with(
            with(result(2), &["vip"], json!(1)),
            &["vip_expire"],
            json!(1_796_811_253),
        );
        let forever = with(with(result(3), &["vip"], json!(1)), &["free"], json!("1"));
        let snatched = with(
            with(result(4), &["my_snatched"], json!(1)),
            &["bookmarked"],
            json!(1_790_000_000),
        );
        let page = json!({"found": 4, "data": [result(1), vip, forever, snatched]});
        let page = bookmark_page(&bytes(&page)).map_err(|_| "not understood")?;
        let bookmarks = page.bookmarks;
        assert_eq!(
            bookmarks
                .iter()
                .map(|bookmark| bookmark.vip)
                .collect::<Vec<_>>(),
            [
                VipTorrent::No,
                VipTorrent::Until(SystemTime::UNIX_EPOCH + Duration::from_secs(1_796_811_253)),
                VipTorrent::Indefinitely,
                VipTorrent::No,
            ]
        );
        assert_eq!(
            bookmarks.get(2).map(|bookmark| bookmark.freeleech),
            Some(Freeleech::ForEveryone)
        );
        assert_eq!(
            bookmarks.get(3).map(|bookmark| bookmark.snatched),
            Some(Snatched::Before)
        );
        assert_eq!(
            bookmarks.get(3).and_then(|bookmark| bookmark.bookmarked_at),
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000))
        );
        let none = bookmark_page(&bytes(&json!({"found": 0})));
        assert_eq!(none.map(|page| page.bookmarks.len()), Ok(0));
        assert_eq!(
            bookmark_page(&bytes(&json!({"found": 3}))),
            Err(Malformed),
            "results counted but not sent"
        );
        assert_eq!(
            bookmark_page(&bytes(&json!({"found": 5_000_000_000_u64}))),
            Err(Malformed)
        );
        let endless = with(
            with(result(1), &["vip"], json!(1)),
            &["vip_expire"],
            json!(u64::MAX),
        );
        assert_eq!(
            bookmark_page(&bytes(&json!({"found": 1, "data": [endless]}))),
            Err(Malformed),
            "a VIP end beyond any time"
        );
        let wrong: &[(&str, Value)] = &[
            ("id", json!(0)),
            ("bookmarked", json!(u64::MAX)),
            ("title", json!("  ")),
            ("free", json!(2)),
            ("vip_expire", json!(-1)),
            ("bookmarked", json!("yesterday")),
        ];
        for (field, value) in wrong {
            let page = json!({"found": 1, "data": [with(result(1), &[field], value.clone())]});
            assert_eq!(bookmark_page(&bytes(&page)), Err(Malformed), "{field}");
        }
        for field in ["title", "my_snatched", "personal_freeleech"] {
            let page = json!({"found": 1, "data": [without(result(1), &[field])]});
            assert_eq!(bookmark_page(&bytes(&page)), Err(Malformed), "{field}");
        }
        Ok(())
    }

    #[test]
    fn bonus_history_keeps_wedges_spent_on_torrents() {
        let history = json!([
            {"timestamp": 1_790_000_000.5, "amount": -1, "type": "wedgePF", "tid": 330_896},
            {"timestamp": 1_641_914_230.1, "amount": 2000, "type": "millionaires", "tid": null},
            {"timestamp": 1_610_486_584.1, "amount": -10, "type": "wedgeGFL", "tid": 220_870},
        ]);
        let spent = wedge_history(&bytes(&history)).unwrap_or_default();
        assert_eq!(
            spent
                .iter()
                .map(|entry| entry.torrent.get())
                .collect::<Vec<_>>(),
            [330_896]
        );
        // A wedge entry Windlass cannot read could hide a wedge already spent.
        for entry in [
            json!({"timestamp": "not a time", "type": "wedgePF", "tid": 5}),
            json!({"timestamp": 1.0, "type": "wedgePF", "tid": 0}),
            json!({"timestamp": 1.0, "type": "wedgePF", "tid": null}),
            json!({"timestamp": 1.0e30, "type": "wedgePF", "tid": 6}),
            json!({"timestamp": 1.0e19, "type": "wedgePF", "tid": 6}),
        ] {
            assert_eq!(
                wedge_history(&bytes(&json!([entry]))),
                Err(Malformed),
                "{entry}"
            );
        }
        assert_eq!(
            wedge_history(&bytes(&json!({"error": "no"}))),
            Err(Malformed)
        );
    }

    #[test]
    fn small_answers_are_read_strictly() {
        let lookup = json!({"ip": "192.0.2.10", "ASN": "209103", "AS": ""});
        assert_eq!(
            ip_observation(&bytes(&lookup)).map(|seen| seen.provider),
            Ok(None),
            "an empty network name is no name"
        );
        assert_eq!(ip_observation(&bytes(&json!({"ASN": 1}))), Err(Malformed));
        let far = json!({"ip": "192.0.2.10", "ASN": 5_000_000_000_u64});
        assert_eq!(ip_observation(&bytes(&far)), Err(Malformed));
        assert_eq!(
            purchase(&bytes(&json!({"success": false, "error": " "}))),
            Err(MamFailure::Refused { message: None })
        );
        for answer in [
            json!({"success": true}),
            json!({"success": true, "seedbonus": -1}),
            json!({"success": "yes"}),
        ] {
            assert_eq!(
                purchase(&bytes(&answer)),
                Err(MamFailure::Malformed),
                "{answer}"
            );
        }
        let completed = json!({"msg": "Completed", "ip": "192.0.2.10"});
        assert!(matches!(
            registration(StatusCode::OK, &bytes(&completed)),
            Ok(MamReply::Registered {
                registration: Registration::Completed,
                ..
            })
        ));
        assert_eq!(
            registration(StatusCode::FORBIDDEN, b"<html>refused</html>"),
            Err(MamFailure::Session(SessionRejection::Other))
        );
        assert_eq!(
            registration(StatusCode::OK, &bytes(&json!({"msg": "Completed"}))),
            Err(MamFailure::Malformed),
            "no IP"
        );
    }

    #[test]
    fn client_entries_are_searched_for_what_windlass_needs() {
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
            {"peer_id": "-qB5"},
        ]);
        let client = |qbittorrent, connectable, refusal| ListedClient {
            qbittorrent,
            connectable,
            refusal,
        };
        assert_eq!(
            clients(&list),
            Ok(vec![
                client(
                    version("5.1.2"),
                    Some(Connectable::Yes),
                    ClientRefusal::NotRefused
                ),
                client(
                    version("5.0.4"),
                    Some(Connectable::No),
                    ClientRefusal::Refused
                ),
                client(
                    version("5.2.0"),
                    Some(Connectable::No),
                    ClientRefusal::NotRefused
                ),
                client(None, Some(Connectable::Yes), ClientRefusal::Refused),
                client(None, None, ClientRefusal::Refused),
                client(version("5.1.1"), None, ClientRefusal::NotRefused),
                client(None, None, ClientRefusal::NotRefused),
                client(
                    version("5.1.3"),
                    Some(Connectable::Yes),
                    ClientRefusal::NotRefused
                ),
                client(None, None, ClientRefusal::NotRefused),
            ])
        );
        let keyed = json!({"1": {"client": "qBittorrent 5.1.2", "connectable": null}});
        assert_eq!(clients(&keyed).map(|clients| clients.len()), Ok(1));
        assert_eq!(clients(&json!("none")), Err(Malformed));
    }
}
