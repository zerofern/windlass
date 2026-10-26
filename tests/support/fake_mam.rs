//! A stand-in for MAM that answers as the owner observed MAM to answer
//! (B-0011's observed responses), with synthetic data. Tests steer it through
//! [`World`] and inspect what Windlass asked through the journal.
#![allow(
    dead_code,
    reason = "each test crate that includes the fake uses a different part of it"
)]
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the fake rounds synthetic balances and display sizes"
)]
#![allow(
    clippy::struct_excessive_bools,
    reason = "the fake mirrors MAM's flags one for one"
)]

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{OriginalUri, RawQuery, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

const GIB: u64 = 1 << 30;
const DAY: Duration = Duration::from_hours(24);
const WEEK: Duration = Duration::from_hours(7 * 24);
const UPLOAD_PRICE_PER_GIB: f64 = 500.0;
const VIP_PRICE_PER_WEEK: f64 = 1250.0;
const SIGNED_OUT: &str = "Error, you are not signed in <br />";

/// Which of MAM's hosts a request reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Host {
    /// `www.myanonamouse.net`.
    Site,
    /// `t.myanonamouse.net`.
    Tracker,
}

/// One request as the fake received it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Request {
    pub(super) host: Host,
    pub(super) method: String,
    pub(super) path: String,
    pub(super) query: String,
    pub(super) body: String,
    pub(super) user_agent: Option<String>,
    pub(super) cookie: Option<String>,
}

/// The account, as MAM would report it.
#[derive(Debug, Clone)]
pub(super) struct Account {
    pub(super) uploaded: u64,
    pub(super) downloaded: u64,
    pub(super) points: f64,
    pub(super) wedges: u32,
    pub(super) class: String,
    pub(super) vip_until: Option<SystemTime>,
    pub(super) unsatisfied: u32,
    pub(super) unsatisfied_limit: u32,
    pub(super) not_seeding_unsatisfied: u32,
    pub(super) partial: bool,
    pub(super) duplicates: u32,
    pub(super) tracker_errors: u32,
    pub(super) connectable: String,
    pub(super) clients: Vec<Value>,
    pub(super) about_to_drop_client: u32,
    pub(super) messages: u32,
    pub(super) wedge_history: Vec<(SystemTime, u64)>,
}

impl Default for Account {
    fn default() -> Self {
        Self {
            uploaded: 80 * GIB,
            downloaded: 0,
            points: 60_000.5,
            wedges: 200,
            class: String::from("VIP"),
            vip_until: SystemTime::now().checked_add(60 * DAY),
            unsatisfied: 0,
            unsatisfied_limit: 150,
            not_seeding_unsatisfied: 0,
            partial: false,
            duplicates: 0,
            tracker_errors: 0,
            connectable: String::from("offline"),
            clients: Vec::new(),
            about_to_drop_client: 0,
            messages: 0,
            wedge_history: Vec::new(),
        }
    }
}

/// A client list entry. MAM's shape is not yet captured (B-0011 call 3), so
/// this is a plausible guess that Windlass's reader must tolerate, not a
/// recorded fixture.
pub(super) fn client(version: &str, connectable: &str) -> Value {
    json!({
        "client": format!("qBittorrent {version}"),
        "connectable": connectable,
        "torrents": 1,
    })
}

/// A synthetic torrent in the catalogue.
#[derive(Debug, Clone)]
pub(super) struct Torrent {
    pub(super) title: String,
    pub(super) size: u64,
    pub(super) vip: bool,
    pub(super) free: bool,
    pub(super) bookmarked: Option<SystemTime>,
    pub(super) snatched: bool,
    pub(super) vip_expire: Option<SystemTime>,
    /// Whether a wedge made it a personal freeleech.
    pub(super) wedged: bool,
    /// What search reports for the wedge; it lags until the test ends the lag.
    pub(super) wedged_visible: bool,
}

