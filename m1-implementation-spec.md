# M1 Implementation Spec: Replace MLM

copilot --resume=b3c8c23b-65b7-4ca4-b012-bd1d32d3f50e

## Goal

A fully working, MAM-compliant, safe audiobook downloader with a web UI.
No LLM, no ABS, no metadata enrichment.
At the end of M1, Windlass replaces MLM: the user can paste a MAM torrent URL,
Windlass downloads it safely, and actively protects HnR compliance on everything
in qBittorrent.

---

## Architecture Constraints (read agent.md before starting)

- **Functional Core, Imperative Shell.** All decisions in `windlass-core/`. All I/O in `windlass/src/shell/`.
- `process_event(state, event) -> (new_state, Vec<Action>)` is the only entry point.
- The shell never makes decisions. It executes Actions and sends Events.
- No mutexes. No shared mutable state outside the event loop.
- Hard file limit: 300 lines. Target: under 200.
- All domain values must be newtypes. No raw primitives across the core/shell boundary.
- Clippy pedantic + nursery. Zero warnings, zero suppressed rules (unless pre-existing).
- Tests: Tier 1 (unit), Tier 2 (WireMock), Tier 3 (tempdir), Tier 4 (real Docker, `#[ignore]`).
- Coverage target: 100% on Tiers 1–3.

---

## Step 1: SQLite Foundation — `windlass-db` crate

**Goal:** A new `windlass-db` crate owns all SQL: migrations, the pool type, and
typed query functions. The shell and web handlers call these functions; no raw
SQL appears outside this crate. Nothing reads or writes yet — just clean
migrations and a connected pool.

### New crate: `windlass-db`

Add to `[workspace]` members in root `Cargo.toml`:

```toml
"windlass-db",
```

Add to `[workspace.dependencies]`:

```toml
sqlx      = { version = "0.8", features = ["sqlite", "runtime-tokio-rustls", "chrono", "migrate", "macros"] }
windlass-db = { path = "windlass-db" }
```

Create `windlass-db/Cargo.toml`:

```toml
[package]
name = "windlass-db"
version.workspace = true
edition.workspace = true

[dependencies]
sqlx           = { workspace = true }
windlass-types = { workspace = true }
tokio          = { workspace = true }
tracing        = { workspace = true }
chrono         = { workspace = true }
thiserror      = { workspace = true }
```

### File layout

```
windlass-db/
  migrations/          ← all .sql files, owned by this crate
  src/
    lib.rs             ← pub use DbPool; pub fn connect(); pub async fn migrate()
    alerts.rs          ← insert(), get_all(), mark_read()
    torrents.rs        ← upsert(), get_all()
    events.rs          ← insert(), get_recent()
    download_queue.rs  ← enqueue(), update_status(), blacklist()
    books.rs           ← upsert(), get_by_mam_id()
```

`DbPool` is a newtype over `sqlx::SqlitePool` so downstream crates do not take a
direct `sqlx` dependency:

```rust
#[derive(Clone)]
pub struct DbPool(sqlx::SqlitePool);

impl DbPool {
    /// Opens (or creates) the SQLite database at `path` in WAL mode.
    ///
    /// WAL mode is mandatory: it allows concurrent readers alongside the single
    /// writer, which is the correct model for Windlass — the shell writes (action
    /// execution) and the web handlers read (API responses) concurrently.
    /// sqlx serializes writers internally; no channel actor is needed.
    pub async fn connect(path: &str) -> Result<Self, DbError> {
        let pool = sqlx::SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal),
        )
        .await
        .map_err(DbError::Connect)?;
        Ok(Self(pool))
    }

    pub async fn migrate(&self) -> Result<(), DbError> {
        sqlx::migrate!()
            .run(&self.0)
            .await
            .map_err(DbError::Migrate)
    }

    pub(crate) fn inner(&self) -> &sqlx::SqlitePool { &self.0 }
}
```

### Migration files

Files live in `windlass-db/migrations/`, named `NNNN_description.sql`,
run in order by `sqlx::migrate!()`.

**`windlass/migrations/0001_books.sql`**

