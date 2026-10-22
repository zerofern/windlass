//! Fake MAM server (`TESTKIT_MODE=mam`).
//!
//! Implements the `MyAnonaMouse` endpoints Windlass calls, with response
//! shapes pinned to `docs/mam-api.md`.  Tests drive responses through a
//! `/control/...` plane and read incoming requests from a journal.
//!
//! See `docs/operator-readiness.md` §34 for the planning lock.

use axum::{
    Json, Router,
    extract::{OriginalUri, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// ── Fake-MAM state ────────────────────────────────────────────────────────────

/// In-memory state: overridable per-endpoint responses and a request journal.
///
/// Defaults match `docs/mam-api.md` happy-path output.  Tests overwrite
/// individual slots via the control plane.
#[derive(Default)]
pub struct MamState {
    seedbox: RwLock<SeedboxResponse>,
    json_load: RwLock<JsonLoadResponse>,
    json_ip: RwLock<JsonIpResponse>,
    check_cookie: RwLock<CheckCookieResponse>,
    journal: RwLock<Vec<JournalEntry>>,
}

#[derive(Clone, Serialize)]
struct SeedboxResponse {
    /// HTTP status code returned.  Some `msg` values (`Last change too recent`
    /// → 429; the `Invalid session` family → 403) ride a non-200 status per
    /// `docs/mam-api.md`.
    status: u16,
    #[serde(rename = "Success")]
    success: bool,
    msg: String,
    ip: String,
    #[serde(rename = "ASN")]
    asn: u32,
    #[serde(rename = "AS")]
    as_org: String,
}

impl Default for SeedboxResponse {
    fn default() -> Self {
        Self {
            status: 200,
            success: true,
            msg: "Completed".to_owned(),
            ip: "10.8.0.1".to_owned(),
            asn: 212_238,
            as_org: "Datacamp Limited".to_owned(),
        }
    }
}

#[derive(Clone, Serialize)]
struct JsonLoadResponse {
    status: u16,
    /// `"yes"` / `"no"`.  Absent ⇒ Windlass treats as `false`
    /// (`docs/mam-api.md` notes this is the §28 bug `?clientStats=` fixes).
    connectable: Option<String>,
    /// MAM exposes this as a JSON number; `windlass-clients` parses with
    /// `#[serde(default)]` so absent ⇒ 0.0 (fail-closed for §26).
    ratio: f64,
    seedbonus: f64,
    username: String,
    /// Present only with `?snatch_summary` per `docs/mam-api.md`.  When
    /// present the fake includes it; tests that need to drive §25-style
    /// quota work overwrite this.
    unsat: Option<UnsatSummary>,
}

impl Default for JsonLoadResponse {
    fn default() -> Self {
        Self {
            status: 200,
            connectable: Some("yes".to_owned()),
            ratio: 2.5,
            seedbonus: 24_425.0,
            username: "BrightVoyage".to_owned(),
            unsat: None,
        }
    }
}

#[derive(Clone, Serialize)]
struct UnsatSummary {
    count: u64,
    limit: u64,
}

#[derive(Clone, Serialize)]
struct JsonIpResponse {
    status: u16,
    ip: String,
    #[serde(rename = "ASN")]
    asn: u32,
    #[serde(rename = "AS")]
    as_org: String,
    time: i64,
}

impl Default for JsonIpResponse {
    fn default() -> Self {
        Self {
            status: 200,
            ip: "10.8.0.1".to_owned(),
            asn: 212_238,
            as_org: "Datacamp Limited".to_owned(),
            time: 1_776_193_859,
        }
    }
}

#[derive(Clone, Serialize)]
struct CheckCookieResponse {
    status: u16,
}

impl Default for CheckCookieResponse {
    fn default() -> Self {
        Self { status: 200 }
    }
}

#[derive(Clone, Serialize)]
pub struct JournalEntry {
    pub method: String,
    pub path: String,
    pub query: String,
    pub body: String,
    pub cookie: Option<String>,
}

// ── Router ────────────────────────────────────────────────────────────────────

/// Build the axum router.  Exposed so the in-process drift test and the
/// `TESTKIT_MODE=mam` binary can mount it on different listeners.
pub fn router(state: Arc<MamState>) -> Router {
    Router::new()
        // MAM endpoints Windlass calls today.
        .route(
            "/json/dynamicSeedbox.php",
            get(get_dynamic_seedbox).post(get_dynamic_seedbox),
        )
        .route("/jsonLoad.php", get(get_json_load))
        .route("/json/checkCookie.php", get(get_check_cookie))
        .route("/json/jsonIp.php", get(get_json_ip))
        // MAM endpoints we don't call yet (librarian).  Stubs return a
        // minimal shape so a future client can decode + a journal entry
        // lands, but tests don't drive these yet.
        .route("/json/bonusBuy.php/{*ts}", post(stub_ok))
        .route(
            "/tor/js/loadSearchJSONbasic.php",
            get(stub_ok).post(stub_ok),
        )
        .route("/tor/download.php/{hash}", get(stub_torrent_bytes))
        .route("/json/loadUserDetailsTorrents.php", get(stub_ok))
        // Control plane.
        .route("/control/seedbox", post(set_seedbox))
        .route("/control/json_load", post(set_json_load))
        .route("/control/json_ip", post(set_json_ip))
        .route("/control/check_cookie", post(set_check_cookie))
        .route("/control/journal", get(get_journal))
        .route("/control/reset", post(reset))
        .with_state(state)
}

// ── Binary entrypoint ─────────────────────────────────────────────────────────

/// Run the fake-MAM binary mode.
///
/// # Errors
/// Returns an error if the TCP listener cannot bind or `axum::serve`
/// fails.
pub async fn run() -> anyhow::Result<()> {
    let state = Arc::new(MamState::default());
    let app = router(state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("Fake MAM listening on :8080");
    axum::serve(listener, app).await?;
    Ok(())
}

// ── Journal helpers ───────────────────────────────────────────────────────────

async fn record(
    state: &MamState,
    method: &str,
    path: &str,
    query: &str,
    body: &str,
    headers: &HeaderMap,
) {
    let cookie = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    state.journal.write().await.push(JournalEntry {
        method: method.to_owned(),
        path: path.to_owned(),
        query: query.to_owned(),
        body: body.to_owned(),
        cookie,
    });
}

// ── Endpoint handlers ─────────────────────────────────────────────────────────

async fn get_dynamic_seedbox(
    State(state): State<Arc<MamState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> impl IntoResponse {
    record(
        &state,
        "GET",
        "/json/dynamicSeedbox.php",
        uri.query().unwrap_or(""),
        "",
        &headers,
    )
    .await;
    let resp = state.seedbox.read().await.clone();
    let body = json!({
        "Success": resp.success,
        "msg": resp.msg,
        "ip": resp.ip,
        "ASN": resp.asn,
        "AS": resp.as_org,
    });
    (status_from_u16(resp.status), Json(body))
}

async fn get_json_load(
    State(state): State<Arc<MamState>>,
    OriginalUri(uri): OriginalUri,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    record(
        &state,
        "GET",
        "/jsonLoad.php",
        uri.query().unwrap_or(""),
        "",
        &headers,
    )
    .await;
    let resp = state.json_load.read().await.clone();
    let mut body = serde_json::Map::new();
    body.insert("username".into(), json!(resp.username));
    body.insert("ratio".into(), json!(resp.ratio));
    body.insert("seedbonus".into(), json!(resp.seedbonus));
    // `connectable` only appears when `?clientStats` is present, per
    // `docs/mam-api.md`.  This mirrors real MAM and lets tests verify the
    // §28 fix that switched Windlass to `?clientStats=`.
    if params.contains_key("clientStats")
        && let Some(c) = resp.connectable
    {
        body.insert("connectable".into(), json!(c));
    }
    if params.contains_key("snatch_summary")
        && let Some(u) = resp.unsat
    {
        body.insert(
            "unsat".into(),
            json!({ "count": u.count, "limit": u.limit }),
        );
    }
    (status_from_u16(resp.status), Json(Value::Object(body)))
}

async fn get_check_cookie(
    State(state): State<Arc<MamState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    record(&state, "GET", "/json/checkCookie.php", "", "", &headers).await;
    let resp = state.check_cookie.read().await.clone();
    status_from_u16(resp.status)
}

async fn get_json_ip(State(state): State<Arc<MamState>>, headers: HeaderMap) -> impl IntoResponse {
    record(&state, "GET", "/json/jsonIp.php", "", "", &headers).await;
    let resp = state.json_ip.read().await.clone();
    let body = json!({
        "ip": resp.ip,
        "ASN": resp.asn,
        "AS": resp.as_org,
        "time": resp.time,
    });
    (status_from_u16(resp.status), Json(body))
}

// ── Deferred-endpoint stubs (librarian) ──────────────────────────────────────

async fn stub_ok() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "stub": true })))
}