impl Torrent {
    /// A bookmarked, ordinary torrent of this size.
    pub(super) fn bookmarked(title: &str, size: u64) -> Self {
        Self {
            title: title.to_owned(),
            size,
            vip: false,
            free: false,
            bookmarked: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000)),
            snatched: false,
            vip_expire: None,
            wedged: false,
            wedged_visible: false,
        }
    }

    /// The torrent file MAM serves for it.
    pub(super) fn file(&self) -> Vec<u8> {
        let mut file = b"d8:announce38:https://t.fake/announce/passkey-secret4:info".to_vec();
        file.extend_from_slice(&self.info());
        file.push(b'e');
        file
    }

    /// Its info hash in lowercase hexadecimal.
    pub(super) fn hash(&self) -> String {
        Sha1::digest(self.info())
            .iter()
            .fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            })
    }

    fn info(&self) -> Vec<u8> {
        format!(
            "d6:lengthi{}e4:name{}:{}12:piece lengthi262144e6:pieces0:e",
            self.size,
            self.title.len(),
            self.title
        )
        .into_bytes()
    }
}

/// An API session and the cookie values that unlock it.
#[derive(Debug, Clone)]
pub(super) struct Session {
    pub(super) cookies: Vec<String>,
    pub(super) asn: u32,
    pub(super) may_register: bool,
}

/// How a route misbehaves, for a number of requests.
#[derive(Debug, Clone)]
pub(super) enum Fault {
    Status(u16),
    Malformed,
    TooSoon,
    Delay(Duration),
    /// Answer with this status and body instead.
    Body(u16, String),
}

/// Everything the fake knows. Tests change it between requests.
#[derive(Debug)]
pub(super) struct World {
    pub(super) account: Account,
    pub(super) torrents: BTreeMap<u64, Torrent>,
    pub(super) sessions: Vec<Session>,
    pub(super) ip: IpAddr,
    pub(super) asn: u32,
    /// Time for rate limits; the real clock when absent.
    pub(super) clock: Option<SystemTime>,
    pub(super) faults: Vec<(String, Fault, u32)>,
    pub(super) journal: Vec<Request>,
    pub(super) violations: Vec<String>,
    pub(super) seedbox: Option<IpAddr>,
    last_registration: Option<SystemTime>,
    last_ip_lookup: Option<SystemTime>,
    issued: u64,
}

impl World {
    fn new() -> Self {
        Self {
            account: Account::default(),
            torrents: BTreeMap::new(),
            sessions: vec![Session {
                cookies: vec![String::from("fake-session")],
                asn: 209_103,
                may_register: true,
            }],
            ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)),
            asn: 209_103,
            clock: None,
            faults: Vec::new(),
            journal: Vec::new(),
            violations: Vec::new(),
            seedbox: None,
            last_registration: None,
            last_ip_lookup: None,
            issued: 0,
        }
    }

    fn now(&self) -> SystemTime {
        self.clock.unwrap_or_else(SystemTime::now)
    }

    /// Let search report every wedge already spent.
    pub(super) fn end_flag_lag(&mut self) {
        for torrent in self.torrents.values_mut() {
            torrent.wedged_visible = torrent.wedged;
        }
    }

    /// Make a route misbehave for this many requests.
    pub(super) fn fail(&mut self, path: &str, fault: Fault, times: u32) {
        self.faults.push((path.to_owned(), fault, times));
    }

    /// Forget every cookie value issued so far except the newest one.
    pub(super) fn expire_old_cookies(&mut self) {
        for session in &mut self.sessions {
            let newest = session.cookies.last().cloned();
            session.cookies = newest.into_iter().collect();
        }
    }

    /// The newest cookie value of the first session.
    pub(super) fn newest_cookie(&self) -> Option<String> {
        self.sessions.first()?.cookies.last().cloned()
    }

    fn fault(&mut self, path: &str) -> Option<Fault> {
        let index = self
            .faults
            .iter()
            .position(|(route, _, times)| route == path && *times > 0)?;
        let (_, fault, times) = self.faults.get_mut(index)?;
        *times -= 1;
        Some(fault.clone())
    }

    /// The session this cookie unlocks from the current ASN, or why not.
    fn session(&self, cookie: Option<&str>) -> Result<usize, &'static str> {
        let cookie = cookie.ok_or("Invalid/missing cookie")?;
        let index = self
            .sessions
            .iter()
            .position(|session| session.cookies.iter().any(|value| value == cookie))
            .ok_or("Invalid/missing cookie")?;
        let session = self.sessions.get(index).ok_or("Invalid/missing cookie")?;
        if session.asn != self.asn {
            return Err("Session ASN mismatch");
        }
        Ok(index)
    }

    /// Issue a new cookie value for this session, as MAM does on every answer.
    fn rotate(&mut self, index: usize) -> Option<String> {
        self.issued += 1;
        let value = format!("fake-rotated-{}", self.issued);
        self.sessions.get_mut(index)?.cookies.push(value.clone());
        Some(value)
    }

    fn vip(&self) -> bool {
        self.account
            .vip_until
            .is_some_and(|until| until > self.now())
    }
}