```sql
CREATE TABLE IF NOT EXISTS books (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    mam_id     INTEGER UNIQUE,
    title      TEXT,
    author     TEXT,
    status     TEXT NOT NULL DEFAULT 'pending_metadata',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

**`windlass/migrations/0002_torrents.sql`**

```sql
CREATE TABLE IF NOT EXISTS torrents (
    hash              TEXT PRIMARY KEY,
    book_id           INTEGER REFERENCES books(id),
    mam_id            INTEGER,
    name              TEXT NOT NULL,
    state             TEXT NOT NULL,
    seeding_time_secs INTEGER NOT NULL DEFAULT 0,
    downloaded_bytes  INTEGER NOT NULL DEFAULT 0,
    seen_at           TEXT NOT NULL,
    added_at          TEXT NOT NULL DEFAULT (datetime('now'))
);
```

**`windlass/migrations/0003_download_queue.sql`**

```sql
CREATE TABLE IF NOT EXISTS download_queue (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    book_id    INTEGER REFERENCES books(id),
    mam_id     INTEGER NOT NULL,
    status     TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
-- status values: pending | downloading | seeding | satisfied | failed | blacklisted
```

**`windlass/migrations/0004_events.sql`**

```sql
CREATE TABLE IF NOT EXISTS events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    source     TEXT NOT NULL,
    action     TEXT NOT NULL,
    book_id    INTEGER REFERENCES books(id),
    detail     TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
-- source values: compliance | download | user | system
-- Retention: purge rows older than 90 days on a scheduled wakeup (added later).
```

**`windlass/migrations/0005_alerts.sql`**

```sql
CREATE TABLE IF NOT EXISTS alerts (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    priority   TEXT NOT NULL,
    title      TEXT NOT NULL,
    body       TEXT NOT NULL,
    read       INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
-- priority values: info | warning | critical
-- Retention: purge rows older than 30 days on a scheduled wakeup (added later).
```

### Typed query functions

Each source file exports plain async functions. Example signatures:

```rust
// alerts.rs
pub async fn insert(pool: &DbPool, priority: AlertPriority, title: &str, body: &str) -> Result<(), DbError>
pub async fn get_all(pool: &DbPool) -> Result<Vec<AlertRow>, DbError>
pub async fn mark_read(pool: &DbPool, id: i64) -> Result<(), DbError>

// torrents.rs
pub async fn upsert(pool: &DbPool, record: &TorrentRow) -> Result<(), DbError>
pub async fn get_all(pool: &DbPool) -> Result<Vec<TorrentRow>, DbError>

// events.rs
pub async fn insert(pool: &DbPool, source: &str, action: &str, book_id: Option<i64>, detail: Option<&str>) -> Result<(), DbError>
pub async fn get_recent(pool: &DbPool, limit: u32) -> Result<Vec<EventRow>, DbError>

// download_queue.rs
pub async fn enqueue(pool: &DbPool, mam_id: MamTorrentId, book_id: i64) -> Result<(), DbError>
pub async fn update_status(pool: &DbPool, mam_id: MamTorrentId, status: &str) -> Result<(), DbError>
pub async fn blacklist(pool: &DbPool, mam_id: MamTorrentId) -> Result<(), DbError>

// books.rs
pub async fn upsert(pool: &DbPool, mam_id: MamTorrentId) -> Result<i64, DbError>  // returns book_id
pub async fn get_by_mam_id(pool: &DbPool, mam_id: MamTorrentId) -> Result<Option<BookRow>, DbError>
```

`TorrentRow`, `AlertRow`, `EventRow`, `BookRow` are plain structs in `lib.rs` —
not core types. They are the DB representation. The shell converts between
`TorrentRecord` (core type) and `TorrentRow` (DB type) at the boundary.

### Config changes (`windlass/src/shell/config.rs`)

Add:

```rust
pub db_path: String,
```

Parse from env:

```rust
db_path: var("DB_PATH").unwrap_or_else(|_| "./windlass.db".to_string()),
```

### Shell startup (`windlass/src/shell/mod.rs`)

Add `windlass-db` to `windlass/Cargo.toml` dependencies.

Before the event loop:

```rust
let db_pool = windlass_db::DbPool::connect(&config.db_path)
    .await
    .context("Failed to open SQLite database")?;
db_pool.migrate().await.context("Database migration failed")?;
```

### AppState changes (`windlass-web/src/app_state.rs`)

Add `windlass-web` dep on `windlass-db`.

Add to `AppState`:

```rust
pub db_pool: windlass_db::DbPool,
```

Pass the pool when constructing `AppState` in `shell/mod.rs`.

### ShellContext changes (`windlass/src/shell/mod.rs`)

Add `db_pool: windlass_db::DbPool` to `ShellContext`. Pass it into `actions.rs`
methods that need DB access (Steps 2 onwards).
Shell actions call `windlass_db::alerts::insert(...)`, never raw SQL.

### Tests

- Tier 3: start with a `tempfile::TempDir`, set `DB_PATH` to a file inside it,
  construct the pool, run `sqlx::migrate!()`, assert all five tables exist by
  querying `sqlite_master`.

### Definition of done

`just check` passes. `just coverage` passes. Migrations run against a temp file DB
in the test suite with no errors.

---

## Step 2: Notification as Action

**Goal:** Replace `GotifyClient` entirely. `Action::SendAlert` writes to the `alerts`
table. A Notifications page in the web UI shows all alerts. Every subsequent step
uses `Action::SendAlert` from core — no direct I/O.

### Types (`windlass-core/src/actions.rs`)

Replace:

```rust
SendGotifyAlert(AlertPriority, String),
```

With:

```rust
SendAlert {
    priority: AlertPriority,
    title: String,
    body: String,
},
```

`AlertPriority` already exists in `windlass-types`. No new types needed.

### Core handlers

Find every handler in `windlass-core/src/handlers/` that produces
`Action::SendGotifyAlert(priority, message)` and replace with:

```rust
Action::SendAlert {
    priority,
    title: "Windlass".into(),   // or a meaningful title per call site
    body: message,
}
```

Update all `match action` arms and tests that reference `SendGotifyAlert`.

### Shell actions (`windlass/src/shell/actions.rs`)

Replace `send_gotify_alert`:

```rust
pub(super) async fn send_alert(
    &self,
    priority: AlertPriority,
    title: String,
    body: String,
) {
    let priority_str = match priority {
        AlertPriority::Info => "info",
        AlertPriority::Warning => "warning",
        AlertPriority::Critical => "critical",
    };
    if let Err(e) = sqlx::query!(
        "INSERT INTO alerts (priority, title, body) VALUES (?, ?, ?)",
        priority_str,
        title,
        body
    )
    .execute(&self.db_pool)
    .await
    {
        tracing::warn!("Failed to write alert to DB: {e}");
    }
}
```

Note: fire-and-forget, consistent with the previous Gotify behaviour.

### Remove Gotify

- Delete `windlass-clients/src/gotify.rs`.
- Remove `pub mod gotify;` from `windlass-clients/src/lib.rs`.
- Remove `GotifyClient` construction and `gotify` field from `ShellContext` in `windlass/src/shell/mod.rs`.
- Remove `gotify_url` and `gotify_token` from `windlass/src/shell/config.rs`.
- Remove `GOTIFY_URL`, `GOTIFY_TOKEN` env vars from the `windlass` service in `docker-compose.dev.yml`.
- Remove the `mock-gotify` service from `docker-compose.dev.yml`.
- Remove `GOTIFY_ADMIN_URL` from the `chaos-controller` service in `docker-compose.dev.yml`.
- Update `windlass-testkit` if it references the Gotify admin URL.
- Update `windlass-types/src/lib.rs`: `HttpExchange.module` comment — remove `"gotify"` from the examples.

### Web route (`windlass-web/`)

Add `GET /api/v1/alerts` handler:

- Query all alerts from `db_pool`, order by `created_at DESC`, limit 200.
- Return JSON array: `[{ id, priority, title, body, read, created_at }]`.

Add `POST /api/v1/alerts/{id}/read` handler:

- Sets `read = 1` for the given alert id.

Add frontend Notifications page at `/notifications`:

- Lists all alerts, newest first, unread highlighted.
- "Mark as read" button per alert.
- Unread count shown in nav badge.

### Tests

- Tier 1: core handler tests — verify the new `SendAlert` variant is produced
  where `SendGotifyAlert` was produced before.
- Tier 3: `send_alert()` shell method — write an alert, query the DB, assert the row.
- Tier 3: `GET /api/v1/alerts` — seed the DB, hit the route, assert JSON shape.

### Definition of done

`just check` passes. `just coverage` passes. `just integration` passes (integration
tests that previously verified Gotify calls should be updated — they now verify
alert rows instead). No reference to `Gotify` or `gotify` remains in the codebase
except in `git` history.

---

## Step 3: qBit Read Client + Integration Tests

**Goal:** `QbitClient` can fetch full torrent details and application preferences.
A real qBit integration test stack verifies these methods against a live container.

### New types (`windlass-types/src/lib.rs`)

```rust
/// The info hash of a torrent as reported by qBittorrent (40-char hex string).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TorrentHash(pub String);

/// A MAM torrent ID parsed from the torrent's comment field.
/// Comment URL format: https://www.myanonamouse.net/t/12345
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MamTorrentId(pub u64);
```

### New types (`windlass-clients/src/qbit.rs` or split to `windlass-clients/src/qbit/details.rs`)

> If `qbit.rs` approaches 300 lines, split: move `QbitClient` impl into
> `qbit/client.rs`, details structs into `qbit/details.rs`, keep `qbit/mod.rs`
> as the public re-export. Check line count before deciding.

```rust
/// Full torrent record as returned by /api/v2/torrents/info.
#[derive(Debug, Clone)]
pub struct QbitTorrentDetails {
    pub hash: TorrentHash,
    pub name: TorrentName,
    pub state: QbitTorrentState,
    pub seeding_time_secs: u64,
    pub downloaded_bytes: u64,
    pub mam_id: Option<MamTorrentId>,   // parsed from comment field
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QbitTorrentState {
    Downloading,
    StalledDownloading,
    Uploading,          // active seeding
    StalledUploading,
    ForcedUpload,       // force-resumed seeding
    PausedDownloading,
    PausedUploading,
    Error,
    Other(String),
}

/// qBittorrent application preferences relevant to compliance.
#[derive(Debug, Clone)]
pub struct QbitPreferences {
    pub max_active_torrents: u32,
    pub max_active_downloads: u32,
    pub max_active_uploads: u32,
}
```

### MAM ID parsing

Extract the MAM torrent ID from the `comment` field of a torrent. The comment set
by MAM is the torrent page URL: `https://www.myanonamouse.net/t/12345`.

```rust
fn parse_mam_id(comment: &str) -> Option<MamTorrentId> {
    // Accepts both /t/12345 and /tor/12345 formats.
    let path = comment
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.myanonamouse.net");
    if let Some(rest) = path.strip_prefix("/t/").or_else(|| path.strip_prefix("/tor/")) {
        rest.split('/').next()?.parse::<u64>().ok().map(MamTorrentId)
    } else {
        None
    }
}
```

Unit-test `parse_mam_id` with: valid `/t/` URL, valid `/tor/` URL, empty string,
unrelated comment, numeric-only string.

### New `QbitClient` methods

**`list_torrent_details`** — calls `/api/v2/torrents/info`:

```rust
pub async fn list_torrent_details(
    &self,
    cookie: &AuthCookie,
) -> Vec<QbitTorrentDetails>
```

Deserialize the JSON array. Each object has at minimum:
`hash`, `name`, `state`, `seeding_time` (seconds), `downloaded`, `comment`.

The `state` string from qBit maps to `QbitTorrentState` variants:

- `"downloading"` → `Downloading`
- `"stalledDL"` → `StalledDownloading`
- `"uploading"` → `Uploading`
- `"stalledUP"` → `StalledUploading`
- `"forcedUP"` → `ForcedUpload`
- `"pausedDL"` → `PausedDownloading`
- `"pausedUP"` → `PausedUploading`
- `"error"` → `Error`
- anything else → `Other(s)`

Parse `comment` → `mam_id` using `parse_mam_id`.

Returns empty vec on any error (consistent with `list_torrents`).

**`get_preferences`** — calls `/api/v2/app/preferences`:

```rust
pub async fn get_preferences(
    &self,
    cookie: &AuthCookie,
) -> Option<QbitPreferences>
```

Deserialize `max_active_torrents`, `max_active_downloads`, `max_active_uploads`
from the JSON object. Returns `None` on any error.

### WireMock tests (Tier 2)

Add to the existing `#[cfg(test)]` block in `qbit.rs`:

- `list_torrent_details_returns_parsed_records` — mock returns JSON with two torrents,
  one with a MAM comment URL. Assert hash, state, seeding_time_secs, downloaded_bytes,
  mam_id fields.
- `list_torrent_details_maps_all_state_strings` — mock returns one torrent per state
  string. Assert correct `QbitTorrentState` variant.
- `list_torrent_details_returns_empty_on_bad_json`
- `list_torrent_details_returns_empty_on_network_error`
- `get_preferences_returns_parsed_limits`
- `get_preferences_returns_none_on_bad_json`

### Real qBit integration test stack (Tier 4)

Create `docker-compose.qbit-integration.yml` at the repo root:

```yaml
# Integration test stack with a real qBittorrent instance.
# Used by: windlass-clients/tests/qbit_integration.rs
# Run with: docker compose -f docker-compose.qbit-integration.yml up -d
# Tear down: docker compose -f docker-compose.qbit-integration.yml down -v

services:
  qbittorrent:
    image: lscr.io/linuxserver/qbittorrent:latest
    environment:
      PUID: "1000"
      PGID: "1000"
      WEBUI_PORT: "8080"
      QBITTORRENT__WebUI__Username: admin
      QBITTORRENT__WebUI__Password: adminadmin
    ports:
      - "18090:8080"
    healthcheck:
      test:
        [
          "CMD-SHELL",
          "curl -sf http://localhost:8080/api/v2/app/version || exit 1",
        ]
      interval: 5s
      timeout: 3s
      retries: 15

volumes: {}
```

Commit a tiny pre-generated test torrent to `windlass-clients/tests/fixtures/test.torrent`.
This torrent contains a single 1 KB file and has a comment field set to
`https://www.myanonamouse.net/t/99999`. It points at no tracker — it will stall
immediately, but adding it is sufficient to exercise all API methods.

To generate the fixture (run once, commit the result):

```
# Using mktorrent or any torrent creation tool:
dd if=/dev/urandom of=/tmp/windlass_test_file.bin bs=1024 count=1
mktorrent -o test.torrent -c "https://www.myanonamouse.net/t/99999" /tmp/windlass_test_file.bin
```

Integration test file `windlass-clients/tests/qbit_integration.rs`:

```rust
// These tests require the qBit integration stack to be running.
// Start with: docker compose -f docker-compose.qbit-integration.yml up -d
// Then run with: cargo test --test qbit_integration -- --ignored

const QBIT_URL: &str = "http://localhost:18090";
const TORRENT_FIXTURE: &[u8] = include_bytes!("fixtures/test.torrent");

// Test: authenticate succeeds
// Test: list_torrent_details returns empty on fresh qBit
// Test: add test.torrent, then list_torrent_details returns one record
//       with hash matching the fixture, mam_id = Some(99999)
// Test: get_preferences returns non-zero limits
// Test: list_torrent_details after add returns correct initial values
//       (seeding_time_secs == 0, downloaded_bytes == 0)
```

All integration tests must be `#[ignore]` and collected into `just integration`
(check the existing `justfile` recipe and add the new test binary there).

### Definition of done

`just check` passes. `just coverage` passes.
`just integration` passes (all four existing integration tests plus new qBit read tests).

---

## Step 4: qBit Write Client + Integration Tests

**Goal:** `QbitClient` can pause, resume, force-resume, delete, and set file priorities
on torrents. All methods tested with WireMock and against real qBit.

### New `QbitClient` methods

All write methods are fire-and-forget at the client level — they log warnings on
failure and do not propagate errors. The compliance monitor (Step 5) will observe
the new state on the next poll.

**`pause_torrent`** — POST `/api/v2/torrents/pause`, form body `hashes={hash}`:

```rust
pub async fn pause_torrent(&self, cookie: &AuthCookie, hash: &TorrentHash)
```

**`resume_torrent`** — POST `/api/v2/torrents/resume`, form body `hashes={hash}`:

```rust
pub async fn resume_torrent(&self, cookie: &AuthCookie, hash: &TorrentHash)
```

**`force_resume_torrent`** — POST `/api/v2/torrents/setForceStart`,
form body `hashes={hash}&value=true`:

```rust
pub async fn force_resume_torrent(&self, cookie: &AuthCookie, hash: &TorrentHash)
```

**`delete_torrent`** — POST `/api/v2/torrents/delete`,
form body `hashes={hash}&deleteFiles=false`:

```rust
pub async fn delete_torrent(&self, cookie: &AuthCookie, hash: &TorrentHash)
```

Always pass `deleteFiles=false`. Windlass never deletes the data files; it only
removes the torrent from qBit's list.

**`set_all_files_priority`** — POST `/api/v2/torrents/filePrio`,
form body `hash={hash}&id=all&priority=1`:

```rust
pub async fn set_all_files_priority(&self, cookie: &AuthCookie, hash: &TorrentHash)
```

Priority `1` = Normal in qBittorrent's API (download at normal priority).
Priority `0` = Do not download. This method ensures no files are skipped,
enforcing the MAM "no partials" rule.

### WireMock tests (Tier 2)

For each write method:

- Success (200 response) — assert the correct endpoint and form params were sent.
- Network error — assert the method returns without panicking.

### Integration tests (Tier 4, extends `qbit_integration.rs`)

```
// Test: add test.torrent, pause it, list_torrent_details shows PausedDownloading
// Test: pause then resume, list shows Downloading (or StalledDownloading — no seeds)
// Test: add test.torrent, set_all_files_priority succeeds (200 from qBit)
// Test: add test.torrent, delete it, list returns empty
```

### Definition of done

Same gates as Step 3.

---

## Step 5: MAM Compliance Monitor

**Goal:** A background compliance poll detects every torrent in qBit and enforces
all MAM compliance rules as pure core logic. The shell only provides I/O.

This step does NOT add any new external HTTP clients. It wires together Steps 1–4.

### New types (`windlass-types/src/lib.rs`)

Add to `WakeupId`:

```rust
CompliancePoll,
```

### New types (`windlass-core/src/types.rs`)

Add `TorrentRecord` and `ComplianceConfig` to `SystemState`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentRecord {
    pub hash: TorrentHash,
    pub name: TorrentName,
    pub state: TorrentState,           // mirrored from QbitTorrentState
    pub seeding_time_secs: u64,
    pub downloaded_bytes: u64,
    pub mam_id: Option<MamTorrentId>,
    pub seen_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum TorrentState {
    Downloading,
    StalledDownloading,
    Uploading,
    StalledUploading,
    ForcedUpload,
    PausedDownloading,
    PausedUploading,
    Error,
    Other,
}
```

Note: `TorrentState` is a core type. `QbitTorrentState` in `windlass-clients` is the
shell-side deserialization type. The shell converts `QbitTorrentState → TorrentState`
before sending the event. This keeps the core free of client-layer knowledge.

Add to `SystemState`:

```rust
pub torrents: HashMap<TorrentHash, TorrentRecord>,
pub unsatisfied_quota_limit: u32,   // loaded from config, default 50
```

Add `ComplianceConfig` to `windlass/src/shell/config.rs`:

```rust
pub unsatisfied_quota_limit: u32,
pub compliance_poll_interval_secs: u64,
```

Parsed from env:

```rust
unsatisfied_quota_limit: var("MAM_UNSATISFIED_QUOTA_LIMIT")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(50),
compliance_poll_interval_secs: var("COMPLIANCE_POLL_INTERVAL_SECS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(60),
```

Pass `unsatisfied_quota_limit` into `SystemState::initial()`.

### New events (`windlass-core/src/events.rs`)

```rust
QbitTorrentDetailsReceived {
    at: DateTime<Utc>,
    torrents: Vec<TorrentRecord>,
},

DeleteTorrentRequested {
    at: DateTime<Utc>,
    hash: TorrentHash,
},
```

### New actions (`windlass-core/src/actions.rs`)

```rust
FetchTorrentDetails(AuthCookie),
PauseTorrent(TorrentHash),
ForceResumeTorrent(TorrentHash),
DeleteTorrent(TorrentHash),
SetAllFilesPriority(TorrentHash),
UpsertTorrentRecords(Vec<TorrentRecord>),
BlacklistMamId(MamTorrentId),
WriteEvent {
    source: String,
    action: String,
    book_id: Option<i64>,
    detail: Option<String>,
},
```

### New core handler (`windlass-core/src/handlers/compliance.rs`)

Create a new file. Keep it under 200 lines; split further if needed.

#### `handle_wakeup_compliance_poll`

Called when `Event::Wakeup { id: WakeupId::CompliancePoll }` fires.

```rust
fn handle_wakeup_compliance_poll(state: &SystemState) -> Vec<Action> {
    let mut actions = vec![
        Action::ScheduleWakeup(
            WakeupId::CompliancePoll,
            Duration::from_secs(state.compliance_poll_interval_secs),
        ),
    ];
    if let QbitState::Ready { cookie, .. } = &state.qbit {
        actions.push(Action::FetchTorrentDetails(cookie.clone()));
    }
    actions
}
```

Schedule the initial `CompliancePoll` wakeup when qBit first becomes `Ready`
(in `handlers/qbit.rs`, alongside the existing heartbeat scheduling).

#### `handle_qbit_torrent_details_received`

Called when `Event::QbitTorrentDetailsReceived { torrents }` fires.

Steps in order:

1. **No-partials enforcement.** For every torrent in `torrents` whose hash is NOT
   already in `state.torrents` (i.e. first time seen), push
   `Action::SetAllFilesPriority(hash)`.

2. **Stalled zero-byte detection.** For every torrent where
   `downloaded_bytes == 0 AND state ∈ {StalledDownloading, Error}`,
   push `Action::DeleteTorrent(hash)`.
   If the torrent has a `mam_id`, also push `Action::BlacklistMamId(mam_id)`.
   Push `Action::WriteEvent { source: "compliance", action: "dead_torrent_removed", ... }`.

3. **HnR at-risk alert.** For every torrent where
   `downloaded_bytes > 0 AND seeding_time_secs < 72 * 3600 AND state ∈ {StalledUploading, Error}`,
   push:

   ```rust
   Action::SendAlert {
       priority: AlertPriority::Critical,
       title: "HnR at risk".into(),
       body: format!("{}: stalled with {}h seeding, {}h required",
           torrent.name.0,
           torrent.seeding_time_secs / 3600,
           72 - torrent.seeding_time_secs / 3600),
   }
   ```

4. **Unsatisfied quota check.** Count torrents where
   `downloaded_bytes > 0 AND seeding_time_secs < 72 * 3600`.
   Call this `unsatisfied_count`.
   - If `unsatisfied_count >= state.unsatisfied_quota_limit`:
     push `Action::SendAlert { priority: Critical, title: "Quota limit reached", ... }`.
   - Else if `unsatisfied_count >= state.unsatisfied_quota_limit.saturating_sub(5)`:
     push `Action::SendAlert { priority: Warning, title: "Approaching quota limit", ... }`.

5. **Queue orchestration.** The goal is to ensure unsatisfied torrents are never
   parked by qBit's active-torrent limit.
   - Count `active_count` = torrents where state ∈ `{Downloading, Uploading, ForcedUpload}`.
   - If `active_count >= state.max_active_torrents` (tracked in SystemState, updated
     from `FetchPreferences` — see below):
     - Find unsatisfied torrents (downloaded > 0, seeding_time < 72h) that are
       NOT in an active state (i.e. `PausedUploading` or `StalledUploading`).
     - If any exist: find the oldest satisfied torrent (seeding_time ≥ 72h) currently
       in `Uploading` state and push `Action::PauseTorrent(hash)` for it.
       Push `Action::ForceResumeTorrent(hash)` for the unsatisfied torrent.

6. **Persist.** Push `Action::UpsertTorrentRecords(all_torrent_records)`.

7. **Update state.** Replace `state.torrents` with the new records.
   Return updated state + all actions collected above.

#### `handle_delete_torrent_requested`

Called when `Event::DeleteTorrentRequested { hash }` fires (user-initiated deletion
via web UI — Step 6 wires this up).

```rust
fn handle_delete_torrent_requested(
    state: SystemState,
    hash: TorrentHash,
) -> (SystemState, Vec<Action>) {
    if let Some(t) = state.torrents.get(&hash) {
        if t.downloaded_bytes > 0 && t.seeding_time_secs < 72 * 3600 {
            let hours_done = t.seeding_time_secs / 3600;
            let hours_left = 72u64.saturating_sub(hours_done);
            return (state, vec![Action::SendAlert {
                priority: AlertPriority::Warning,
                title: "HnR lock — cannot delete".into(),
                body: format!(
                    "{}: {hours_done}h seeded, {hours_left}h remaining. \
                     Manual deletion blocked to protect your HnR.",
                    t.name.0
                ),
            }]);
        }
    }
    (state, vec![
        Action::DeleteTorrent(hash.clone()),
        Action::WriteEvent {
            source: "user".into(),
            action: "torrent_deleted".into(),
            book_id: None,
            detail: Some(format!("{{\"hash\":\"{}\"}}", hash.0)),
        },
    ])
}
```

### qBit preferences tracking

To support queue orchestration, the core needs to know qBit's `max_active_torrents`.
Add to `SystemState`:

```rust
pub max_active_torrents: u32,   // default 5; updated by FetchPreferences result
```

Add new event and action:

```rust
// In events.rs:
QbitPreferencesReceived {
    at: DateTime<Utc>,
    max_active_torrents: u32,
    max_active_downloads: u32,
    max_active_uploads: u32,
},

// In actions.rs:
FetchQbitPreferences(AuthCookie),
```

Schedule `FetchQbitPreferences` alongside `FetchTorrentDetails` in the compliance
poll handler. Add a handler in `handlers/qbit.rs` for `QbitPreferencesReceived`
that updates `state.max_active_torrents`.

### Shell actions (`windlass/src/shell/actions.rs`)

Shell actions call `windlass_db` functions — no raw SQL here.

```rust
pub(super) async fn upsert_torrent_records(&self, records: Vec<TorrentRecord>) {
    for r in records {
        let row = TorrentRow::from_record(&r);
        if let Err(e) = windlass_db::torrents::upsert(&self.db_pool, &row).await {
            tracing::warn!("Failed to upsert torrent {}: {e}", r.hash.0);
        }
    }
}

pub(super) async fn blacklist_mam_id(&self, mam_id: MamTorrentId) {
    if let Err(e) = windlass_db::download_queue::blacklist(&self.db_pool, mam_id).await {
        tracing::warn!("Failed to blacklist mam_id {}: {e}", mam_id.0);
    }
}

pub(super) async fn write_event(
    &self,
    source: &str,
    action: &str,
    book_id: Option<i64>,
    detail: Option<&str>,
) {
    if let Err(e) = windlass_db::events::insert(&self.db_pool, source, action, book_id, detail).await {
        tracing::warn!("Failed to write event: {e}");
    }
}
```

    let qbit = self.qbit.clone();
    tokio::spawn(causal_tx.run(move |causal_tx| async move {
        let raw = qbit.list_torrent_details(&cookie).await;
        let torrents = raw.into_iter().map(convert_qbit_torrent_details).collect();
        causal_tx.send(Event::QbitTorrentDetailsReceived {
            at: Utc::now(),
            torrents,
        }).await;
    }));

}

pub(super) fn fetch_qbit_preferences(&self, cookie: AuthCookie, causal_tx: CausalTx) {
let qbit = self.qbit.clone();
tokio::spawn(causal_tx.run(move |causal_tx| async move {
if let Some(prefs) = qbit.get_preferences(&cookie).await {
causal_tx.send(Event::QbitPreferencesReceived {
at: Utc::now(),
max_active_torrents: prefs.max_active_torrents,
max_active_downloads: prefs.max_active_downloads,
max_active_uploads: prefs.max_active_uploads,
}).await;
}
}));
}

pub(super) fn pause_torrent(&self, hash: TorrentHash) {
let qbit = self.qbit.clone();
let cookie = self.cached_cookie(); // helper that reads cookie from ShellContext
tokio::spawn(async move {
qbit.pause_torrent(&cookie, &hash).await;
});
}

pub(super) fn force_resume_torrent(&self, hash: TorrentHash) { /_ same pattern _/ }

pub(super) fn delete_torrent(&self, hash: TorrentHash) {
let qbit = self.qbit.clone();
let cookie = self.cached_cookie();
tokio::spawn(async move {
qbit.delete_torrent(&cookie, &hash).await;
});
}

pub(super) fn set_all_files_priority(&self, hash: TorrentHash) { /_ same pattern _/ }

pub(super) async fn upsert_torrent_records(&self, records: Vec<TorrentRecord>) {
for r in records {
sqlx::query!(
"INSERT INTO torrents (hash, name, state, seeding_time_secs, downloaded_bytes,
mam_id, seen_at)
VALUES (?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(hash) DO UPDATE SET
name = excluded.name,
state = excluded.state,
seeding_time_secs = excluded.seeding_time_secs,
downloaded_bytes = excluded.downloaded_bytes,
mam_id = COALESCE(excluded.mam_id, torrents.mam_id),
seen_at = excluded.seen_at",
r.hash.0, r.name.0, torrent_state_str(&r.state),
r.seeding_time_secs as i64, r.downloaded_bytes as i64,
r.mam_id.map(|id| id.0 as i64), r.seen_at.to_rfc3339()
)
.execute(&self.db_pool)
.await
.unwrap_or_else(|e| { tracing::warn!("Failed to upsert torrent: {e}"); ... });
}
}

````

Note on cookie access: `pause_torrent`, `force_resume_torrent`, `delete_torrent`,
and `set_all_files_priority` need the current auth cookie. The Action variants
should carry the `AuthCookie` as a field rather than reading it from shared state.
Add the cookie to `Action::PauseTorrent`, `Action::ForceResumeTorrent`,
`Action::DeleteTorrent`, `Action::SetAllFilesPriority`.
The core can extract the cookie from `QbitState::Ready { cookie }` when producing
these actions.

### Note: scope of `BlacklistMamId` in M1

`Action::BlacklistMamId` is produced in M1 in exactly one place: the stalled
zero-byte cleanup in `handle_qbit_torrent_details_received`. When a dead torrent
is auto-deleted, its MAM ID is blacklisted so future automated discovery (M4+)
does not re-queue it.

In M1 the blacklist is write-only from the core's perspective. The core does not
read the blacklist back because M1 has no automated discovery. In M4, before
auto-queuing any MAM ID, the shell will call
`windlass_db::download_queue::is_blacklisted(pool, mam_id)` before emitting the
event — this is a DB read in the shell, not a core state check, because the
blacklist may grow large and does not need to be held in memory.

### Tests

- Tier 1: `handle_qbit_torrent_details_received` — unit tests for each compliance
  branch:
  - New torrent → `SetAllFilesPriority` produced.
  - Stalled, 0 bytes → `DeleteTorrent` + `BlacklistMamId` produced.
  - Stalled, >0 bytes, <72h → `SendAlert` (HnR at risk) produced.
  - Stalled, >0 bytes, <72h → `DeleteTorrent` NOT produced.
  - `unsatisfied_count` at `limit - 5` → warning alert produced.
  - `unsatisfied_count` at `limit` → critical alert produced.
  - Active limit full, unsatisfied parked → `PauseTorrent` + `ForceResumeTorrent` produced.
- Tier 1: `handle_delete_torrent_requested`:
  - Downloaded > 0, seed_time < 72h → `SendAlert` produced, `DeleteTorrent` NOT produced.
  - Downloaded > 0, seed_time ≥ 72h → `DeleteTorrent` produced.
  - Downloaded == 0 → `DeleteTorrent` produced.
  - Hash not in state → `DeleteTorrent` produced (fail-safe: unknown hash is safe to delete).
- Tier 3: `upsert_torrent_records` — write records, query DB, assert values.
- Tier 3: `write_event` — write event, query DB, assert source/action.

### Definition of done

`just check` passes. `just coverage` passes. `just integration` passes.

---

## Step 6: Manual Download

**Goal:** User pastes a MAM torrent URL in the web UI. Windlass fetches the torrent,
adds it to qBit, and the compliance monitor picks it up on the next poll.

### MAM torrent fetch

Add method to `MamClient` (`windlass-clients/src/mam/`):

```rust
/// Downloads the .torrent file bytes for a given MAM torrent ID.
/// Returns `None` on any error.
pub async fn fetch_torrent(&self, mam_id: MamTorrentId) -> Option<Vec<u8>>
````

URL: `https://www.myanonamouse.net/tor/download.php?tid={mam_id}`
Uses the existing session cookie. The response body is the raw `.torrent` bytes.

WireMock test: mock the endpoint, assert the correct bytes are returned.
WireMock test: 403 response returns `None`.

### qBit torrent add

Add method to `QbitClient`:

```rust
/// Adds a torrent to qBittorrent from raw .torrent file bytes.
/// Returns the info hash on success, None on failure.
pub async fn add_torrent(
    &self,
    cookie: &AuthCookie,
    torrent_bytes: Vec<u8>,
) -> Option<TorrentHash>
```

Endpoint: POST `/api/v2/torrents/add` with `multipart/form-data`,
field name `torrents`, filename `file.torrent`, content type `application/x-bittorrent`.

After adding, fetch the new torrent's hash by comparing `list_torrent_details`
before and after. Or: qBit returns the info hash as the response body in newer
versions — check and use that if available, otherwise fall back to the list diff.

WireMock tests: success case returns hash; failure case returns None.

### New types (`windlass-types/src/lib.rs`)

No new types needed — `MamTorrentId` and `TorrentHash` already added in Step 3.

### New events (`windlass-core/src/events.rs`)

```rust
ManualDownloadRequested {
    at: DateTime<Utc>,
    mam_id: MamTorrentId,
},

TorrentAddedToQbit {
    at: DateTime<Utc>,
    mam_id: MamTorrentId,
    hash: TorrentHash,
},

TorrentAddFailed {
    at: DateTime<Utc>,
    mam_id: MamTorrentId,
},
```

### New actions (`windlass-core/src/actions.rs`)

```rust
FetchAndAddTorrent {
    mam_id: MamTorrentId,
    cookie: AuthCookie,
},
UpsertBook {
    mam_id: MamTorrentId,
},
EnqueueDownload {
    mam_id: MamTorrentId,
    book_id_placeholder: (),   // book_id resolved by shell after UpsertBook
},
```

Alternatively, `FetchAndAddTorrent` can include all context the shell needs,
and the shell handles the DB writes (books + download_queue) atomically in one
action. This is simpler:

```rust
FetchAndAddTorrent {
    mam_id: MamTorrentId,
    cookie: AuthCookie,
},
```

The shell, when executing `FetchAndAddTorrent`:

1. Writes a `books` row (`mam_id`, `status = 'pending_metadata'`).
2. Gets the new `book_id`.
3. Fetches the `.torrent` file from MAM.
4. Adds to qBit.
5. Writes a `download_queue` row (`book_id`, `mam_id`, `status = 'downloading'`).
6. Writes an `events` row (`source = 'user'`, `action = 'manual_download'`).
7. Sends `Event::TorrentAddedToQbit { mam_id, hash }` on success.
8. Sends `Event::TorrentAddFailed { mam_id }` on failure.

### New core handler (`windlass-core/src/handlers/download.rs`)

```rust
fn handle_manual_download_requested(
    state: SystemState,
    mam_id: MamTorrentId,
) -> (SystemState, Vec<Action>) {
    // Quota check: if unsatisfied_count >= quota_limit, block.
    let unsatisfied_count = state.torrents.values()
        .filter(|t| t.downloaded_bytes > 0 && t.seeding_time_secs < 72 * 3600)
        .count() as u32;

    if unsatisfied_count >= state.unsatisfied_quota_limit {
        return (state, vec![Action::SendAlert {
            priority: AlertPriority::Warning,
            title: "Download blocked — quota full".into(),
            body: format!(
                "{} unsatisfied torrents at class limit of {}.",
                unsatisfied_count, state.unsatisfied_quota_limit
            ),
        }]);
    }

    let cookie = match &state.qbit {
        QbitState::Ready { cookie, .. } => cookie.clone(),
        _ => {
            return (state, vec![Action::SendAlert {
                priority: AlertPriority::Warning,
                title: "Download blocked — qBit not ready".into(),
                body: "qBittorrent is not connected. Try again shortly.".into(),
            }]);
        }
    };

    (state, vec![Action::FetchAndAddTorrent { mam_id, cookie }])
}

fn handle_torrent_added_to_qbit(
    state: SystemState,
    mam_id: MamTorrentId,
    hash: TorrentHash,
) -> (SystemState, Vec<Action>) {
    (state, vec![
        Action::SendAlert {
            priority: AlertPriority::Info,
            title: "Download started".into(),
            body: format!("MAM torrent {} added to qBittorrent.", mam_id.0),
        },
        Action::WriteEvent {
            source: "download".into(),
            action: "torrent_added".into(),
            book_id: None,
            detail: Some(format!("{{\"mam_id\":{},\"hash\":\"{}\"}}", mam_id.0, hash.0)),
        },
    ])
}

fn handle_torrent_add_failed(
    state: SystemState,
    mam_id: MamTorrentId,
) -> (SystemState, Vec<Action>) {
    (state, vec![Action::SendAlert {
        priority: AlertPriority::Warning,
        title: "Download failed".into(),
        body: format!("Failed to add MAM torrent {} to qBittorrent.", mam_id.0),
    }])
}
```

### Web route

Add to `windlass-web/src/routes/`:

`POST /api/v1/download/add`

Request body:

```json
{ "mam_url": "https://www.myanonamouse.net/t/12345" }
```

or

```json
{ "mam_id": 12345 }
```

Handler:

1. Parse `mam_id` from `mam_url` using the same `parse_mam_id` logic (share the
   implementation — consider putting `parse_mam_id` in `windlass-types` or
   `windlass-core` so both the client and the web handler can use it).
2. `event_tx.send(Event::ManualDownloadRequested { at: Utc::now(), mam_id }).await`.
3. Return `202 Accepted`.

### Frontend (simple panel, grows in Step 7)

A text input box + "Download" button on the new Download panel.
Accepts a full MAM URL or a numeric torrent ID.
On submit: POST to `/api/v1/download/add`, show a toast notification.

### Tests

- Tier 1: `handle_manual_download_requested` with quota full → `SendAlert`, no `FetchAndAddTorrent`.
- Tier 1: `handle_manual_download_requested` with qBit not ready → `SendAlert`.
- Tier 1: `handle_manual_download_requested` with space in quota → `FetchAndAddTorrent` produced.
- Tier 1: `handle_torrent_added_to_qbit` → `SendAlert(Info)` + `WriteEvent` produced.
- Tier 1: `handle_torrent_add_failed` → `SendAlert(Warning)` produced.
- Tier 2: `MamClient::fetch_torrent` — mock endpoint, assert bytes returned.
- Tier 2: `QbitClient::add_torrent` — mock endpoint, assert multipart form sent.
- Tier 3: POST `/api/v1/download/add` with valid URL → 202, `Event::ManualDownloadRequested` in channel.
- Tier 3: POST with invalid URL → 400.

### Definition of done

`just check` passes. `just coverage` passes. `just integration` passes.
Manual end-to-end test: paste a real MAM torrent URL, observe the torrent appear
in qBit within seconds, observe compliance monitor pick it up on next poll.

---

## Step 7: M1 Web UI

**Goal:** Three connected panels that give a complete operational view of what
Windlass is doing. The Notifications page (added in Step 2) is already in nav.

### New web routes

`GET /api/v1/torrents`

- Query `torrents` table, join `books` for title where available.
- Return JSON: `[{ hash, name, title?, mam_id?, state, seeding_time_secs, downloaded_bytes,
hnr_satisfied, hnr_hours_remaining, added_at, seen_at }]`
- `hnr_satisfied = seeding_time_secs >= 72 * 3600`
- `hnr_hours_remaining = max(0, 72 - seeding_time_secs / 3600)`

`GET /api/v1/download-queue`

- Query `download_queue` join `books`.
- Return JSON: `[{ id, mam_id, title?, status, created_at, updated_at }]`

`GET /api/v1/events`

- Query `events`, order by `created_at DESC`, limit 200.
- Return JSON: `[{ id, source, action, book_id?, detail?, created_at }]`

### Frontend panels

All panels use SSE (`/api/v1/stream`) to refresh when new observations arrive —
the existing SSE infrastructure already broadcasts on every state change.

**Torrent Monitor panel** (`/torrents`):

- Table: Name | State | Seeded | HnR Status | Downloaded
- HnR Status column: green badge "Satisfied" if ≥72h, amber "X h remaining" if <72h
  and downloading/seeding, red "At Risk" if stalled with <72h.
- Auto-refreshes on SSE event.

**Download Queue panel** (`/queue`):

- Table: MAM ID | Title | Status | Queued At
- "Add Download" form: text input + button (from Step 6).
- Status badges: pending (grey), downloading (blue), seeding (green),
  satisfied (light green), failed (red), blacklisted (dark grey).

**Event Log panel** (`/events`):

- Table: Time | Source | Action | Detail
- Searchable by source and action.
- Paginated (load more button).

**Navigation**:

- Update the existing nav to include: Dashboard | Torrents | Queue | Events | Notifications | Chaos (dev only).

### Definition of done

`just check` passes. `just coverage` passes. `just integration` passes.
All three panels render correctly in the browser against the dev stack.
Notifications page shows alerts from Step 2.

---

## Completion Criteria for M1

At the end of Step 7:

1. User pastes a MAM torrent URL → Windlass downloads it to qBittorrent.
2. Every torrent in qBittorrent is polled every 60 seconds.
3. No torrent with `downloaded_bytes > 0` and `seeding_time < 72h` can be deleted
   by Windlass or via the web UI.
4. Stalled torrents with `downloaded_bytes == 0` are auto-deleted and blacklisted.
5. qBit's active-torrent limit is managed to keep unsatisfied torrents seeding.
6. Unsatisfied quota approaching the class limit fires an alert.
7. All compliance events are written to the `events` table.
8. All alerts are visible on the Notifications page.
9. Gotify is fully removed from the codebase.
10. `just check && just coverage && just integration` all pass with zero warnings.