async fn stub_torrent_bytes() -> impl IntoResponse {
    // A real MAM torrent fetch returns bencoded bytes.  Tests that
    // actually drive autograb will overwrite this in a later story.
    (StatusCode::OK, [0u8; 0].to_vec())
}

// ── Control-plane handlers ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct SetSeedbox {
    status: Option<u16>,
    success: Option<bool>,
    msg: Option<String>,
    ip: Option<String>,
    asn: Option<u32>,
    as_org: Option<String>,
}

async fn set_seedbox(
    State(state): State<Arc<MamState>>,
    Json(body): Json<SetSeedbox>,
) -> StatusCode {
    let mut slot = state.seedbox.write().await;
    if let Some(v) = body.status {
        slot.status = v;
    }
    if let Some(v) = body.success {
        slot.success = v;
    }
    if let Some(v) = body.msg {
        slot.msg = v;
    }
    if let Some(v) = body.ip {
        slot.ip = v;
    }
    if let Some(v) = body.asn {
        slot.asn = v;
    }
    if let Some(v) = body.as_org {
        slot.as_org = v;
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct SetJsonLoad {
    status: Option<u16>,
    /// `Some(s)` overwrites; omit to leave alone.  To clear a field call
    /// `/control/reset` (it restores all defaults).
    connectable: Option<String>,
    ratio: Option<f64>,
    seedbonus: Option<f64>,
    username: Option<String>,
    unsat: Option<UnsatPatch>,
}

#[derive(Deserialize)]
struct UnsatPatch {
    count: u64,
    limit: u64,
}

async fn set_json_load(
    State(state): State<Arc<MamState>>,
    Json(body): Json<SetJsonLoad>,
) -> StatusCode {
    let mut slot = state.json_load.write().await;
    if let Some(v) = body.status {
        slot.status = v;
    }
    if let Some(v) = body.connectable {
        slot.connectable = Some(v);
    }
    if let Some(v) = body.ratio {
        slot.ratio = v;
    }
    if let Some(v) = body.seedbonus {
        slot.seedbonus = v;
    }
    if let Some(v) = body.username {
        slot.username = v;
    }
    if let Some(v) = body.unsat {
        slot.unsat = Some(UnsatSummary {
            count: v.count,
            limit: v.limit,
        });
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct SetJsonIp {
    status: Option<u16>,
    ip: Option<String>,
    asn: Option<u32>,
    as_org: Option<String>,
    time: Option<i64>,
}

async fn set_json_ip(
    State(state): State<Arc<MamState>>,
    Json(body): Json<SetJsonIp>,
) -> StatusCode {
    let mut slot = state.json_ip.write().await;
    if let Some(v) = body.status {
        slot.status = v;
    }
    if let Some(v) = body.ip {
        slot.ip = v;
    }
    if let Some(v) = body.asn {
        slot.asn = v;
    }
    if let Some(v) = body.as_org {
        slot.as_org = v;
    }
    if let Some(v) = body.time {
        slot.time = v;
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct SetCheckCookie {
    status: u16,
}

async fn set_check_cookie(
    State(state): State<Arc<MamState>>,
    Json(body): Json<SetCheckCookie>,
) -> StatusCode {
    state.check_cookie.write().await.status = body.status;
    StatusCode::OK
}

async fn get_journal(State(state): State<Arc<MamState>>) -> impl IntoResponse {
    let journal = state.journal.read().await.clone();
    Json(journal)
}

async fn reset(State(state): State<Arc<MamState>>) -> StatusCode {
    *state.seedbox.write().await = SeedboxResponse::default();
    *state.json_load.write().await = JsonLoadResponse::default();
    *state.json_ip.write().await = JsonIpResponse::default();
    *state.check_cookie.write().await = CheckCookieResponse::default();
    state.journal.write().await.clear();
    StatusCode::OK
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn status_from_u16(code: u16) -> StatusCode {
    StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}