/// A running fake with its two hosts.
#[derive(Debug, Clone)]
pub(super) struct FakeMam {
    world: Arc<Mutex<World>>,
    pub(super) site: reqwest::Url,
    pub(super) tracker: reqwest::Url,
}

type Shared = (Arc<Mutex<World>>, Host);

impl FakeMam {
    /// Serve the site and tracker hosts on two loopback ports.
    pub(super) async fn start() -> std::io::Result<Self> {
        let world = Arc::new(Mutex::new(World::new()));
        let site = serve((Arc::clone(&world), Host::Site)).await?;
        let tracker = serve((Arc::clone(&world), Host::Tracker)).await?;
        Ok(Self {
            world,
            site,
            tracker,
        })
    }

    /// Read or change the world.
    pub(super) fn with<R>(&self, change: impl FnOnce(&mut World) -> R) -> Option<R> {
        self.world.lock().ok().map(|mut world| change(&mut world))
    }

    /// Every request received so far.
    pub(super) fn journal(&self) -> Vec<Request> {
        self.with(|world| world.journal.clone()).unwrap_or_default()
    }

    /// Every broken rule seen so far.
    pub(super) fn violations(&self) -> Vec<String> {
        self.with(|world| world.violations.clone())
            .unwrap_or_default()
    }
}

async fn serve(shared: Shared) -> std::io::Result<reqwest::Url> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let routes = Router::new().fallback(route).with_state(shared);
    tokio::spawn(async move { axum::serve(listener, routes).await });
    reqwest::Url::parse(&format!("http://{address}/")).map_err(std::io::Error::other)
}

async fn route(
    State((world, host)): State<Shared>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let header = |name: header::HeaderName| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let request = Request {
        host,
        method: method.to_string(),
        path: uri.path().to_owned(),
        query: query.unwrap_or_default(),
        body: String::from_utf8_lossy(&body).into_owned(),
        user_agent: header(header::USER_AGENT),
        cookie: header(header::COOKIE).and_then(|cookie| {
            cookie
                .split(';')
                .find_map(|part| part.trim().strip_prefix("mam_id="))
                .map(str::to_owned)
        }),
    };
    let fault = match world.lock() {
        Ok(mut world) => {
            world.journal.push(request.clone());
            world.fault(&request.path)
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    match fault {
        Some(Fault::Status(status)) => {
            return StatusCode::from_u16(status)
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
                .into_response();
        }
        Some(Fault::Malformed) => return "{\"not\": json".into_response(),
        Some(Fault::TooSoon) => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, "120")],
                "{\"Success\":false,\"msg\":\"Too many requests\"}",
            )
                .into_response();
        }
        Some(Fault::Body(status, body)) => {
            return (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                body,
            )
                .into_response();
        }
        Some(Fault::Delay(delay)) => tokio::time::sleep(delay).await,
        None => {}
    }
    world.lock().map_or_else(
        |_| StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        |mut world| answer(&mut world, &request),
    )
}

fn answer(world: &mut World, request: &Request) -> Response {
    let query = Query::parse(&request.query);
    match (request.host, request.method.as_str(), request.path.as_str()) {
        (_, "GET", "/json/jsonIp.php") => ip_lookup(world),
        (Host::Tracker, "GET", "/json/dynamicSeedbox.php") => register(world, request),
        (Host::Site, "GET", "/jsonLoad.php") => authenticated(world, request, |world| {
            json_response(&user_data(world, &query))
        }),
        (Host::Site, "GET" | "POST", "/tor/js/loadSearchJSONbasic.php") => {
            authenticated(world, request, |world| {
                json_response(&search(world, request, &query))
            })
        }
        (Host::Site, "GET", "/tor/download.php") => {
            authenticated(world, request, |world| download(world, &query))
        }
        (Host::Site, "GET", "/json/bonusBuy.php") => {
            authenticated(world, request, |world| json_response(&buy(world, &query)))
        }
        (Host::Site, "GET", "/json/userBonusHistory.php") => {
            authenticated(world, request, |world| {
                let now = world.now();
                let entries: Vec<Value> = world
                    .account
                    .wedge_history
                    .iter()
                    .rev()
                    .map(|(at, tid)| {
                        json!({ "timestamp": seconds(*at), "amount": -1, "type": "wedgePF",
                                "tid": tid, "title": "Synthetic", "other_userid": null,
                                "other_name": null, "now": seconds(now) })
                    })
                    .collect();
                json_response(&Value::Array(entries))
            })
        }
        _ => {
            world.violations.push(format!(
                "unsupported request: {} {}",
                request.method, request.path
            ));
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

/// Answer for a valid session, setting a new cookie value as MAM does.
fn authenticated(
    world: &mut World,
    request: &Request,
    answer: impl FnOnce(&mut World) -> Response,
) -> Response {
    match world.session(request.cookie.as_deref()) {
        Ok(index) => {
            let mut response = answer(world);
            if let Some(cookie) = world.rotate(index)
                && let Ok(value) = format!(
                    "mam_id={cookie}; expires=Sun, 08 Nov 2026 17:35:53 GMT; Max-Age=1296000; \
                     path=/; domain=.myanonamouse.net; secure; SameSite=Lax"
                )
                .parse()
            {
                response.headers_mut().append(header::SET_COOKIE, value);
            }
            response
        }
        Err(reason) => (StatusCode::FORBIDDEN, format!("{SIGNED_OUT}{reason}")).into_response(),
    }
}

fn ip_lookup(world: &mut World) -> Response {
    let now = world.now();
    if world
        .last_ip_lookup
        .is_some_and(|last| now.duration_since(last).unwrap_or_default() < Duration::from_mins(1))
    {
        world
            .violations
            .push(String::from("IP lookup more than once a minute"));
    }
    world.last_ip_lookup = Some(now);
    json_response(&json!({
        "ip": world.ip.to_string(),
        "ASN": world.asn,
        "AS": "ProtonVPN Proton AG",
        "time": seconds(now),
    }))
}

fn register(world: &mut World, request: &Request) -> Response {
    let now = world.now();
    let ip = world.ip.to_string();
    let asn = world.asn;
    let reply = |status: StatusCode, success: bool, message: &str| {
        (
            status,
            json_response(&json!({ "Success": success, "msg": message, "ip": ip,
                                   "ASN": asn, "AS": "ProtonVPN Proton AG" })),
        )
            .into_response()
    };
    let index = match world.session(request.cookie.as_deref()) {
        Ok(index) => index,
        Err(reason) => {
            let message = if reason.contains("ASN") {
                "Invalid session - ASN mismatch"
            } else {
                "Invalid session - Invalid Cookie"
            };
            return reply(StatusCode::FORBIDDEN, false, message);
        }
    };
    if !world
        .sessions
        .get(index)
        .is_some_and(|session| session.may_register)
    {
        return reply(
            StatusCode::FORBIDDEN,
            false,
            "Incorrect session type - not allowed this function",
        );
    }
    if world.last_registration.is_some_and(|last| {
        now.duration_since(last).unwrap_or_default() < 60 * 60 * Duration::from_secs(1)
    }) {
        world
            .violations
            .push(String::from("seedbox registration more than once an hour"));
        return reply(
            StatusCode::TOO_MANY_REQUESTS,
            false,
            "Last change too recent",
        );
    }
    world.last_registration = Some(now);
    if world.seedbox == Some(world.ip) {
        return reply(StatusCode::OK, true, "No change");
    }
    world.seedbox = Some(world.ip);
    reply(StatusCode::OK, true, "Completed")
}

/// Query parameters; a flag counts only with a value, as MAM honours them.
struct Query(Vec<(String, String)>);

impl Query {
    fn parse(query: &str) -> Self {
        Self(
            query
                .split('&')
                .filter(|pair| !pair.is_empty())
                .map(|pair| {
                    let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                    (decode(name), decode(value))
                })
                .collect(),
        )
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn flag(&self, name: &str) -> bool {
        self.get(name).is_some_and(|value| !value.is_empty())
    }
}

fn decode(text: &str) -> String {
    let bytes = text.replace('+', " ").into_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        let hex = bytes
            .get(index + 1..index + 3)
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        if let (b'%', Some(value)) = (byte, hex) {
            decoded.push(value);
            index += 3;
        } else {
            decoded.push(byte);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn user_data(world: &World, query: &Query) -> Value {
    let account = &world.account;
    let mut data = json!({
        "classname": account.class,
        "country_code": "xx",
        "country_name": "Nowhere",
        "downloaded": "0.00 KiB",
        "downloaded_bytes": account.downloaded,
        "ratio": if account.downloaded == 0 { String::from("\u{221e}") } else {
            format!("{:.2}", account.uploaded as f64 / account.downloaded as f64)
        },
        "seedbonus": account.points.floor() as u64,
        "uid": 1,
        "uploaded": "80.00 GiB",
        "uploaded_bytes": account.uploaded,
        "username": "SyntheticUser",
        "wedges": account.wedges,
    });
    let mut set = |name: &str, value: Value| {
        if let Some(data) = data.as_object_mut() {
            data.insert(name.to_owned(), value);
        }
    };
    if let Some(until) = account.vip_until {
        set("vip_until", Value::from(mam_time(until)));
    }
    if query.flag("snatch_summary") {
        let category = |name: &str, count: u32, red: bool| json!({ "name": name, "count": count, "red": red, "size": null });
        set(
            "snatch_summary",
            json!({
                "unsat": { "name": "Unsatisfied", "count": account.unsatisfied,
                           "limit": account.unsatisfied_limit, "red": false, "size": null },
                "inactUnsat": category("Not Seeding - Not Yet Satisfied",
                                       account.not_seeding_unsatisfied, true),
                "inactHnr": category("Not Seeding - H&R - Not Yet Satisfied", 0, true),
                "seedUnsat": category("Seeding - Not Yet Satisfied", 0, false),
                "duplicates": { "name": "Duplicate peer entries", "count": account.duplicates, "red": true },
                "ite": { "name": "Important Tracker Errors", "count": account.tracker_errors, "latest": 0 },
                "partial": account.partial,
                "connectable": account.connectable,
                "v6_connectable": null,
                "created_at": mam_time(world.now()),
                "created": seconds(world.now()),
            }),
        );
    }
    if query.flag("notif") {
        set(
            "notifs",
            json!({
                "requests": 0, "topics": 0, "aboutToDropClient": account.about_to_drop_client,
                "waiting_tickets": 0, "pms": account.messages, "tickets": 0,
            }),
        );
    }
    if query.flag("clientStats") {
        set("clientStats", Value::Array(account.clients.clone()));
    }
    data
}

fn search(world: &World, request: &Request, query: &Query) -> Value {
    let body: Value = serde_json::from_str(&request.body).unwrap_or(Value::Null);
    let tor = body.get("tor").cloned().unwrap_or(Value::Null);
    let id = tor
        .get("id")
        .and_then(Value::as_u64)
        .or_else(|| query.get("tor[id]").and_then(|id| id.parse().ok()));
    // MAM reads the bookmark filter only from a JSON POST.
    let bookmarks = request.method == "POST"
        && tor.get("searchIn").and_then(Value::as_str) == Some("bookmarks");
    let start = tor
        .get("startNumber")
        .and_then(|start| start.as_u64().or_else(|| start.as_str()?.parse().ok()))
        .unwrap_or_default();
    let perpage = body.get("perpage").and_then(Value::as_u64).unwrap_or(20);
    let matching: Vec<(&u64, &Torrent)> = world
        .torrents
        .iter()
        .rev()
        .filter(|(tid, torrent)| {
            id.is_none_or(|id| **tid == id) && (!bookmarks || torrent.bookmarked.is_some())
        })
        .collect();
    let vip = world.vip();
    let data: Vec<Value> = matching
        .iter()
        .skip(usize::try_from(start).unwrap_or(usize::MAX))
        .take(usize::try_from(perpage).unwrap_or(usize::MAX))
        .map(|(tid, torrent)| {
            json!({
                "id": tid,
                "title": torrent.title,
                "size": format!("{:.1} MiB", torrent.size as f64 / 1_048_576.0),
                "vip": u8::from(torrent.vip),
                "free": u8::from(torrent.free),
                "personal_freeleech": u8::from(torrent.wedged_visible),
                // Free for this user, for any reason, as observed.
                "fl_vip": u8::from(torrent.wedged_visible || torrent.free || (torrent.vip && vip)),
                "my_snatched": u8::from(torrent.snatched),
                "bookmarked": torrent.bookmarked.map(seconds),
                "vip_expire": torrent.vip_expire.map_or(0, seconds),
                "author_info": "{\"1\":\"Synthetic Author\"}",
            })
        })
        .collect();
    json!({ "perpage": perpage, "start": start, "data": data,
            "total": data.len(), "found": matching.len() })
}

fn download(world: &mut World, query: &Query) -> Response {
    let Some(tid) = query.get("tid").and_then(|tid| tid.parse::<u64>().ok()) else {
        return empty_html();
    };
    let now = world.now();
    let wedge = query.flag("fl");
    let wedges = world.account.wedges;
    let Some(torrent) = world.torrents.get_mut(&tid) else {
        return empty_html();
    };
    if wedge && !torrent.wedged {
        if wedges == 0 {
            return empty_html();
        }
        torrent.wedged = true;
        world.account.wedges -= 1;
        world.account.wedge_history.push((now, tid));
    }
    let Some(torrent) = world.torrents.get(&tid) else {
        return empty_html();
    };
    (
        [
            (
                header::CONTENT_TYPE,
                String::from("application/x-bittorrent"),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("inline; filename=\"[{tid}]_{}.torrent\"", torrent.title),
            ),
        ],
        Body::from(torrent.file()),
    )
        .into_response()
}

fn buy(world: &mut World, query: &Query) -> Value {
    let refuse = |message: &str| json!({ "success": false, "error": message });
    let now = world.now();
    match query.get("spendtype") {
        Some("upload") => {
            let Some(amount) = query
                .get("amount")
                .and_then(|amount| amount.parse::<u64>().ok())
            else {
                return refuse("Invalid amount specified");
            };
            if amount < 50 {
                return refuse("Invalid amount specified");
            }
            let price = amount as f64 * UPLOAD_PRICE_PER_GIB;
            if price > world.account.points {
                return refuse("Not enough points");
            }
            world.account.points -= price;
            world.account.uploaded += amount * GIB;
            json!({ "success": true, "type": "upload", "amount": amount,
                    "seedbonus": world.account.points, "uploaded": world.account.uploaded,
                    "downloaded": world.account.downloaded, "ratio": "inf." })
        }
        Some("VIP") if query.get("duration") == Some("max") => {
            let target = now + 90 * DAY;
            let current = world
                .account
                .vip_until
                .filter(|until| *until > now)
                .unwrap_or(now);
            let weeks = (target
                .duration_since(current)
                .unwrap_or_default()
                .as_secs_f64()
                / WEEK.as_secs_f64())
            .max(1.0);
            let price = (weeks * VIP_PRICE_PER_WEEK).round();
            if price > world.account.points {
                return refuse("Not enough points");
            }
            world.account.points -= price;
            world.account.vip_until = Some(current.max(now) + WEEK.mul_f64(weeks));
            world.account.class = String::from("VIP");
            json!({ "success": true, "type": "VIP", "amount": (weeks * 100.0).round() / 100.0,
                    "seedbonus": world.account.points })
        }
        Some(kind @ ("gift" | "sendWedge")) => {
            world
                .violations
                .push(format!("automated {kind}, which MAM forbids"));
            refuse("Not allowed")
        }
        _ => refuse("Invalid spend type"),
    }
}

fn empty_html() -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=UTF-8")], "").into_response()
}

fn json_response(value: &Value) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        value.to_string(),
    )
        .into_response()
}

fn seconds(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// MAM's `YYYY-MM-DD HH:MM:SS` in UTC.
fn mam_time(time: SystemTime) -> String {
    let format = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    time::OffsetDateTime::from(time)
        .format(format)
        .unwrap_or_default()
}
