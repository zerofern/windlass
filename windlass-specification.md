# Windlass Operator: Product Specification

This document is the long-term product specification for Windlass. It covers everything
from the current operator core through the full AI Librarian vision. Use it as the
authoritative reference as the project grows.

---

## 1. Project Overview

Windlass is a lightweight, event-driven Rust operator and AI-driven personal librarian. It
manages the lifecycle, network synchronisation, and intelligent curation of a Docker Compose
VPN audiobook torrenting stack (Gluetun + qBittorrent + MLM + Mousehole) while syncing
seamlessly with Audiobookshelf.

### Design Philosophy: Notification-First

The ideal Windlass experience is one where the user never needs to open the web UI. Windlass
runs silently, makes decisions autonomously, and nudges the user at exactly the right moment.
The web UI (Action Center) is a **pipeline oversight tool** — for users who want to check
what is queued, adjust priorities, or add something manually. It is not the primary
interaction surface.

Notification delivery is fully abstracted — the rest of the spec refers to notification
types only; the delivery mechanism is defined once in §2.

---

## 2. Architectural Foundation

- **Paradigm:** Strict Functional Core, Imperative Shell (FCIS) / Sans I/O.
  - _Functional Core:_ A pure, synchronous state machine that makes all decisions without
    side effects. Receives an `Event`, returns `(SystemState, Vec<Action>)`.
  - _Imperative Shell:_ The async Tokio layer that executes API calls, reads files, manages
    Docker sockets, and feeds events back to the Core.
- **Dynamic Docker Discovery:** Automatically identifies dependent containers attached to
  the `service:gluetun` network namespace via bollard.
- **Resilient Network Sync:** Detects VPN drops, frozen NATs, and silent port-sync
  failures. Automatically coordinates stack restarts.
- **Automated Crash Dumps:** Extracts the last 100 log lines from the VPN and all
  dependent containers into a unified dump file upon critical failures.
- **VPN IP Compliance (MAM Rule 1.2):** Gluetun is locked to a single static server
  registered with MAM staff. Windlass monitors the VPN IP and alerts on unexpected changes.
- **Tailscale (Required):** Windlass assumes a Tailscale network for all remote access.
  The server binds to its local network interface; Tailscale exposes it securely to
  authorised devices. All traffic from the mobile companion app — including background fetch
  notification polling — travels over the encrypted Tailscale tunnel. No content ever
  transits Apple's or any third-party infrastructure.

### Notification Architecture

All alert events are persisted to the `alerts` table and dispatched through the configured
notification provider. The rest of the spec refers to notification *types* only — delivery
mechanism details live here.

**Notification types:**

| Type | Description | iOS PWA | Desktop PWA |
|---|---|---|---|
| `Alert` | Title + body + icon + deep-link URL. Tap opens the relevant PWA card. | ✅ | ✅ |
| `Action` | Same as `Alert` plus up to 3 action buttons for in-notification decisions. Degrades gracefully to `Alert` on iOS Web Push — buttons are removed but the tap still deep-links to the correct card where the same choices are presented as UI elements. | ⚠️ degrades | ✅ |
| `Sync` | Silent background push. No visible UI. Wakes the PWA service worker to pre-load data (e.g., fetch suggestions before the user opens the app). Used sparingly — iOS throttles silent pushes aggressively if overused. | ⚠️ throttled | ✅ |

**Severity levels:**

| Severity | Behaviour |
|---|---|
| `Critical` | Delivered immediately, never batched. H&R risk and compliance violations. |
| `High` | Delivered promptly. Book finished, series arrivals, vault guardian, queue additions. |
| `Normal` | Standard delivery. Slog detector, disk space warnings, worker complete. |

**Current provider:** Web Push (VAPID + Service Worker) delivered to the installed PWA.
On mobile, the PWA is installed via Safari "Add to Home Screen" and receives notifications
through Safari's built-in APNs relay. The push payload carries only a silent wake signal;
the PWA fetches actual notification content directly from the Windlass server over Tailscale.
Apple's servers see a tap on the device at a timestamp — nothing else.

**Silent operations (no notification fired):**
- Active series continuation promoted into the Active Queue
- Routine download completions
- Background queue maintenance and `Sync` pre-loads

---

## 3. Data Persistence

Windlass uses a **hybrid database architecture** matched to the access patterns of each tier:

| Engine | Tables | Reason |
|---|---|---|
| **PostgreSQL + pgvector** (Docker on NUC) | All operational tables — `books`, `profile_signals`, `mood_state`, `active_queue`, `download_queue`, `reading_ledger`, `reviews`, `series`, `torrents`, `tags`, `alerts`, `events`, `playback_sessions`, `metadata_cache`, `sync_artifacts`, `context_chunks` | MVCC concurrency (worker writes + server reads simultaneously without locking), `pgvector` native `sparsevec(500)` for exact dot-product scoring, mature ACID guarantees |
| **DuckDB** (in-process library, no separate server) | `book_candidates` | Columnar vectorised execution for batch pre-filter scans across millions of rows; native `ARRAY` types + GIN-equivalent zone maps; zero server overhead on the NUC |

PostgreSQL is deployed as a Docker container on the NUC alongside Home Assistant. Memory
usage is bounded via `shared_buffers` tuning. The `book_candidates` DuckDB file lives at
`windlass_data/candidates.db` and is written by discovery workers, read by the daily
pre-filter batch job that promotes candidates into the PostgreSQL `books` table.

Large per-book artifacts are stored as flat files under `windlass_data/` rather than as
database blobs, keeping the databases lean and making per-book deletion trivial:

| Artifact | Path | Deleted when |
|---|---|---|
| Enrichment summary (epub prose summary used for Stage 2) | `windlass_data/enrichment/{book_id}.json` | 90 days after book removed from disk |
| Sync artifact (forced-alignment map) | `windlass_data/sync/{book_id}.json` | When book is removed from disk |

### Table Overview

`books` is the canonical library record for every title Windlass knows about. **One row
per work (title) — not per edition.** Multiple MAM torrents may exist for the same work
(different narrators, re-releases, languages); the §5 scoring engine selects the single
best torrent to download. Everything else hangs off `books`.

| Table                 | Contents                                                                                                                                                                                                                                                                     |
| --------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `book_candidates`     | **DuckDB only.** Pre-filter pool — potentially millions of lightweight rows sourced from discovery workers before any enrichment cost is incurred. Columns: `isbn13` (unique dedup key), `title`, `author`, `language_code`, `bisac_codes` (native `TEXT[]` array — GIN-indexed for fast genre lookup), `publication_year`, `word_count` (nullable), `rating_count`, `avg_rating` (stored for display only — never used as a filter), `source`, `discovered_at`, `status` (`candidate` / `promoted` / `rejected`), `rejection_reason` (which pre-filter gate dropped it: `language` / `bisac` / `rating_count` / `word_count`), `books_id` FK (set when promoted to PostgreSQL `books` table). Rejected rows are never deleted — they serve as the permanent dedup blacklist so a re-discovered ISBN is silently skipped. |
| `books`               | Canonical library record for every title Windlass knows about. **One row per work — not per edition.** Library status lifecycle: `pending_s1 → known → epub_pending → enriching → watchlist → monitoring → downloading → seeding → completed` (plus `rejected_s2` terminal state for Stage 1 or Stage 2 failures). Source (`manual_abs`, `windlass_download`, `ai_suggestion`, `freeleech`). Audnexus ASIN, Hardcover ID, ABS item ID. `epub_status` (`searching` / `found` / `not_found`). **`tag_vector sparsevec(500)`** — a single sparse vector storing all tag intensity scores (-100→+100) using a stable application-level index dictionary (e.g. index 0 = `hard_scifi`, index 1 = `space_opera`, …). Sparse format stores only non-zero entries; declaring 500 dimensions provides headroom for future tags without schema migration. S1 tag indices are populated at discovery time; S2 indices (style, arc, narrative) are filled in when Stage 2 enrichment completes. Dot-product queue scoring uses `tag_vector <#> subprofile_vector` — always computed live (sub-5 ms for thousands of rows); no cached score columns. `enrichment_stage` (`discovery` / `post_download_lite` / `post_download_full`). `enrichment_summary_path` (path to `windlass_data/enrichment/{book_id}.json` — raw NLP arrays, smoothed arcs, LIX score, prose summary; null until Stage 2). `reason` (LLM Decide blurb; overwritten on re-evaluation). A book record survives disk deletion. |
| `metadata_cache`      | Read-through cache for external API responses. Keyed by `(source, external_id)` where `source` is `audnexus` or `hardcover` and `external_id` is the ASIN or Hardcover ID. Stores the raw `response_json` and `fetched_at` timestamp. TTL: Audnexus 30 days (stable data), Hardcover 7 days (community reviews change frequently). Eliminates redundant API calls across Stage 1, Stage 2, and all LLM context assembly. |
| `tags`                | Canonical tag registry. `id` (slug), `canonical_name`, `category` (`genre` / `mood` / `tone` / `style` / `arc` / `arc_relation` / `narrative` / `preference` / `content_warning` / `length` / `format` / `protagonist`), `description`, `source` (`audnexus` / `hardcover` / `llm_mint`), `status` (`active` / `deprecated`). Controls the tag vocabulary — see §7.6. |
| `series`              | Series identity and health (Audnexus data, user started/following flags). `engagement_trend_json`: array of `{book_number, rating, completion_ratio, slog_events}` appended after each series book review. Used for series drop-off detection. |
| `torrents`            | File data once a download starts: qBittorrent hash, seed time, HnR status, ratio, disk path. |
| `download_queue`      | Thin table: books actively in the approval/download funnel only. `status` lifecycle: `pending_review → approved → monitoring → downloading`. `priority`: `critical` (series continuation) / `high` (strong profile match, freeleech) / `normal` / `low`. `freeleech_window_end` (nullable — elevates urgency when set). `enrichment_confidence` (float). Row deleted once `books.library_status` advances to `seeding`. |
| `active_queue`        | The 3-slot ABS playlist Windlass manages. `slot` (1–3), `book_id` FK to `books`, `pinned` (bool — user-locked slot; never auto-replaced), `reason` (the Decide call's blurb for this pick), `mood_snapshot_json` (snapshot of `mood_state` at time of selection). Pinned slots survive mood re-evaluations. When a pinned book finishes, the pin is consumed and the slot returns to Windlass control. |
| `reading_ledger`      | One row per listening attempt (supports re-reads). `started_at` (first playback session), `finished_at` (ABS completion webhook), `completion_ratio` (actual calendar days ÷ expected days, computed at finish), `mood_snapshot_json` (mood state at listen-start). Retained permanently after disk deletion. |
| `reviews`             | User feedback rows keyed by ledger entry: completion review, optional midway note, DNF autopsy. Fields: `star_rating` (1–5), `review_text`, `circumplex_pleasure_endemo INTEGER` (1–5, null for midway notes), `circumplex_activeness_endemo INTEGER` (1–5, null for midway notes), `ranking_peers_json` (ordered array of the last 5 book IDs as explicitly placed by the user via drag-and-drop; null for midway notes and when skipped). Retained permanently. |
| `slog_detector`       | Pacing stall detection state per active ledger entry. Purged when the ledger entry is closed (finished or DNF). |
| `series_check_ins`    | Records of the 60–75% series check-in: what was offered, what the user chose. |
| `profile_signals`     | One row per scored dimension. `dimension_type` (`tag` / `author` / `narrator`), `dimension_id` (canonical tag slug or name), `score` (integer -100→+100), `context_id`. Two context types: **circumplex subprofiles** (`circumplex_high_activeness`, `circumplex_low_activeness`, etc.) created when the mood-split gate fires; **taste-cluster subprofiles** (`taste_{genre}_{cluster}`, e.g. `taste_fantasy_grimdark`) created when the taste-cluster split gate fires. `context_id = 'global'` is always present — the universal repository for dimensions not yet statistically proven to vary across contexts, and the cold-start baseline. The Decide call fuses the active subprofile with `global` for any dimension absent from the subprofile. **Author and narrator scores** (`dimension_type: author/narrator`) are stored here but never enter `tag_vector` — they are injected into the Decide prompt as context and used by the Learn call to strengthen correlated style/genre dimensions. See §7.6 for split gate mechanics. |
| `user_constraints`    | Hard veto constraints that apply regardless of profile scores. `constraint_type` (`content_warning` / `format` / `author_block` / `narrator_block`), `dimension_id` (tag slug or name), `reason` (optional user note). Applied as SQL `WHERE` filters **before** any dot-product scoring runs — never stored in `tag_vector` and never mathematically blended with scores. A dealbreaker must never be outweighed by a high genre score. Populated from the same -100 entries the user sets, but enforced as hard filters rather than soft scoring weights. |
| `mood_state`          | Single-row table (replaced on each update). `circumplex_pleasure INTEGER` (1–5, null until first inference or explicit input — see §7.6 for label mapping), `circumplex_activeness INTEGER` (1–5, null until first inference or explicit input). These represent the current Circumplex state anchor: set by explicit grid input (un-decayed, updated only on new input) or by the Mood Inference call. `inferred_modifiers_json` (tag score deltas from inference), `explicit_override_json` (user-set tag modifier deltas from grid-triggered Mood Inference and vibe text; decays at 0.65× per pick, dropped when `\|score\| < 5` — **never** contains raw Circumplex coordinates), `inferred_context` (human-readable explanation shown in Queue View and Panel 6), `computed_at` timestamp. |
| `events`              | Internal audit log. One row per significant system action. `source` (which rule or feature triggered it — e.g. `freeleech_scavenger`, `mood_inference`, `series_continuation`, `stage2_enrichment`), `action` (e.g. `book_grabbed`, `slot_replaced`, `epub_found`), `book_id` (nullable FK), `detail_json` (structured context). Read-only — never modified after insert. **Retention: 90 days rolling.** Visible in the desktop UI Event Log panel. Distinct from `alerts` (which are user-facing and actionable). |
| `alerts`              | Fired alerts. UUID primary key for notification deep-links. Severity, timestamp, triggering event, system state snapshot. **Retention: 30 days rolling.** |
| `playback_sessions`   | One row per play/pause event (or scheduled ABS position poll) per book. `start_time`, `start_position_sec`, `end_time`, `end_position_sec`, `playback_speed` (float — e.g. 1.0, 1.25, 1.5; null if not reported by client), `device_id`, `time_of_day_bucket` (`morning` / `afternoon` / `evening` / `night`), `day_of_week`, `source` (`webhook` / `poll`). Used for Sleep Recovery, slog detection, and mood inference. Retained permanently (required for seasonal pattern queries). |
| `sync_artifacts`      | Metadata row for a book's forced-alignment file. `book_id` FK, `alignment_path` (path to `windlass_data/sync/{book_id}.json`), `state` (`pending_alignment → aligned`). Only present when an epub counterpart exists. The file is deleted with the book; this row is deleted at the same time. |
| `context_chunks`      | Hierarchical Act summaries per book generated JIT as the user progresses. FK to `books`. Stores `act_index`, `plot_advancements`, `character_roster`, and `world_lore` as structured JSON. **Retention: deleted 24 hours after `reading_ledger.finished_at`, or after a "Previously On" recap has been generated — whichever is later.** |

### JIT (Just-In-Time) Context Injection

Each LLM call receives only the data relevant to its task — the full context contract for
each call type is defined in §7.6. The general principle: Windlass queries PostgreSQL for a
small, hyper-relevant payload rather than passing the entire reading history.

### External Meta-Scraping

**Audnexus** (`api.audnex.us`) provides blurbs, series ordering, tags, and release dates.
**Hardcover.app** provides written user reviews and social metrics. Both are bundled into
the RAG payload before any LLM call.

### Database Schemas

#### Table A — `book_candidates` (DuckDB)

```sql
CREATE TABLE book_candidates (
    id               INTEGER PRIMARY KEY,
    isbn13           TEXT UNIQUE,
    isbn10           TEXT,
    openlibrary_key  TEXT,
    title            TEXT NOT NULL,
    author           TEXT NOT NULL,

    -- Pre-filter columns (all indexed)
    language_code    TEXT NOT NULL,
    bisac_codes      TEXT[],          -- native array; GIN-equivalent zone map in DuckDB
    publication_year INTEGER,
    word_count       INTEGER,         -- nullable; unknown books pass through
    rating_count     INTEGER DEFAULT 0,
    avg_rating       REAL,            -- stored for display only; never used as filter

    -- Pipeline state
    source           TEXT NOT NULL,
    discovered_at    TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    status           TEXT NOT NULL DEFAULT 'candidate'
                     CHECK(status IN ('candidate','promoted','rejected')),
    rejection_reason TEXT,            -- 'language' | 'bisac' | 'rating_count' | 'word_count'
    books_id         INTEGER          -- set when promoted to PostgreSQL books table
);

-- Physical sort order — DuckDB uses zone maps; clustering on these columns
-- lets the pre-filter skip entire data blocks
-- ORDER BY (status, language_code, rating_count DESC) at load time
CREATE INDEX idx_candidates_filter ON book_candidates(language_code, status, rating_count);
CREATE UNIQUE INDEX idx_candidates_isbn ON book_candidates(isbn13);
```

**Pre-filter query** (daily batch — millions → hundreds):
```sql
SELECT id FROM book_candidates
WHERE language_code = 'en'
  AND status = 'candidate'
  AND rating_count >= 30
  AND (word_count >= 20000 OR word_count IS NULL)
  AND list_has_any(bisac_codes, ['FIC028000','FIC009000','FIC010000','FIC002000'])
ORDER BY rating_count DESC
LIMIT 500;
```

#### Tables B/C — `books` (PostgreSQL + pgvector)

All downstream tables (`download_queue`, `active_queue`, `reviews`, `reading_ledger`) FK to `books.id`. S1 and S2 enrichment live in the same table — S2 vector indices are 0.0 until Stage 2 completes.

```sql
CREATE EXTENSION IF NOT EXISTS vector;

CREATE TYPE lib_status   AS ENUM ('pending_s1','known','epub_pending','enriching','watchlist',
                                   'monitoring','downloading','seeding','completed','rejected_s2');
CREATE TYPE enrich_stage AS ENUM ('discovery','post_download_lite','post_download_full');
CREATE TYPE epub_state   AS ENUM ('searching','found','not_found');

CREATE TABLE books (
    id               SERIAL PRIMARY KEY,
    candidate_id     INTEGER,          -- FK to DuckDB book_candidates.id (soft reference)

    -- External IDs
    audnexus_asin    TEXT,
    hardcover_id     TEXT,
    abs_item_id      TEXT,

    -- Metadata (populated at Stage 1)
    title            TEXT NOT NULL,
    author           TEXT NOT NULL,
    narrator         TEXT,
    series_id        INTEGER REFERENCES series(id),
    series_position  REAL,
    duration_seconds INTEGER,
    publication_year INTEGER,
    language_code    TEXT DEFAULT 'en',
    hardcover_ratings_count INTEGER,   -- raw count for FairLRM H/T classification

    -- Pipeline state
    library_status   lib_status   NOT NULL DEFAULT 'pending_s1',
    enrichment_stage enrich_stage NOT NULL DEFAULT 'discovery',
    epub_status      epub_state   DEFAULT 'searching',
    source           TEXT,
    enrichment_summary_path TEXT,      -- windlass_data/enrichment/{id}.json; null until S2
    reason           TEXT,             -- LLM Decide blurb; overwritten on re-evaluation
    last_scored_at   TIMESTAMP,

    -- Tag vector: sparsevec(500)
    -- Application-level index dictionary maps tag slug → integer index (0–499).
    -- S1 indices (genre/mood/tone/protagonist/cw/format/length) populated at discovery.
    -- S2 indices (style/arc/arc_relation/narrative) populated when enrichment completes.
    -- New tags are assigned the next unused index — no schema migration required.
    -- Dot-product: ORDER BY tag_vector <#> $subprofile_vector (negative inner product;
    --              ASC order returns highest similarity first).
    tag_vector       sparsevec(500),

    created_at       TIMESTAMP DEFAULT NOW(),
    updated_at       TIMESTAMP DEFAULT NOW()
);

-- Partial indexes — only index rows actively moving through each queue stage
CREATE INDEX idx_books_s1_queue   ON books(id)
    WHERE library_status = 'pending_s1';
CREATE INDEX idx_books_epub_queue ON books(id)
    WHERE library_status = 'known';
CREATE INDEX idx_books_dl_queue   ON books(id)
    WHERE library_status IN ('watchlist','monitoring');
CREATE INDEX idx_books_completed  ON books(id)
    WHERE library_status = 'completed';
CREATE INDEX idx_books_enriching  ON books(enrichment_stage, last_scored_at)
    WHERE enrichment_stage = 'discovery';
```

**Queue sort query** (epub queue — live dot-product, no cached scores):
```sql
SELECT id, title
FROM books
WHERE library_status = 'known'
  AND tag_vector IS NOT NULL
ORDER BY tag_vector <#> $subprofile_vector
LIMIT 50;
```

**Stratified portfolio pass** (run once per subprofile, results merged by caller):
```sql
-- Called N times with different $subprofile_vector and $limit per subprofile bucket
SELECT id FROM books
WHERE library_status IN ('watchlist','monitoring')
  AND tag_vector IS NOT NULL
ORDER BY tag_vector <#> $subprofile_vector
LIMIT $limit;
```

#### Tag Vector Index Dictionary

The application maintains a YAML/TOML dictionary (`windlass_data/tag_index.toml`) mapping
each tag slug to its fixed vector index. Indices are append-only — never reused or
reordered. Current allocation bands:

| Indices | Category |
|---|---|
| 0–29 | Genre |
| 30–49 | Mood + Tone |
| 50–69 | Protagonist + Content Warning + Format + Length |
| 70–99 | Narrative (POV, tense, structure) |
| 100–149 | Arc + Arc Relation |
| 150–219 | Style (algorithmic: linguistic_complexity, dialog_density, …) |
| 220–289 | Style (LLM: prose_ornamentation, cognitive_load, lore_density, …) |
| 290–499 | Reserved for future tags |

---



Windlass is engineered to perfectly emulate a well-behaved MAM user, actively protecting
the account from automated bans.

- **Account Keep-Alive (Rule 1.6):** The core heartbeat routinely hits the MAM homepage to
  prevent the account from being disabled for inactivity.
- **MAM Connectability Heartbeat:** Periodically checks whether qBittorrent is listed as
  connectable on MAM (i.e., the tracker can reach qBit for incoming connections). Alerts if
  `NotConnectable`; distinguishes network failure (`Unreachable`) from a genuine
  connectivity problem.
- **Unsatisfied Quota & Queue Limits Manager (Rule 2.8):** The core tracks the user's
  Class Limit for unsatisfied torrents (e.g., 50 for User, 100 for Power User). It
  continuously polls qBittorrent for torrents that have _not yet_ reached 72 hours of seed
  time. If the active unsatisfied count approaches the limit, all new automated downloads
  are paused. Furthermore, Windlass monitors qBittorrent's global maximum active,
  downloading, and seeding limits. To prevent H&R violations caused by qBittorrent parking
  torrents when limits are reached, Windlass actively orchestrates the queue, temporarily
  pausing fully satisfied torrents to guarantee unsatisfied torrents remain actively seeding.
- **MAM HnR Compliance Monitor (Rules 2.5 & 2.7):**
  - _No Partials:_ Forces qBittorrent to download 100% of a torrent's files. MAM's rules prohibit stopping a download partway through and keeping only some files — every file in the torrent must be downloaded in full.
  - _HnR Lock:_ Auto-eviction is mathematically prohibited from deleting any torrent that
    has downloaded data until `seed_time ≥ 72 hours`.
  - _Safe Deletion:_ Stalled or dead torrents are only automatically deleted and
    blacklisted if they have downloaded exactly 0 bytes.
- **The Vault Guardian:** Windlass monitors the MAM Millionaires Vault. When a new vault
  cycle reaches 20,000,000 BP, Windlass checks if the user's global ratio is ≥ 1.05. If
  eligible, it fires an `Action` (`High`) notification: _"The Millionaires Vault is open. Click here to
  donate 2,000 BP and secure your Freeleech Wedges."_ To strictly comply with MAM's rules
  against automated scripts, the system will never execute the donation via headless
  background scripts; it requires the user's explicit click via the notification deep-link.
  Freeleech Wedges extend freeleech-like benefits to individual non-freeleech books,
  complementing the Freeleech Scavenger (§7.4) for high-priority acquisitions — series
  continuations and specific titles — where a freeleech window is not available.
- **qBittorrent Configuration Validator & Auto-Tuner:** Windlass does not just send
  torrents to the client; it actively manages the client's internal configuration via the
  WebAPI to ensure optimal throughput and strict tracker compliance. Enforcement is tiered
  by risk level:
  - _Port Forwarding:_ Always silently auto-updated as part of the core VPN sync loop. No
    notification required.
  - _Privacy Settings — DHT, PeX, Local Peer Discovery (Rule 6.1):_ These carry an
    immediate ban risk on private trackers. If any are detected as enabled, Windlass
    auto-reverts them immediately and fires an `Alert` (`Critical`) notification: _"DHT was re-enabled in
    qBittorrent — I've corrected it."_ The intervention is logged. This does not wait for
    user confirmation.
  - _Queue Limits — `max_active_downloads`, `max_active_uploads`, `max_active_torrents`:_
    Windlass first attempts to work around restrictive limits via queue orchestration
    (pausing satisfied torrents, reordering priorities) without touching qBittorrent's
    config. If the limits are so low that orchestration cannot prevent an H&R violation,
    Windlass escalates: it auto-corrects the setting and fires an `Action` (`Critical`) notification
    explaining exactly what was changed and why — _"Your max active torrents was set to 5.
    With 12 unsatisfied torrents, H&R violations were unavoidable. I've raised it to 25."_
- **Upload Health Math (Rule 1.4):** Enforced before queueing new downloads:
  - Global Ratio must remain ≥ 2.0 (well above the 1.0 minimum).
  - Upload credit buffer must remain ≥ 25 GB.
- **Disk Space Management:** Monitors the mounted volume continuously. Disk management
  operates at two levels:

  _Automatic (silent):_ If free space drops below a hard floor threshold, Windlass
  immediately auto-evicts the lowest-value HnR-satisfied torrents (completed + low rating
  - longest time since last play) without user input. This is the emergency brake.

  _User-directed (proactive):_ When projected free space over the next month (based on
  expected downloads) drops below a configurable buffer, Windlass fires an `Action` (`Normal`)
  notification with a deep-link to a deletion suggestion card:

  > _"Windlass has 47 GB free. To comfortably fit this month's queue, we need ~80 GB._
  > _Here are the best candidates to remove — confirm to free the space."_

  The suggestion list is ranked by deletion value:
  1. Completed + low rating (≤ 2★) + HnR satisfied
  2. DNF + HnR satisfied
  3. Completed + high rating but long since listened + HnR satisfied (user can un-tick these)
  4. Unstarted + long wait + low AI score

  HnR-unsatisfied torrents are never shown as deletion candidates. The user reviews the
  list, un-ticks anything they want to keep, and confirms. Ratings and listening history
  are retained in `reading_ledger` after deletion so the data is never lost.

---

## 5. Search & Scoring Engine

Windlass evaluates raw tracker search results to automatically select the optimal release.
All scoring data comes directly from the MAM search API response fields.

- **Built-in Base Factors (not user-editable):** Applied before Custom Format Weight rules.
  These use MAM API fields directly:

  | Factor | Field | Effect |
  |---|---|---|
  | Seeder count | `seeders` | `+min(seeders, 20)` — rewards well-seeded torrents up to a cap |
  | Community trust | `times_completed` | `+min(times_completed / 50, 10)` — heavily snatched = proven |
  | Already snatched | `my_snatched` | **Auto-skip** — never re-download a previously snatched torrent |
  | Collection detected | `numfiles > 20` | **Auto-skip** — multi-book collections cannot be auto-processed (see below) |

- **Custom Format Weights (Radarr-Style):** User-defined score adjustments matched against
  torrent title, tags, narrator name, uploader name, and format fields. Rules are evaluated
  in priority order and combined additively with the base score. This is the primary
  mechanism by which the auto-grabber selects the correct release — narrator preferences,
  bitrate preferences, and uploader trust defined here directly control what is snatched.

  **Default rules (pre-installed, user-editable):**

  | Rule | Field | Score | Rationale |
  |---|---|---|---|
  | Format: `m4b` | `filetypes` | `+0` | Baseline — preferred format |
  | Format: `mp3`, `m4a`, `ogg`, other | `filetypes` | `−100` | Excluded by default |
  | Tags contain `Abridged` | `tags` | `−100` | Condensed content — missing plotlines, not the full work |
  | Tags contain `Unabridged` | `tags` | `+20` | Mild confirmation of full content; abridged already excluded by the rule above |
  | Bitrate ≥ 128 Kbps | `tags` | `+30` | Good audio quality |
  | Bitrate < 64 Kbps | `tags` | `−50` | Poor audio quality |
  | Language: `English` | `lang_code` | `+50` | Prefer English editions by default |
  | Language: not `English` | `lang_code` | `−50` | Deprioritise non-English editions |

  Users add narrator and uploader rules (e.g. `+80` for narrator "Ray Porter", `+40` for
  uploader "trusted_user"). These are the primary mechanism for resolving between multiple
  editions of the same work. Scores are integers on the same **−100 → +100 scale** used
  throughout Windlass.

  A candidate whose total score (base + format weights) falls below **0** is not
  auto-grabbed. When multiple torrents match the same work, the highest-scoring one is
  selected — only one torrent is ever downloaded per work.

- **Collection Handling:** Torrents with `numfiles > 20` are skipped by automated
  discovery. However, any MAM torrent URL pasted into the Universal Input Box is accepted
  regardless — collections can be added manually. When a collection is added manually it
  surfaces in Panel 1 with an "Unknown contents — manual review required" flag; the user
  must review and approve before download begins.

- **MAM New Additions Monitor:** Windlass polls MAM's audiobook catalogue sorted by date
  added at regular intervals. New entries are cross-referenced against `books` (skip
  already-known titles) and passed through Stage 1 enrichment (§7.1). Strong profile
  matches enter the monitoring queue automatically; borderline matches surface in Panel 1.
- **Hardcover Discovery:** Windlass queries the Hardcover GraphQL API on a daily background
  schedule across four signals: trending this month, upcoming releases, popular curated
  lists, and mood/tag browsing filtered to the user's top-scored genres and moods. Candidates
  are deduplicated against `books`, checked for MAM availability, and passed through Stage 1
  enrichment. This surfaces books the user has never heard of but would love.

---

## 6. Media & Series Intelligence

- **Audiobookshelf (ABS) Sync:** Continuously polls ABS for playback progress and triggers
  library scans upon completed downloads.
- **Pipeline Depth Management:** Windlass continuously tracks pipeline depth — the total
  hours of approved, ready-to-play content across the Download Queue and the In Library
  (Unread) panel. This is the primary metric governing acquisition aggressiveness:

  | Depth                 | State   | Acquisition behaviour                                             |
  | --------------------- | ------- | ----------------------------------------------------------------- |
  | < 1 week of listening | Thin    | Aggressive — push curated recs                                    |
  | 1–4 weeks             | Healthy | Normal — curated recommendations only                             |
  | > 4 weeks             | Deep    | Conservative — only exceptional scores                            |

  *"1 week of listening" is computed from the user's rolling average listening velocity.*
  *Freeleech grabs are not gated by pipeline depth — see §7.4.*

- **Predictive Series Syncing:** For a series the user has already started and rated
  positively, the next entry is automatically queued and downloaded in the background with
  no approval required. Before queuing, the **Novella Navigator** (see below) is consulted
  to determine the correct next entry. The check-in timing is **dynamic** — it fires when
  the estimated time remaining in the current book equals the time needed to acquire the
  next book plus a safety buffer. In practice:
  - Next book already on disk → check-in at ~1 day estimated remaining (confirmation only)
  - Next book needs downloading → check-in at ~2 days estimated remaining
  - Next book availability unknown → check-in at ~3 days estimated remaining
  - Hard limits: never before 30% remaining; never after 90% remaining

  The check-in is an `Action` (`High`) notification deep-linking to a card offering:
  **Continue series** · **Pause series** · **Skip to Book N+2** · **Find me something else**

  If the user ignores the check-in, Windlass assumes continuation and ensures the next book
  is ready. The check-in is only decision-critical when the next book has not yet been
  pre-fetched.

- **Series Drop-Off Detection:** After every series book review, Windlass appends a record
  to `series.engagement_trend_json` and evaluates the trend. If two or more of the
  following signals are present, auto-queuing pauses and a check-in fires:
  - Ratings declining across successive books (e.g. 5★ → 4★ → 3★)
  - Completion ratios increasing (books taking longer than expected relative to the user's
    pace)
  - Slog detector triggered on a series book
  - DNF on a series book

  The check-in notification: *"Your engagement with [Series] seems to be declining. Keep
  auto-queuing?"* **Keep going** · **Pause series** · **Tell me what's ahead** (triggers
  the Series Health Forecaster on the remaining books).

- **The Novella Navigator (Smart Reading Order):** Determines whether fractional series
  entries (e.g., Book 1.5) are essential lore or skippable filler, ensuring the auto-queue
  always presents books in the optimal order.

  Whenever Predictive Series Syncing identifies a fractional entry between two whole-number
  books, it pauses the auto-queue and consults the Novella Navigator. The LLM analyses
  aggregated reader reviews and Audnexus series metadata to classify the entry:
  - _Essential:_ Contains plot or character development that affects later books. The entry
    is inserted into the queue before the next whole-number book; the series check-in
    surfaces a note explaining why.
  - _Recommended:_ Adds depth but is not required for plot continuity. The series check-in
    offers it as an optional choice.
  - _Skippable:_ Filler or purely supplementary. Silently bypassed and logged in the series
    record so the classification is not repeated.

  This ensures the auto-queue never skips a novella that redefines a character, nor stalls
  momentum with unnecessary filler.

- **Finish-Book Notification:** When ABS marks a book complete, Windlass fires an `Action`
  (`High`) notification:

  > _"You finished [Book Title]. How was it?"_
  > **Rate it** · **Skip [Next Book]**

  Tapping **Rate it** opens the Universal Review Component. The next book in the Active
  Queue is already playing — the user does not need to interact at all for listening to
  continue. **Skip [Next Book]** removes the next queued title and triggers the LLM to
  select a replacement from downloaded books. Pending reviews can always be completed later
  from the Reading Ledger.

- **Active Queue:** A short ABS playlist (target depth: 3 books) maintained automatically
  by Windlass from books that are already downloaded, Stage 2 enriched, and on disk. It is
  entirely separate from the download pipeline. Plappa plays the playlist continuously; no
  interaction is required between books. The full queue model is specified in §7.7.

  **Promotion logic:**
  1. *Active series continuation:* the next book in an ongoing series is promoted silently
     with no notification.
  2. *All other slots:* Windlass runs a two-stage pick. First, a SQL dot-product
     pre-score eliminates near-dealbreaker tag mismatches and ranks the eligible pool
     using `books.tag_vector <#> (profile_signals + mood_state modifiers)`. The top 10
     candidates go to a Decide call (§7.6), which reasons about contrast, narrative variety,
     and mood fit — returning a ranked list with a `reason` per pick. The top pick is
     promoted; its `reason` becomes the notification blurb.

  **Non-series additions** fire an `Action` (`High`) notification:
  > _"[Book] has been added to your queue."_
  > Blurb (the Decide call's `reason` field, generated against current mood)
  > **Keep it** · **Swap out** · **Already Read** · **Change mood**

  Ignoring the notification keeps the book in the queue. **Already Read** opens the
  Universal Review Component inline, logs the entry to the `reading_ledger`, and
  immediately triggers a replacement pick. **Change mood** opens the hybrid mood input
  (Circumplex grid + optional vibe text) and re-runs the Decide call on submission.

  **Long-awaited series arrival:** when a new entry becomes available for a series where
  the user has read all existing books and the next was previously unreleased, Windlass
  fires an `Action` (`High`) notification:
  > _"[Book N] in [Series] is now available and has been queued."_
  > **Get a recap** · **Skip**

  This is the only series-related notification that fires proactively — standard series
  continuations remain silent.

- **Release Calendar:** Tracks upcoming release dates for incomplete series via Audnexus
  and displays them in the Action Center's "Upcoming in Series" panel.

---

## 7. The AI Librarian Engine

### 7.1 Discovery Pipeline & Pre-Download Intelligence

#### Discovery Sources

Windlass pulls candidates from multiple sources. All feed into `book_candidates` (DuckDB)
and share the same pre-filter → Stage 1 → epub queue pipeline.

| Source | Mechanism | Cadence |
|---|---|---|
| **User-initiated** | Universal Input Box (see below) | On demand |
| **MAM new additions** | Poll MAM audiobook catalogue sorted by date added | Hourly |
| **Hardcover trending** | GraphQL API: trending, upcoming, popular, anticipated lists | Daily |
| **Hardcover mood/tag browse** | GraphQL API: filtered to user's top-scored genres and moods | Daily |
| **NYT Books API** | Bestseller lists — Fiction, Hardcover Fiction, Audio Fiction | Weekly |
| **iTunes Search API** | Top audiobooks chart (no key required) | Daily |
| **bibliotek.dk / DBC** | Danish national library catalogue — useful for translated fiction | Weekly |
| **Open Library bulk dump** | Periodic ingest of Open Library title metadata for pre-filter | Monthly |
| **AudioFile / Kirkus / Locus RSS** | Critic review feeds for genre fiction — surfaces pre-release titles | Daily |
| **Series continuation** | Predictive Series Syncing (§6) — bypasses pre-filter, goes direct to `priority: critical` | Event-driven |

Freeleech candidates have their own pipeline (§7.4) but share the same monitoring queue.

#### Universal Input Box

> A single entry point for all user-initiated book discovery — search, paste, or describe.

The Action Center header contains a single universal input field. The system auto-detects
intent:

- **Direct MAM torrent URL:** Queued immediately with no LLM call. The book card is created
  from Audnexus metadata and placed directly in the monitoring queue at `priority: high`.
- **Audible or ABS URL:** Metadata is resolved, Series Health is run if it is Book 1 in a
  series. Enters Stage 1 enrichment, then appears in Panel 1 for approval.
- **Author / title search:** Queries MAM, returns ranked results. User picks one — same
  flow as URL paste.
- **Vibe query** (e.g. _"short snarky sci-fi under 10 hours"_): see below.

While the LLM is processing, the card shows an "Analysing…" state and populates in place.

#### Vibe Query Translation

When the user types a free-text vibe, Windlass runs a two-step translation before
searching:

**Step 1 — LLM translates vibe to structured intent:**
```json
{
  "tag_modifiers": { "short": 80, "dry_wit": 70, "hard_scifi": 50 },
  "mam_search_terms": ["science fiction", "humor"],
  "hardcover_mood_tags": ["funny", "adventurous"],
  "max_runtime_hours": 10
}
```

**Step 2 — Query MAM and Hardcover** using the derived terms. Score all candidates against
`profile_signals + vibe tag_modifiers` using the dot-product pre-score. Top results surface
in Panel 1. The vibe modifiers are a one-shot strong override — they expire once the user
acts (approves or rejects a result).

#### Pre-filter Layer

Before any enrichment cost is incurred, every discovered candidate passes through a
lightweight DuckDB query that eliminates obviously irrelevant titles. This gate runs as a
daily batch job, promoting survivors into the PostgreSQL `books` table as `library_status:
known`.

**Gates (applied in order — first failure sets `rejection_reason` and marks `rejected`):**

| Gate | Condition | Notes |
|---|---|---|
| Language | `language_code = 'en'` | Deterministic; publisher-assigned |
| Genre | `bisac_codes` overlaps fiction subcategory list | Excludes non-fiction, poetry, reference |
| Popularity | `rating_count >= 30` | Ensures statistical signal; does **not** filter on `avg_rating` |
| Length | `word_count >= 20000 OR word_count IS NULL` | Excludes short stories; unknown passes through |

`avg_rating` is stored but **never used as a filter gate** — community ratings cluster
tightly around 4.0 (no discriminating power) and niche books are systematically
underrated by mainstream audiences.

#### Book Lifecycle

```
book_candidates (DuckDB)
   candidate ──[pre-filter]──► promoted
                              └──► rejected (permanent blacklist; dedup shield)

books (PostgreSQL)
   pending_s1   ← promoted from DuckDB; awaiting Stage 1 scoring
   known        ← passed Stage 1; in epub queue
   epub_pending ← epub fetch in progress
   enriching    ← Stage 2 running on worker
   watchlist    ← S2 done; not yet on MAM
   monitoring   ← on MAM; awaiting download conditions
   downloading
   seeding
   completed
   rejected_s2  ← terminal; failed Stage 1 or Stage 2 score threshold
```



#### Stage 0.5 — Free API Enrichment & Structured Scoring

Runs on every candidate that passes ingestion. No LLM calls — uses only free public API data. Purpose: eliminate books with no profile overlap before paying for Stage 1 LLM calls.

**Input:**
- Audnexus: genre tags, series info (position, completion status), narrator, publisher
- Hardcover: mood tags, community rating count, "related titles" list
- Open Library: additional subject classifications, related authors

**Tag normalization:** Community tags are noisy and fragmented (e.g. `ya-dystopian`, `teen-dystopian`, `antiutopian` all map to one canonical slug). All API tags are normalized to canonical Windlass slugs before scoring. This is the primary value of Stage 0.5 — structured, normalized tags enable a reliable dot-product even without LLM interpretation.

**Scoring:** Dot-product using the normalized API tags against the Stratified Portfolio (all subprofiles × historical frequency weights). Only tag indices populated by API data are used — no imputation for missing dimensions.

**Gate:** Score below a low floor → discard (DuckDB `status: rejected`). The bar is intentionally low — Stage 0.5 only eliminates books with zero profile relevance, not borderline cases. Those go to Stage 1.

**Output:** ~50–100 survivors/day promoted to PostgreSQL `library_status: pending_s1`.

**Rate limiting:** Stage 1 is capped at ~20–50 LLM calls/day. Stage 0.5 feeds a queue; the Stage 1 worker drains it at the configured rate.

#### Stage 1 Enrichment (Discovery-Time)

Runs on every candidate promoted from the pre-filter. Fast and cheap — many books pass
through here.

**Input:**
- Already-normalized Stage 0.5 API tags (no re-fetch needed — cached in DuckDB)
- Book description
- Up to 5 Hardcover community review excerpts
- Current Stratified Portfolio subprofile vectors (for scoring output)

**What the LLM adds beyond Stage 0.5 API tags:**
- `tone:*` — banter_heavy, dry_wit, satirical, slow_burn (APIs never tag these)
- `protagonist:*` — morally_grey, found_family, ensemble_cast
- Content warnings with accurate intensity scores
- Format/length verification and correction of noisy API data
- Preliminary `style:*` hints (low confidence — full style analysis is Stage 2)
- Cross-genre signal that API subject codes miss (e.g. "literary sci-fi")

**Output (written into `books.tag_vector` S1 index band):**
- Full S1 tag scores (-100→+100) for genre / mood / tone / protagonist / content_warning / format / length
- `enrichment_stage: discovery`, `enrichment_confidence: low/medium`
- Stratified Portfolio re-score to determine queue placement

Strong matches (above the **Acquisition Confidence Threshold** — configurable with a
sensible default, adjustable in the Action Center settings) auto-enter the epub queue as
`library_status: known`. Borderline matches surface in Panel 1 for user review.
Non-matches advance to `rejected_s2` — the `books` record is retained so the same title
is not re-evaluated on the next poll.

**Discovery dispatch table:**

| Score vs threshold | Action |
|---|---|
| ≥ threshold | Auto-approved → `known` (epub queue), no notification |
| 50–threshold | Shown in Panel 1 (Suggested Next Listens) for user approval |
| < 50 | `rejected_s2`; `books` record retained as dedup shield |

*During the cold-start period (fewer than 20 reviewed books in `reading_ledger`), the
threshold is automatically raised so that more candidates surface in Panel 1 rather than
auto-grabbing — profile confidence is too low to trust silent auto-approval.*

#### Epub-First Evaluation Pipeline

Books that pass Stage 1 enter the **epub queue** (`library_status: known`). Windlass
fetches the epub (2–5 MB) as a pre-evaluation tool **before** committing to the full
audiobook download (400 MB–2 GB). This is the entry point for Stage 2 enrichment.

**Epub sources (in priority order):**
1. MAM — searched simultaneously with the audiobook on approval
2. Open Library — public domain and community-contributed epubs
3. Anna's Archive — investigated; policy and reliability TBD

`books.epub_status` is updated to `found` or `not_found`. Epub absence is never a
blocker — Stage 2 runs Path B (lite enrichment from reviews and metadata) when no epub
exists.

**Stage 2 rate limit:** a maximum of **5 epub enrichment jobs per day** are dispatched to
the worker node. This prevents the epub queue from draining faster than the audiobook
download pipeline can consume, and keeps worker GPU usage predictable.

**Epub deletion:** the epub file is deleted after Stage 2 completes — all signal is
captured in `windlass_data/enrichment/{book_id}.json`. Storage cost is transient.

#### Stratified Portfolio — Universal Queue Mechanism

The Stratified Portfolio is the queue management mechanism for **all upstream stages** — epub queue sort, download queue sort, Stage 0.5 acquisition, and Stage 1 acquisition gating. It is never used at Decide time (which uses current mood directly).

**Why not current mood for upstream stages:** A book acquired based on today's mood will be consumed weeks or months later when mood will be different. Using current mood at acquisition causes overspecialization — the buffer fills with books that mismatch when mood shifts.

**Why not max-score-across-subprofiles:** Ignores probability — fills the buffer with books for a 5%-frequency mood state at the expense of the 60%-frequency baseline state.

**Why not blended/averaged scores:** Destroys polarization — a book scoring +100 in one subprofile and -100 in another averages to 0, losing to a mediocre book that safely scores +20 everywhere.

**Algorithm (runs on every profile update and at each acquisition decision):**
1. **Compute subprofile frequencies** — query `reading_ledger` history to derive the probability distribution across ALL active subprofiles: circumplex subprofiles (from `mood_snapshot_json`) AND taste-cluster subprofiles (from the secondary tags of rated books). Example: 40% `taste_fantasy_grimdark`, 25% `taste_fantasy_romantasy`, 20% `circumplex_low_activeness`, 15% `circumplex_high_activeness`.
2. **Allocate slots proportionally** — partition the available queue slots or acquisition budget by frequency weight.
3. **Score each partition with its pure isolated subprofile** — run entirely separate `ORDER BY tag_vector <#> $subprofile_vector LIMIT $slots` passes, one per partition. Vectors are never blended.
4. **Apply `user_constraints` as a pre-filter** — SQL `WHERE` filters excluding hard-vetoed books before any subprofile scoring.
5. **Merge** the ranked partition results into the final queue order.

The epub queue (`library_status: known`), download queue (`watchlist`/`monitoring`), and Stage 1 acquisition gate all use this algorithm.

#### The Monitoring Queue

Approved books sit in `download_queue` at `status: monitoring` until Windlass has
sufficient resources to download them. The drain loop evaluates:

1. **Disk space floor** not breached
2. **MAM ratio** healthy — or book is freeleech (`freeleech_window_end` set)
3. **Pipeline depth** not already deep (freeleech bypasses this check)
4. **Priority order:** `critical` (series) → `high` (strong match / freeleech) →
   `normal` → `low`
5. **Stratified portfolio sort** (§ above) applied within each priority tier


When resources are available, the highest-priority `monitoring` book advances to
`status: downloading`. Multiple downloads can run concurrently subject to qBittorrent
limits managed by the MAM Compliance layer (§4).

#### The "Series Health & Slog" Forecaster

> Protects the user from investing time in dead, meandering, or genre-shifting series.

**Execution:** When a Book 1 URL is pasted, the shell fetches metadata for Book 1, the
middle book, and the latest published book. The LLM analyses aggregated reviews for the
entire series and returns a `SeriesHealthReport` JSON.

**UI Integration:**

- Alerts if the series is incomplete and abandoned _(anti-Name of the Wind protocol)_
- Flags "Authorial Drift" if later books radically shift tone or genre
- Generates a visual "Pacing Map," highlighting middle books in yellow or red if critical
  consensus deems them a slog

#### The "Sell It To Me" Custom Pitch

> Replaces generic publisher blurbs with personalised, mood-aware justifications.

**Timing:** Pitches are the `reason` field from the Decide call (§7.6), generated
**just-in-time at the moment of decision** — never pre-stored and served cold. A pitch
generated weeks before the user encounters a book does not account for their current mood,
what they just finished, or how much energy they have.

**Context injected at generation time:**

- The last 1–2 books the user finished and their ratings
- Current `mood_state` inferred context
- Time of day and approximate season
- How long the book has been waiting in the queue

**Delivery:** Pitches appear in notification cards (series check-ins, recommendations,
freeleech alerts) and in individual book cards when opened in the Action Center. They are
_not_ shown in the queue list view — that surface is for pipeline management, not decisions.

---

### 7.2 Active Listening Support

#### The Glossary Generator

> An on-demand, spoiler-free cheat sheet for dense sci-fi/fantasy world-building.

**Execution:** When a user is confused by factions or physics (e.g., in _Blindsight_), they
click "Generate Glossary." The LLM generates a structured Dramatis Personae and term
glossary barring any plot points beyond the user's current position. When a sync artifact exists (§8.2), the text payload is truncated at the user's exact
audio timestamp for a precise, paragraph-level spoiler boundary. Without a sync artifact
but with an epub, truncation falls back to chapter-level granularity. Without an epub,
this feature is unavailable (see §8 feature tier table).

#### "Previously On…" Series Recaps

> Refreshes the user's memory when starting a sequel after a long real-world gap.

**Execution:** When Windlass queues the next book in a series, it triggers a background job
to summarise the previous books. The LLM writes a punchy, 3-paragraph recap tailored to the
user's preferred tropes (e.g., focusing on political maneuvering).

**Delivery:** The recap is displayed as a card in the Action Center when book N+1 is
queued, and an `Action` (`High`) notification is fired with a deep-link. It is _not_
injected into ABS metadata — the recap lives in Windlass' own domain and can be
regenerated or dismissed at any time. Availability depends on epub status; see §8 feature
tier table for degraded-mode behaviour.

---

### 7.3 Post-Listening & Recovery

#### The DNF "Bailout" Protocol

> Automated momentum recovery after abandoning a book.

**Execution:** Triggered by an ABS webhook when a book is marked DNF. Windlass immediately
presents the Universal Review Component (star rating + _"What went wrong?"_) via an
`Action` (`High`) notification deep-link. The review is captured first — it feeds Learn
and Mood Inference regardless of what the user chooses next.

**What's next? (three options):**

After submitting the review, the user is presented with three choices:

| Option | Effect |
|---|---|
| **Skip this book** | Slot 1 vacated, filled by a normal Decide call against `profile_signals` + updated `mood_state`. Mild contrast context applied. |
| **Fresh start — replace all 3** | Full queue reset. "User requested fresh start after DNF" is passed as an explicit strong signal into the Mood Inference call — a clearer mood-shift indicator than inferred signals alone. All 3 slots refilled via a single Decide call with heavy contrast context. |
| **Let me guide you** | Opens the Build My Queue wizard (§7.7). Queue is left untouched until the wizard completes. |

The system never hard-codes what a good palate cleanser is — all replacement picks are
made by the Decide call using the profile, updated mood, and DNF contrast context. A user
who DNFs a grimdark epic and loves cozy mysteries will get a different replacement than one
who DNFs a slow romance and prefers hard sci-fi.

#### The Universal Review Component

> A standardized, single-interface review system used across all interactions to ensure a
> consistent user experience and clean data ingestion.

**Execution:** Whether a user finishes a book, triggers a DNF, completes the onboarding
wizard, or marks a suggested book as "Already Read," they are presented with the exact same
UI card.

- **The Interface:** Four sections on a single card:
  1. **Absolute rating:** A standard 1–5 star rating scale.
  2. **EndEmo grid:** The same Circumplex 5×5 grid used for "Change mood" — tapped immediately after finishing or DNF'ing to capture the *End Emotion* (the affective state the book induced at its conclusion). Stored in `reviews` as `circumplex_pleasure_endemo` / `circumplex_activeness_endemo`. Fed into the Learn call to correlate which literary tags produce which emotional outcomes.
  3. **Free text:** A box prompted with _"What did you think?"_ (or _"What went wrong?"_ during a Bailout).
  4. **Relative ranking:** A vertical drag-and-drop list of the last 5 finished/DNF books, ordered from most to least preferred based on prior rankings. The new book appears as a floating item at the edge; the user drags it to its correct relative position among the five. Powered by `@dnd-kit/core` (already used in Active Queue Manager). The resulting explicit order is saved as `ranking_peers_json` in `reviews` and consumed by the Learn call as ground-truth pairwise preferences. Because Windlass is a single-user personal tool, this minor friction is acceptable and produces substantially higher-fidelity profile signals than backend-inferred pairs alone.
- **The Pipeline:** The raw text and rating are saved permanently to the `reviews` and
  `reading_ledger` tables. A Learn call (§7.6) ingests this payload alongside the book's
  tags, the current mood snapshot, and recent reading history to produce a calibrated delta
  on the user's `profile_signals`. For finished or DNF'd books, submitting the review
  seamlessly transitions the user to the "what's next" suggestion or Bailout protocol. If
  pipeline depth is below the healthy threshold after a book is marked complete, acquisition
  aggressiveness increases immediately.

#### The Listening Velocity Monitor (The "Slog Detector")

> Proactively detects waning interest based on listening habits — before an official DNF.

**Execution:** The shell polls the ABS API daily to calculate "Listening Velocity" (average
minutes per day per book). If velocity on a specific book drops significantly below the
user's baseline for 3+ consecutive days, the core flags a `Pacing_Stall` state.

**LLM Magic:** Windlass fires an `Action` (`Normal`) notification with a deep-link to a UI card:
_"Your listening pace on [Book Title] has dropped by 80%. Is it dragging?"_ The card
presents three options:

| Option                            | Meaning             | System behaviour                                                        |
| --------------------------------- | ------------------- | ----------------------------------------------------------------------- |
| "See what's ahead (spoiler-free)" | Evaluating the book | LLM assesses upcoming pacing and advises                                |
| "Trigger Bailout Protocol"        | Done with this book | Mark DNF, find a palate cleanser                                        |
| "I'm just busy right now"         | Life, not the book  | Snooze the detector for this book for 5 days; no profile weight changes |

After a "just busy" response, the detector will not re-fire for that book for 5 days.

---

### 7.7 Active Queue & Queue View

> The primary listening experience. Windlass maintains a 3-slot queue of the right books
> for the user's current mood — silently, continuously, without requiring interaction.

#### The 3-Slot Model

Windlass always tries to keep all 3 slots filled from the downloaded + Stage 2 enriched
pool. Selection uses a two-stage pick: SQL dot-product pre-score → Decide call against
`profile_signals` + `mood_state`. The `reason` from the Decide call is stored in
`active_queue.reason` and shown as the blurb on the card.

**Slot 1** (currently playing or immediately up next) is **never interrupted**. Windlass
will not swap slot 1 even if mood changes significantly.

**Slots 2 & 3** are re-evaluated silently any time `mood_state` is updated — after every
Learn call, every Mood Inference, or an explicit vibe query. If the re-evaluation produces
a better fit, the slot is quietly replaced.

#### Mood Context Display

The Queue View displays the current `mood_state.inferred_context` string at the top of the
screen (e.g. *"Detecting a high-energy week — lighter, faster books prioritised"*). This
gives the user full transparency into why Windlass picked these books. A **"Change mood"**
button opens the hybrid mood input directly from this display:

- **Circumplex grid (required):** a 5×5 grid with Pleasure (P1 very displeased → P5 very
  pleased) on one axis and Activeness (A1 very inactive → A5 very active) on the other.
  Tapping a cell immediately captures the coordinate anchor, stores it in
  `mood_state.circumplex_pleasure/activeness`, and triggers a fresh Mood Inference call.
- **Vibe text box (optional):** free-text input processed as a vibe query. The LLM
  translates context (e.g. *"exhausted from travelling"*) into tag modifier deltas and also
  detects life-context cues — if the negative affect is clearly external (e.g. work stress),
  the current book's tags are not penalised. Tag deltas are stored in
  `mood_state.explicit_override_json` with the standard 0.65× decay.

#### Slot Pinning — User Override

The user can manually pin any downloaded+enriched book to a slot. Pinned slots:
- Show a 📌 indicator on the card
- Are never touched by automatic mood re-evaluations
- Survive Mood Inference updates
- Pin is consumed when the book finishes — the slot returns to Windlass control

To pin a book, the user long-presses a card in the Queue View and selects a target slot,
or manually adds a specific book from the downloaded pool via "Add to queue."

#### Build My Queue Wizard

Accessed via the **"Build My Queue"** button in the Queue View, or from the DNF screen's
"Let me guide you" option. This is the user-facing escape hatch for taking full control
of what's next.

1. **Vibe query (optional):** Free-text input — *"something short and funny"*, *"a cozy
   mystery for a rainy evening"*. The LLM translates this into temporary tag modifiers.
   This does **not** permanently update `mood_state`.
2. **Candidate cards:** Windlass generates 5–6 candidates from the downloaded+enriched
   pool, scored against `profile_signals` + vibe modifiers. Each card shows the AI blurb
   and tag scores.
3. **User selects:** User approves and reorders picks to fill desired slots, or hits
   **"Accept Windlass's picks"** to apply the top results. Selected books can be pinned
   from this view.

#### Download Gap Feedback Loop

After every Decide call for queue filling, Windlass checks whether the downloaded pool
contains enough well-matched books for the current `profile_signals` + `mood_state`. If
the pool is thin on needed tags (e.g., user is in a cozy-mystery mood but owns none):

1. `priority_boost_tags` is written to relevant `download_queue` rows, bumping books
   matching those tags up in download priority.
2. If no matching books exist in the monitoring queue either, a targeted discovery scan is
   triggered (sources: MAM new additions, Hardcover mood-tag browse for the matching tags).

This ensures the download pipeline is always ahead of what the queue needs — the gap is
filled before the user notices it.

#### Stage 2 → Queue Gap Check

Immediately after Stage 2 enrichment completes for any book (§8.0), Windlass runs the
queue gap check against the newly scored book. If it fills a current profile/mood gap,
its `download_queue` priority is raised before it is even promoted to the active pool. This
prevents the situation where a perfect book is sitting on disk unrecognised while the queue
is making do with weaker matches.

---

### 7.4 Tracker Economy

#### The MAM "Freeleech" Scavenger

> Opportunistically builds the long-term library buffer whenever a freeleech window is
> active, without spending ratio or wedges.

**Strategic role:** Freeleech is the primary mechanism for keeping the library well-stocked
at all times. Because freeleech downloads do not count against ratio, Windlass grabs strong
profile matches regardless of pipeline depth — a deep queue today is the buffer that
prevents a thin pipeline next month. Ratio and wedges are conserved for series continuations
and high-priority non-freeleech acquisitions.

**Trigger:** Windlass polls `https://www.myanonamouse.net/freeleech.php` at regular
intervals. When a new freeleech window is detected, Windlass records the `window_start` and
`window_end` timestamps and immediately begins evaluation.

**Execution (in order):**

1. *Filter audiobooks:* Ebooks on the list are ignored.

2. *Window timing check (pre-LLM):* For each candidate, Windlass estimates download
   completion time based on current seeder count and swarm health. Candidates where
   `estimated_completion + safety_buffer > window_end` are discarded silently — a download
   that does not complete before the window closes would have its remaining data charged
   against ratio, negating the benefit.

3. *Disk space check (pre-LLM):* Candidates that would breach the disk space floor are
   discarded. If disk is tight, remaining candidates are sorted by profile score so the
   best matches are evaluated first.

4. *LLM scoring:* Remaining candidates are passed through a Decide call (§7.6) against
   `profile_signals` only — **mood is not used for download decisions**. Freeleech is a
   library-building signal; what the user wants to own is governed by their stable taste,
   not their current listening mood. Returns `strong_match`, `borderline`, or `no_match`.

**Dispatch:**

| Score | Action |
|---|---|
| `strong_match` | Auto-grab silently. No approval required, no pipeline depth gate. Book is added to the download queue immediately. |
| `borderline` | Shown in the Free Discoveries panel (§9 Panel 4) for optional user approval. If the window is closing, the card carries a timing warning. |
| `no_match` | Discarded silently. |

**Constraints that still apply during a freeleech window:**
- HnR 72-hour seed requirement per book (unsatisfied quota, Rule 2.8)
- qBittorrent active torrent limits
- Disk space floor
- Window completion timing (see step 2 above)

Snatched books are seeded for upload credit and added to a "Free Discovery" collection in
ABS, clearly tagged so their origin is visible in the library and Active Queue Manager.

---

### 7.5 Supported LLM Engines (Privacy-First)

Both options guarantee zero data training. All three call types defined in §7.6 (Learn,
Decide, Mood Inference) are compatible with either engine.

| Option              | Engine                    | Cost                      | Notes                                                     |
| ------------------- | ------------------------- | ------------------------- | --------------------------------------------------------- |
| **A (Recommended)** | Cloudflare Workers AI     | Free (10,000 Neurons/day) | OpenAI-compatible endpoints; Llama 3.3 70B or DeepSeek R1 |
| **B**               | Google AI Studio (Gemini) | ~$0.25/month              | Natively supports Structured Output JSON schemas          |

---

### 7.6 Profile & Mood Engine

> The unified system for building, maintaining, and applying the user's taste profile.
> Every recommendation, queue pick, and scoring decision flows through this engine.

#### The Tag Registry

Windlass maintains a canonical `tags` table as the shared vocabulary for both books and the
user profile. Tags are grouped into categories:

| Category           | Examples / Description                                                  |
| ------------------ | ----------------------------------------------------------------------- |
| `genre`            | `hard_scifi`, `space_opera`, `romantasy`, `epic_fantasy`, `litrpg`     |
| `mood`             | `funny`, `hopeful`, `dark`, `tense`, `cozy`, `atmospheric`, `emotional` |
| `tone`             | `dry_wit`, `banter_heavy`, `slow_burn`, `action_packed`, `satirical`   |
| `style`            | Prose, structural, and psychological dimensions scored -100 to +100. Stage 2 produces two types: **algorithmic** (computed deterministically by NLP libraries — never passed through an LLM) and **LLM-estimated** (semantic dimensions requiring contextual reasoning). Both are stored as independent indices in `books.tag_vector` (S2 index band) and fused at inference time by the dot-product and Decide call. See §8.0 for the full 20-dimension table with source annotations. Legacy discovery-time tags `puzzle_solving` and `world_building_heavy` are superseded by `style:puzzle_density` and `style:lore_density`. `character_driven` is retired — replaced by the algorithmic `style:cs_density`. |
| `arc`              | Overarching emotional arc shape, assigned by Stage 2. Six types: `arc:rags_to_riches` (steady rise), `arc:tragedy` (steady fall), `arc:man_in_hole` (fall–rise), `arc:icarus` (rise–fall), `arc:cinderella` (rise–fall–rise), `arc:oedipus` (fall–rise–fall). |
| `arc_relation`     | Character relationship arc dynamics, assigned by Stage 2 Relation Arc sub-stage (§8.0). Distinct from `arc:*` which captures plot-level sentiment — `arc_relation` captures the *interpersonal circumstance* between directed character pairs. `arc_relation:volatile` (-100 = static dynamics throughout; +100 = highly volatile — betrayals, enemies-to-lovers, shifting power). **Gate: only computed for books with ≥ 500 extractable narrative events; novellas and short stories are skipped.** Runs locally on AMD GPU via ROCm using BookNLP (big model) + RoBERTa + GoEmotions. |
| `narrative`        | Syntactic structure and viewpoint. Stage 2 examples: `narrative:1st_person`, `narrative:3rd_limited`, `narrative:3rd_omniscient`, `narrative:2nd_person`, `narrative:single_pov`, `narrative:dual_pov`, `narrative:multi_pov`, `tense:past`, `tense:present`. Always +100 (present) or absent — treated as scoreable preference dimensions like any other tag, not as hard filters. |
| `preference`       | User-side queue composition meta-preferences. Example: `preference:tonal_variance` (-100 = binge reader — prefers sustained tonal consistency; +100 = variety reader — fatigues quickly on similar tone). **Never enters the SQL dot-product pre-score** — books carry no `preference:` score. Used exclusively in the Decide prompt as a semantic distance instruction and updated by the Learn call. Starts at 0 (neutral) and drifts with behaviour. |
| `content_warning`  | `sexual_content`, `graphic_violence`, `death`, `trauma`, `war`         |
| `length`           | `short` (<8h), `medium` (8–15h), `long` (15–25h), `epic` (>25h)       |
| `format`           | `standalone`, `series_complete`, `series_ongoing`                      |
| `protagonist`      | `solo_protagonist`, `ensemble_cast`, `morally_grey`, `found_family`    |

**Sourced tags** come directly from Audnexus and Hardcover, normalised to canonical slugs
at acquisition time. **LLM-enriched tags** are derived from the book description at
acquisition time — the LLM assigns tags from categories that the source APIs do not cover
(tone, style, protagonist, detailed length). **Algorithmically computed tags** are produced
by the Stage 2 NLP library pipeline (textstat, spaCy, LitNER, BookNLP, VADER, TextBlob,
LIWC-equivalent lexicons) — these are deterministic, GPU-accelerated where applicable
(ROCm/HIP), and never passed through an LLM for estimation.

**Controlled growth:** If the LLM encounters a quality no existing tag covers, it proposes a
new tag with a `canonical_name` and `description`. Windlass runs a normalisation check
against the existing registry before minting — a proposed tag is only added if it is
genuinely distinct. Users can review, rename, merge, or retire tags via Panel 6.

#### Profile Scores

The user profile is a flat table of scored dimensions in `profile_signals`. Each row is one
dimension: a tag, an author, or a narrator. Scores range from **-100 to +100** with 0
meaning no opinion:

| Score | User-facing label | LLM-facing definition |
|---|---|---|
| +100 | Must-Have | Absolute Positive Driver. Heavily elevates the score; can override minor negative traits. |
| +75 | Love It | Strong Positive Weight. A significant draw; weight the recommendation positively. |
| +50 | Like It | Moderate Positive Weight. Adds value but is not the sole driver of choice. |
| +25 | Nice to Have | Minor Positive Weight. A small contributing factor that slightly boosts appeal. |
| 0 | Neutral | Zero Affinity. Presence or absence has no impact on the score. |
| -25 | Not My Thing | Minor Negative Weight. Slightly lowers appeal but won't ruin a good book. |
| -50 | Dislike It | Moderate Negative Weight. Apply a moderate penalty; decreases enjoyment. |
| -75 | Hate It | Strong Negative Weight. Strongly penalise; only recommend if countered by +100 tags. |
| -100 | Dealbreaker | Hard Constraint (Veto). If prominently featured, reject regardless of other positives. |

Hard veto constraints (content warnings at -100, format dealbreakers, author/narrator blocks) are stored in `user_constraints` and applied as SQL `WHERE` filters before any dot-product scoring. Soft preferences (-99 to +99) remain in `profile_signals` as scoring weights. This separation ensures a dealbreaker cannot be mathematically outweighed by high scores on other dimensions.

> **LLM prompt injection:** Raw numbers are never passed to LLMs alone. Each score is
> accompanied by its anchor label at construction time, e.g.:
> `"Romance: -100 (Dealbreaker — Hard Constraint), Gritty: 85 (between Love It and Must-Have), Humor: 20 (Nice to Have)."`
> This gives the LLM both the mathematical weight and the semantic meaning it needs to
> reason accurately about tradeoffs.

#### The Three Call Types

**Learn** — runs after every review submission.

*Input:*
1. Full `profile_signals` table (all current scores, all context_ids)
2. The new review: book's tag scores + star rating + free-text + EndEmo Circumplex coordinates (`circumplex_pleasure_endemo`, `circumplex_activeness_endemo`) + explicit ranking position (`ranking_peers_json` — ordered list of the last 5 book IDs as placed by the user)
3. `completion_ratio` from `reading_ledger` (fast finish amplifies signal; slow finish dampens it)
4. `mood_snapshot_json` stored at listen-start (prevents mood-inflated ratings from over-writing the base profile)
5. Last 5 finished books: tags + rating + one-line review (same books shown in the ranking UI)
6. Last 3 DNF books: tags + stated reason

*Output:* a JSON delta of only the dimensions that changed, e.g.
`{"puzzle_solving": +8, "emotional": -5, "narrator:Ray Porter": +3}`.
Windlass merges the delta into `profile_signals` and stores the previous version for
Panel 6's optional re-calibration feature.

> **Pairwise Ranking Integration:** When `ranking_peers_json` is present, the Learn call
> uses a relative prompting strategy rather than evaluating the new book in isolation.
> The LLM is instructed: *"The user ranked Book A higher than Book B but lower than Book C.
> Analyse the decoded tag labels for all three books and identify the specific tags that
> explain this placement."* A **minimum distance filter** discards any pair where the
> absolute star rating difference is 0 to eliminate noise. The LLM outputs a tag delta
> based on this comparative analysis — isolating the user's core literary preferences from
> mood-congruent rating drift that would corrupt the absolute star score alone.

> **Queue Variance Learning:** The Learn call also evaluates the sequential pattern of the
> last 5 finished + last 3 DNF books to update `preference:tonal_variance` in
> `profile_signals`. Signals: if the user gave high ratings to consecutive books sharing
> dominant tone/mood tags, decrease (more negative) the score — binge pattern detected.
> If the user DNF'd a tonally similar book but highly rated a contrasting one, increase
> (more positive) the score — variety preference detected. `preference:tonal_variance` is
> never routed through subprofile logic directly in the Learn call; the existing subprofile
> gate handles mood-conditional splitting automatically over time.

**Subprofile Routing:** After merging the delta, the Learn call runs two independent split gates. Both gates run on every Learn call.

#### Gate A — Mood Splits (Circumplex-based)

Checks whether the user's ratings differ significantly between Circumplex states.

1. **Minimum Data Gate:** Query `reading_ledger` — require ≥5 reviews where the
   Circumplex state falls in the target quadrant AND ≥5 where it does not. If unmet,
   the delta is applied to `global` only and this gate is silently skipped.
2. **Statistical Gate (t_mean):** Calculate:
   $$t_{mean} = \frac{|\mu_{ic} - \mu_{\bar{ic}}|}{\sqrt{s_{ic}/n_{ic} + s_{\bar{ic}}/n_{\bar{ic}}}}$$
   where $ic$ = in-context ratings, $\bar{ic}$ = out-of-context ratings, $s$ = variance,
   $n$ = count. If $t_{mean} > 4.0$ (≈ p ≤ 0.05), the target subprofile rows are created
   in `profile_signals` (if absent) and the delta is routed there in addition to `global`.

Context ID naming: `circumplex_high_activeness` (A4–A5), `circumplex_low_activeness` (A1–A2),
`circumplex_high_pleasure` (P4–P5), `circumplex_low_pleasure` (P1–P2). Quadrant combinations
(e.g. `circumplex_low_pleasure_low_activeness`) are created if subsequent splits pass the gate.

#### Gate B — Taste-Cluster Splits (Bimodal variance detection)

Checks whether the user has incompatible preference clusters within a genre — e.g. consistently loving both grimdark fantasy and romantasy while rating genre-blending books poorly.

1. **Minimum Data Gate:** Require ≥10 ratings within the target genre dimension before running the scan.
2. **Variance Gate:** For each genre dimension with high rating variance, evaluate secondary tag axes (tone, style, content_warning) using Information Gain (t_IG / KL divergence):
   $$t_{IG} = \sum_{v} P(v) \cdot KL(R_{genre} \| R_{genre|secondary=v})$$
   If splitting the genre ratings along a secondary tag axis reduces entropy significantly (threshold: t_IG > 0.3), the system creates two taste-cluster subprofiles for that genre.
3. **Split creation:** Two `profile_signals` rows created with `context_id = 'taste_{genre}_{secondary_tag}'` (e.g. `taste_fantasy_grimdark`, `taste_fantasy_romantasy`). Future ratings are routed to the matching cluster based on the book's secondary tags.

Context ID naming: `taste_{genre_slug}_{cluster_slug}` — e.g. `taste_fantasy_grimdark`, `taste_scifi_hardscifi`, `taste_scifi_spaceoperera`.

**Author/narrator style transfer:** When the Learn call processes a highly-rated book by a known author (score in `profile_signals` dimension_type:author ≥ +50), it strengthens correlated style and genre dimensions in the active subprofile. A +5 Sanderson read → `style:cognitive_load`, `genre:epic_fantasy`, `style:lore_density` all drift upward in the matching taste-cluster subprofile. This propagates author affinity into the scoring vector without storing authors in `tag_vector`.

---

**Mood Inference** — runs at every queue pick and after every review.

> **Architecture note:** The Mood Inference call acts as the system's attention mechanism,
> translating the user's current behavioural state into concrete tag score deltas. The
> Circumplex coordinates it outputs serve as the subprofile routing key for subsequent
> Learn and Decide calls. SQL then executes the mathematical fusion; the Decide call is
> freed to explain, not rank.

> **Circumplex label mapping:** Coordinates are always translated to semantic labels
> before inclusion in any LLM prompt — raw integers are never passed. Fixed mappings:
> `circumplex_pleasure`: 1 = "very displeased", 2 = "displeased", 3 = "neutral",
> 4 = "pleased", 5 = "very pleased". `circumplex_activeness`: 1 = "very inactive",
> 2 = "inactive", 3 = "neutral", 4 = "active", 5 = "very active". Example prompt
> fragment: *"The user is currently feeling displeased and active (P2, A4)."*

*Input:*
1. Current date and month, with explicit prompt instruction to consider season as a
   potential factor — only if the data supports it, not as an assumption
2. Cross-book velocity delta: total listening hours in the last 7 days vs the previous 7 days
3. Time-of-day session pattern: which `time_of_day_bucket` slots dominate this week vs history
4. Rewind ratio: per-session backward jump frequency computed from `playback_sessions`
   (`start_position_sec` regressions ÷ total session duration) — high ratio signals
   distraction or low activeness
5. Last 5 finished books + last 3 DNF books (tags, ratings, review text)
6. Same-period historical reviews: all reviews where `finished_at` falls within ±4 weeks of
   today's calendar date in any prior year — included with a recency-weighting instruction
   so the LLM can detect (or dismiss) seasonal patterns without algorithmic pre-processing
7. Any active explicit vibe input or Vacation Mode flag
8. Current `mood_state.circumplex_pleasure` and `mood_state.circumplex_activeness` if
   recently set by explicit user input (translated to semantic labels per the mapping above)
9. Active subprofile rows from `profile_signals` where `context_id` matches the nearest
   Circumplex quadrant (if any exist) — allows the LLM to reference learned subprofile
   weights when generating tag deltas
10. **Cumulative emotional load:** For each of the last 5 finished books: `style:cognitive_load`
    (narrative complexity — working memory load), `style:linguistic_complexity` (sentence-level
    density), `style:emotional_distance`, and dominant `arc:*` tag from `books.tag_vector`.
    The LLM uses this to estimate the user's current "emotional battery" — consecutive
    `arc:tragedy` + high `style:cognitive_load` books deplete it faster than light,
    fast-paced novellas. When battery is judged low, output modifiers should reflect a
    recovery need (boosting `tone:hopeful`, `pacing:fast`, or `preference:tonal_variance`).
    This is a continuous judgment — there is no hard numeric streak threshold.

*Output:*
```json
{
  "circumplex_pleasure": 2,
  "circumplex_activeness": 4,
  "inferred_modifiers": { "short": 35, "emotional": -20, "dark": -30 },
  "inferred_context": "Listening time down ~60% this week — favouring shorter, lower-commitment picks",
  "explicit_override_decay": 0.65
}
```

`circumplex_pleasure` and `circumplex_activeness` are stored in `mood_state` as the
current inferred state anchor and used as the subprofile routing key for subsequent
Learn and Decide calls. Inferred modifiers are recomputed fresh at every queue pick —
they are never stored with a decay function. **Explicit overrides** (vibe query,
"Change mood" grid input) are stored separately in `mood_state.explicit_override_json`
and multiplied by 0.65 after each queue pick, dropping off the table when `|score| < 5`.
This gives explicit user intent a natural 4–5 pick fade without a jarring cutoff.

The `inferred_context` string is displayed in Panel 6 so the user always knows why the
queue shifted. It is never shown as a notification — it is ambient information.

---

**Decide** — runs at every queue pick, freeleech scoring, vibe query, and DNF palate
cleanser request.

> **Prompt structure (hourglass order):** Role & task → User profile + mood + popularity
> segment → Candidates → Constraints. Constraints go last to exploit the LLM's recency
> bias — rules placed mid-prompt after dense JSON are forgotten.

*Input:*
1. Active `profile_signals` rows — fused from two layers:
   - **Active circumplex subprofile** (if it exists for the current quadrant) takes precedence for matching dimension_ids
   - **`global`** fills any dimension not yet present in the active subprofile
   - Taste-cluster subprofiles are NOT used at Decide time — they shape what is already in the download buffer via the Stratified Portfolio; by the time Decide runs, the buffer already contains well-matched books for all clusters
   - `user_constraints` are applied as SQL `WHERE` filters before candidates reach the Decide call — hard vetos never appear in the prompt
   - Each score injected with its anchor label (e.g., "Space Opera: +80 (Love It)") — never raw numbers alone
   - Author/narrator preferences injected as a separate section: "User strongly prefers: [authors]. User avoids: [authors]."
2. Current `mood_state` (inferred modifiers + any active explicit override, combined)
3. **Top 10 pre-scored candidates** from the SQL dot-product, **randomly shuffled** before
   prompt injection (eliminates position bias). Each candidate includes title, author,
   decoded tag labels from `tag_vector`, and popularity class (H-class / T-class). The
   aggregate dot-product score is **not shown** — showing it causes the LLM to anchor on it
   and echo the SQL ranking rather than applying semantic reasoning. The decoded tag labels
   provide the "why" without handing the LLM the answer.
4. **User popularity segment:** N-Group or P-Group — computed from `profile_signals` ×
   `metadata_cache` at call time (see FairLRM Grounding Constraint below).
5. **Current Active Queue dominant tags:** For each occupied slot, the top 3–5 dominant
   `tone`, `mood`, and `arc` tags decoded from `books.tag_vector`. Used by the Queue
   Variance Rule to evaluate semantic distance between candidates and what the user is
   already reading.
6. **`preference:tonal_variance`** from `profile_signals` (current Circumplex subprofile
   if available, else `global`). This is a queue composition meta-preference — it has no
   item-side counterpart in `tag_vector` and never enters the dot-product pre-score.
7. For queue picks: pipeline depth tier

> **FairLRM Grounding Constraint:** The Decide prompt must include an explicit instruction
> grounding the pick in the user's `profile_signals` scores rather than the LLM's
> pre-trained preferences. For N-Group users the prompt states that niche/T-class candidates
> are the *correct* recommendation for this user; for P-Group users H-class candidates are
> preferred. This Dual-Side Semantic Understanding (user segment + item class together)
> prevents the LLM from substituting one popular title for another in the name of
> "diversity." Generic instructions like "avoid popular books" are insufficient and must not
> be used alone.

> **Queue Variance Rule:** The Decide prompt includes the current Active Queue dominant
> tags and the user's `preference:tonal_variance` score. The LLM is instructed to act as a
> semantic distance evaluator: if the score is negative (binge reader), prioritise the
> candidate whose dominant tags most closely match the current queue tone; if positive
> (variety reader), prioritise a candidate that matches the user's `profile_signals` but
> has contrasting primary `tone`/`mood` tags to the current queue. The LLM does not
> calculate numeric distances — it reasons about semantic similarity. Because all 10
> candidates are already strong profile matches from the SQL stage, even a "variety" pick
> is guaranteed to be a book the user will enjoy.

The Decide call is **two sequential LLM passes**. Ranking and blurb generation are
structurally different tasks (TKR vs. generative prose) — combining them in one prompt
causes negative knowledge transfer: the prose constraint degrades ranking accuracy and the
ranking context inflates blurb length. Separating them costs one extra call but both passes
become cheaper because each is simpler.

**Pass 1 — TKR Ranking:** Receives all inputs listed above. Returns only a ranked
selection with internal rationale.

*Output (Pass 1):*
```json
{
  "book_id": "abc123",
  "ranking_rationale": "Best mood-match on tone:dry_wit + arc:episodic; lowest tonal overlap with current queue; niche T-class consistent with user's N-Group segment."
}
```

`ranking_rationale` is output **first** — lightweight chain-of-thought that forces the LLM
to articulate constraint satisfaction before committing to a `book_id`. It is never stored
or surfaced; it exists only to improve pick accuracy. No picks array — Pass 1 returns
exactly one book.

**Pass 2 — Blurb Generation:** Receives only the selected `book_id`, its decoded tag
labels, the current `mood_state`, and the user's top 5 `profile_signals` dimensions. No
candidate list, no queue context. Can use a lighter/faster model than Pass 1.

*Output (Pass 2):*
```json
{
  "reason": "Matches your appetite for dry wit and brisk pacing after a heavy fantasy run. Short enough for a work week."
}
```

`reason` must be ≤ 50 words. If the queue addition is silent (series continuation),
`reason` is stored in `active_queue.reason` but never surfaced. If it fires an Action
notification, `reason` is the notification body. The routing is Windlass's decision, not
the LLM's.

> **Bias constraint:** The `reason` field must never contain star ratings, numeric scores,
> "X/5", "5-star", or any rating-scale language. Qualitative, descriptive prose only. This
> prevents the Scale Compatibility Effect: the review UI uses a 1–5 star scale, and a
> matching numeric format in the blurb anchors the user's post-consumption rating even
> when the blurb was delivered before the book was started.

---

## 8. Advanced AI Pipeline

> Windlass actively acquires epub counterparts alongside every audiobook download — epubs
> are small (2–5 MB) and unlock significant features. When an epub is unavailable, all
> standard features continue to operate and advanced features degrade gracefully as
> described in the feature tier table below.

| Feature | No epub | Epub available |
|---|---|---|
| Stage 1 enrichment (rough tag scores) | ✅ | ✅ |
| Stage 2 enrichment (detailed tag scores) | ⚠️ Hardcover reviews + description, medium confidence | ✅ hierarchical summary, high confidence |
| Hierarchical summarisation | ❌ | ✅ |
| Forced sync (audio ↔ text) | ❌ | ✅ |
| Glossary / Dramatis Personae | ❌ | ✅ |
| Sleep Recovery | ⚠️ progress-bounded fallback | ✅ full session-bounded search |
| "Previously On…" recap | ⚠️ from Hardcover/description only | ✅ from summary |
| Series Health Forecaster | ✅ Hardcover reviews sufficient | ✅ |
| Active Queue scoring | ✅ lower confidence | ✅ high confidence |

### 8.0 Stage 2 Post-Download Enrichment

> Upgrades a book's tag intensity scores from discovery-quality to high-confidence using
> the full book content. **A book is not eligible for Active Queue promotion until Stage 2
> completes** — the Decide call depends on accurate tag scores.

Stage 2 runs automatically after a book finishes downloading. It has two paths:

**Path A — Full enrichment (epub available):**
As soon as the epub is on disk, the main Windlass server runs an async enrichment job across
five stages:

**Stage A1 — Batch splitting, baseline metrics & full-text algorithmic pass:**
The epub is segmented into ≥ 10 sequential batches of approximately 10,000 words each (the
minimum window proven by narrative arc research to yield meaningful sentiment extraction and
reliable Savitzky-Golay smoothing). Simultaneously, a one-time full-text algorithmic pass
runs deterministically using NLP libraries:

- **Readability:** LIX score; Flesch reading ease score and average syllables-per-word
  (textstat). Together these calibrate `style:linguistic_complexity`.
- **Vocabulary richness:** Type-token ratio (unique words ÷ total words) for
  `style:vocabulary_richness`.
- **Structural density:** Total dialogue percentage (quote-span extraction);
  `style:dialog_density` baseline.
- **Expansiveness:** LitNER or spaCy NER run on full text — exact count of unique fictional
  characters and distinct location entities for `style:expansiveness`.
- **Invented vocabulary proxy:** Out-of-vocabulary (OOV) word density against a standard
  English dictionary. High OOV = dense invented terminology. Feeds as one component of
  `style:invented_vocabulary`.
- **Stylistic lexicon:** Concreteness scores from the MRC Psycholinguistic Database
  (word-level imageability) for `style:concreteness`; spaCy POS distribution
  (adverb %, adjective %, noun %) as a formality proxy for `style:formality`;
  TextBlob subjectivity score for `style:subjectivity`.
- **Perceptual word density:** LIWC-equivalent open-source lexicons (seeing/observation
  words for `style:perceptual_visual`; hearing + feeling + touch words for
  `style:perceptual_sensory`). These are among the strongest individual predictors of
  reading preference in algorithmic recommender research.

All full-text metrics are stored once in the file artifact (§Stage A6). They do **not**
need per-batch time-series — they are single scores computed over the entire text.

**Stage A2 — Multi-track per-batch processing:**
For each batch, five parallel tracks process the text. Tracks 1–4 produce one time-series
array per dimension. Track 5 produces a running character sentence count.

- **Track 1 — Emotional (LLM):** A single sentiment score (0–100) representing the
  protagonist's overall fortune/circumstances in that section. Produces the raw emotional
  arc array.
- **Track 2 — Thematic (LLM):** Presence scores (0–100) for *semantic* style dimensions
  that require contextual reasoning: `style:prose_ornamentation`, `style:narrative_pacing`,
  `style:focus`, `style:emotional_distance`, `style:core_drives`, `style:cognitive_load`,
  `style:lore_density`, `style:puzzle_density`, `style:tone_shift_magnitude`, and all
  `genre`, `mood`, `tone`, `protagonist` tags. Dimensions already computed algorithmically
  in A1 or A2-Track 4 are **excluded** from this prompt — the LLM is never asked to
  estimate things that libraries compute more accurately.
- **Track 3 — Syntactic (rule-based):** The dominant narrative perspective
  (`narrative:1st_person` / `narrative:3rd_limited` / `narrative:3rd_omniscient` /
  `narrative:2nd_person`) and tense (`tense:past` / `tense:present`) per batch. Usually
  constant, but per-batch checking catches experimental alternating-chapter structures.
- **Track 4 — Algorithmic (NLP libraries, per-batch):** Deterministic metrics that vary
  across the book and benefit from time-series smoothing: VADER sentiment score, TextBlob
  subjectivity score, dialogue % for this batch, average sentence length, sentence length
  variance, and punctuation frequency (!, ?, ;, :, em-dash) for `style:rhythmic_punctuation`.
  These run CPU-local with no LLM involvement.
- **Track 5 — Character Sentence (BookNLP + spaCy):** BookNLP (`big` model, GPU-accelerated
  via ROCm) performs coreference resolution and event tagging across the batch. spaCy
  dependency parsing then filters to sentences where a resolved character entity is the
  grammatical subject of an action, thought, or emotional predicate — a **Character Sentence
  (CS)**. The CS count per batch divided by total sentences gives a per-batch CS density.
  Produces the raw CS density array for `style:cs_density`.

- **Prose summary:** A structured short summary of this batch — key plot advancements,
  active characters, and world-lore introduced. Raw material for Stage A5.

**Stage A3 — Algorithmic smoothing (Savitzky-Golay):**
Raw per-batch arrays are too noisy for direct LLM interpretation (a single dark chapter in a
comedy creates a misleading spike). Each array is smoothed using a **Savitzky-Golay filter**:
window size = 1/10 of the total sequence length; polynomial degree 3. This eliminates noise
while preserving the true local maxima (peaks) and minima (valleys) of the trajectory.

**Stage A4 — LLM semantic translation + Relation Arc computation:**
The smoothed arrays are passed to a single LLM prompt that produces:
1. **Core emotional arc classification:** The overarching emotional array is matched to one
   of the six arc shapes and stored as an `arc:*` tag score (e.g., `"arc:cinderella": +85`).
2. **Thematic arc labels:** Each significant thematic array is translated into a semantic arc
   modifier added to the tag slug (e.g., romance `[0,10,30,70,100]` → `"romance:slow_burn"`
   scored +80; grief `[90,80,40,10,0]` → grief scored with `"arc:falling"` modifier in the
   summary).
3. **Narrative tag confirmation:** Dominant perspective and tense are written as +100 tags
   (e.g., `"narrative:1st_person": +100`, `"tense:past": +100`).
4. **POV count tag:** Single POV (1 unique perspective character), Dual POV (2), or
   Multi-POV (3+) assigned from the per-batch POV character lists after fuzzy-match
   deduplication (e.g., "Jon" and "Jon Snow" collapsed to one entity).
5. **20 writing style dimensions** — split by source:

**Algorithmically computed** (derived from A1 full-text pass or A2 Track 4 time-series;
averaged or directly stored; never LLM-estimated):

| Tag slug | -100 anchor | +100 anchor | Source |
|---|---|---|---|
| `style:linguistic_complexity` | Simple sentences / easy listen | Dense multi-clause / high syllable load | Flesch reading ease + syllable count (textstat) |
| `style:dialog_density` | Narration-heavy | Dialog-heavy | Dialogue span % per batch (A2 Track 4) |
| `style:concreteness` | Abstract / conceptual | Concrete / sensory | MRC Psycholinguistic Database imageability scores |
| `style:formality` | Colloquial / slangy | Formal / literary | spaCy POS distribution — adverb/adjective density |
| `style:subjectivity` | Objective / detached narrator | Subjective / feeling-heavy | TextBlob subjectivity score |
| `style:expansiveness` | Intimate / small cast | Expansive / large cast + many settings | LitNER unique character + location count |
| `style:event_density` | Variable pacing / literary | Constant action / pulp | BookNLP event count per batch (A2 Track 5 by-product) |
| `style:rhythmic_punctuation` | Staccato / clipped delivery | Flowing / cadenced | Punctuation frequency + sentence length variance (A2 Track 4) |
| `style:cs_density` | Exposition / world-building dominated | Character thought / action dominated | BookNLP coreference + spaCy dep parse CS count (A2 Track 5) |
| `style:vocabulary_richness` | Repetitive / narrow vocabulary | Varied / sophisticated vocabulary | Type-token ratio (A1) |
| `style:invented_vocabulary` | Contemporary realism / standard English | Dense invented terminology (Tolkien / Sanderson) | OOV word density against standard English dictionary (A1) |
| `style:perceptual_visual` | Abstract / non-observational | Highly visual / seeing-word-dense | LIWC-equivalent seeing/observation lexicon (A1) |
| `style:perceptual_sensory` | Non-sensory | Hearing + feeling + touch word-dense | LIWC-equivalent hearing/feeling/touch lexicon (A1) |

**LLM-estimated** (require semantic reasoning; computed from A2 Track 2 smoothed arrays):

| Tag slug | -100 anchor | +100 anchor | Note |
|---|---|---|---|
| `style:prose_ornamentation` | Sparse / Hemingway | Lyrical / Tolkien | |
| `style:narrative_pacing` | Deliberate / slow-burn | Staccato / frantic | Distinct from event_density — subjective feel, not event count |
| `style:focus` | Internal / introspective | External / action-driven | |
| `style:emotional_distance` | Clinical / detached | Visceral / intimate | |
| `style:core_drives` | Affiliation (allies, belonging) | Achievement / power | Psychological motivation of narrative |
| `style:cognitive_load` | Light mental load | Heavy working-memory load — many characters, plot threads, unreliable narrators | Distinct from linguistic_complexity — narrative complexity, not sentence difficulty |
| `style:lore_density` | Contemporary realism / minimal world-building | Dense explanatory lore / magic systems / invented history | Complements invented_vocabulary — captures depth, not just vocabulary |
| `style:puzzle_density` | Pure immersive narrative | Highly clue-structured / reader-solves-alongside-protagonist | |
| `style:tone_shift_magnitude` | Consistent / predictable tone | Volatile / bait-and-switch | Computed from smoothed *variance* of the Tone batch array, not averaged |

**Relation Arc sub-stage** (runs after the main LLM call, GPU-accelerated via ROCm):
Using the BookNLP entity and event output already produced in Stage A2 Track 5:
1. Extract all narrative events with their actor and experiencer entities.
2. Score each event's sentiment using RoBERTa and assign fine-grained emotion labels
   (anger, joy, fear, trust, etc.) using a GoEmotions multi-label classifier.
3. For each significant directed character pair (protagonist ↔ antagonist, protagonist ↔
   love interest, etc.), plot a time-series of event sentiment across the book.
4. Apply Savitzky-Golay smoothing to each pair's arc.
5. Compute the variance across all pair arcs → stored as `arc_relation:volatile`.

**Gate:** This sub-stage only runs if BookNLP extracted ≥ 500 narrative events from the
full text. Books with fewer events (novellas, short stories) skip this sub-stage silently —
insufficient events produce statistically unreliable arcs.

**Stage A5 — Narrative summary consolidation:**
A single LLM call receives all per-batch prose summaries from Stage A2 in sequence and
produces the consolidated **act-by-act narrative summary** — a structured document covering
key plot advancements, character roster, and world-lore for the full book. This summary
powers: JIT context injection (§8.3), "Previously On…" recaps, Glossary / Dramatis
Personae generation, Sleep Recovery session-bounded search, and Series Health Forecasting.
The summary is never shown to the user directly; it is consumed by other pipeline stages and
LLM calls as high-signal context.

**Stage A6 — Dual storage:**
- **PostgreSQL (lean):** Only the final concrete tag scores from Stages A1, A2, and A4 are
  written into `books.tag_vector` (S2 index band). `enrichment_stage` advances to
  `post_download_full`. The book becomes eligible for Active Queue promotion immediately.
- **File artifact (rich):** The following are bundled into
  `windlass_data/enrichment/{book_id}.json`:
  - Raw unsmoothed per-batch arrays (all tracks)
  - Savitzky-Golay smoothed arrays
  - Full-text A1 metric values (LIX, Flesch, type-token ratio, POS distributions, OOV
    density, perceptual word counts, character/location counts)
  - BookNLP entity and event output files (`.entities`, `.tokens`, `.events`)
  - Per character-pair Relation Arc time-series and smoothed arrays (if ≥ 500 events)
  - LLM's full prose narrative summary

  Raw arrays and BookNLP outputs are **never** stored in PostgreSQL — they would bloat
  the database and slow the Decide call's dot-product queries. The file artifact is
  retained for 90 days, giving a re-enrichment window if smoothing parameters, tag
  definitions, or NLP models improve without needing to re-download the epub.

Separately and in parallel, the Worker Node runs forced alignment (§8.1/§8.2), which
unlocks the precision user-facing features (Glossary spoiler boundary, Sleep Recovery,
JIT context summaries). These are additive — the book is already playable before
alignment completes.

**Path B — Lite enrichment (no epub):**
Runs on the main server without the Worker Node. The LLM receives the full Hardcover
community review set (up to 20 reviews), the book description, and all Audnexus/MAM tags.
This substantially improves on Stage 1 but cannot capture structural narrative details.
- Tag categories scored with medium confidence
- `enrichment_stage` updated to `post_download_lite`

In both cases, `books.tag_vector` (S2 indices) is updated in place. The book then becomes eligible
for Active Queue promotion.

### 8.1 The Asynchronous Worker Node

> Off-loads compute-heavy forced alignment to local high-performance hardware without
> impacting the lightweight server. The Worker Node does not run LLMs.

**Rationale:** Forced audio-to-text alignment (WhisperX) is CPU/GPU intensive and
unsuitable for a low-power home server. Windlass offloads this work to a separate worker
process designed to run on local high-compute hardware (e.g., a workstation or gaming
laptop).

**Execution:**

- The worker is governed by a `systemd` timer with a `ConditionACPower=true` guard,
  ensuring it only activates when the device is on wall power and never drains battery.
- On wake, the worker polls the Windlass REST API for books in a `pending_alignment` state.
  If the queue is empty, it exits silently.
- For each queued book, the worker pulls the `.m4b` and `.epub` files (via direct network
  mount or temporary API download), runs forced alignment (§8.2), and pushes the resulting
  `sync_artifacts` back to the Windlass API.
- On completion the book's alignment state advances to `aligned`, an `Alert` (`Normal`)
  notification is fired, and the JIT summarization pipeline (§8.3) becomes available.
  The worker scrubs all local temporary files after each job.

### 8.2 Forced Synchronization Engine (Audio-to-Text Mapping)

> Bridges Audiobookshelf's time-based tracking and the LLM's text-based context by
> generating a precise map between audio timestamps and epub paragraphs.

**Execution:** The worker node runs a forced-alignment tool (e.g., WhisperX) against the
`.m4b` audio and `.epub` text. The output is a structured JSON map stored in `sync_artifacts`,
linking every audio timecode (e.g., `04:12:35`) to the corresponding paragraph and chapter
in the epub.

**Impact on existing features:**

- **Glossary Generator & "Previously On…":** The LLM text payload is truncated at the
  user's exact audio timestamp rather than chapter-level granularity, hardening the
  spoiler-free guarantee.
- **Hierarchical Summarization:** The sync map is the prerequisite that enables JIT
  summary generation tied to precise reading progress milestones (§8.3).

### 8.3 Hierarchical Context Summarization

> Compresses a novel into structured, spoiler-safe LLM memory that grows as the user
> progresses — distinct from the pre-reading enrichment summary (§8.0).

**Execution:** Once a sync artifact exists, Windlass segments the epub into logical "Acts"
(roughly 20% milestones). These summaries are **Just-In-Time and spoiler-gated** —
triggered when ABS reports the user has completed each Act, never pre-processed. Each Act
is summarised via a single LLM call using a strict JSON Schema:

- `plot_advancements`: Key events that occurred in the Act
- `character_roster`: New characters introduced; status updates on existing ones
- `world_lore`: New rules, locations, or mechanics explained in the text

**The Super-Summary:** When any feature requiring book-wide context fires (Bailout Protocol,
Series Recap, Slog Detector analysis), Windlass concatenates all completed Act summaries
into a single dense payload — giving the LLM full memory of everything the user has read so
far at a fraction of the token cost.

### 8.4 The "Sleep Recovery" Protocol

> A natural-language rewinding tool that rescues users who fall asleep during playback and
> wake up having lost their place.

**Session Boundary Tracking:** Windlass continuously monitors ABS for playback state
changes, logging a row to `playback_sessions` every time playback starts or stops
(`start_time`, `start_position_sec`, `end_time`, `end_position_sec`). This bounds the
search window to the exact block of audio that played during the suspected sleep session,
preventing the LLM from finding a false match elsewhere in the book.

**Execution:**

1. The user opens the Action Center and selects **"I fell asleep."**
2. Windlass prompts: _"What is the last thing you remember happening?"_
3. Windlass identifies the last significant listening session (e.g., 4 hours from 11 PM to
   3 AM) and extracts only the epub text corresponding to that time window via the sync map.
4. The LLM searches strictly within this bounded text block, returning the exact matching
   paragraph.
5. Windlass translates that paragraph back to an audio timestamp, uses the ABS API to rewind
   the user's progress to that exact second, and logs the correction in `reading_ledger`.

**Offline / Partial Sync Fallback:** When playing on a mobile client without a live server
connection (e.g., the "Still" iOS app in airplane mode), real-time session boundaries are
unavailable. Windlass falls back to a **progress-bounded search**: it restricts the LLM's
search window to the epub text between the last known synced position before the offline
session and the newly synced position on reconnect. The result is identical — the user is
rewound to the correct sentence.

**Degraded Mode (No Epub):** If no sync artifact exists, Sleep Recovery cannot perform
text localisation. Windlass instead presents the user with the raw session log (_"You
listened for 3h 42m, from 06:14:22 to 10:01:44"_) to assist with manual rewinding.

---

## 9. The Control Plane (Web UI)

- **Embedded Web UI:** A responsive dashboard (mobile + desktop) served directly from the
  Rust binary via axum and rust-embed. Built with React + Vite + shadcn/ui.
- **Web Push Notifications:** Delivers push notifications for critical NAT errors, series
  check-ins, queue additions, and prompts to review finished media. Each notification
  deep-links to the relevant PWA card. See §2 Notification Architecture for delivery
  details. All alerts are persisted to the database with a unique ID, severity, timestamp,
  triggering event, and system state snapshot.
- **Real-time Updates:** The UI subscribes to a Server-Sent Events (SSE) stream for live
  state updates and event/action history. Commands (manual reset, queue actions) are sent
  via REST POST.

### Onboarding: The Librarian Interview

On first boot, Windlass performs a guided profile initialisation:

1. **Library Import & Rating Wizard:** Windlass scans the existing Audiobookshelf library
   and actively prompts the user to rate every book in it. The UI presents each title as a
   card — the user swipes or taps to assign a star rating and optionally leave a free-text
   review (using the Universal Review Component). Partially completed wizards resume where
   they left off; books left unrated are skipped and can be reviewed later from the Reading
   Ledger. The goal is to build a rich initial history — the more books rated here, the
   better the profile from day one.
2. **Dealbreakers & Preferences:** The wizard explicitly asks the user for hard boundaries,
   dealbreakers, and core preferences (e.g., "No LitRPG", "Must have a single narrator").
3. **LLM Profile Generation:** Each review and dealbreaker submitted during the wizard
   triggers a Learn call (§7.6). By the end of the wizard, `profile_signals` is populated
   with initial tag, author, and narrator scores derived from the user's actual ratings and
   stated preferences — no manual slider-setting required.

**Cold-start behaviour:** Until `reading_ledger` contains at least 20 reviewed books,
Windlass operates in cold-start mode. The Acquisition Confidence Threshold is automatically
raised — more candidates surface in Panel 1 for explicit user approval rather than
auto-grabbing silently. This prevents the system from making confident decisions from a
sparse profile. The user is shown a progress indicator: *"Your profile is N% complete —
rate more books to improve recommendations."*

### Action Center

The Action Center is a **pipeline oversight panel** — not the primary interaction surface.
Most users will interact with Windlass primarily through push notifications and never need
to open it. It exists for users who want to inspect the current state of their pipeline,
adjust priorities, or add something manually.

It is organised into seven panels.

#### 1. Suggested Next Listens

AI-curated recommendations based on `profile_signals`, `mood_state`, and reading history. Also the landing zone for books discovered via the universal input box (URL paste,
search, vibe query). Each card in the list shows book cover, title, author, narrator,
duration, format badge, and series health badge (if applicable). **"Sell It To Me" pitches
are not shown in the list** — they are generated fresh when the user opens a specific card
or receives a notification, ensuring the pitch reflects current context.

- **The "Already Read" Workflow:** To easily build history without downloading known books,
  every AI-curated card features an **"Already Read"** action button alongside _Approve_,
  _Reject_, and _Snooze_. Users can also paste an external URL (e.g., Audible) into the
  Universal Input Box and tag it as "Already Read".
- **Immediate Capture:** Clicking "Already Read" instantly opens the Universal Review
  Component. The rating and text are injected directly into the `reading_ledger`, and the
  LLM uses it to immediately refine the user's profile.

Actions per card: **Approve** (moves to monitoring queue) · **Reject** · **Snooze** · **Already Read**

The universal input box sits at the top of this panel:

```
┌──────────────────────────────────────────────────────────┐
│  🔍  Search, paste a URL, or describe what you want…    │
└──────────────────────────────────────────────────────────┘
```

- **Direct MAM URL** → queued immediately, no LLM call
- **Any other URL / author search** → resolves metadata, runs Series Health if Book 1,
  card appears for approval
- **Vibe query** → LLM picks best match; card appears for approval

#### 2. Download Queue

Books approved for download, in priority order. Supports drag-and-drop reordering
(touch-first via `@dnd-kit/core`). Shows live download progress per torrent. Series
continuations auto-populate here without requiring approval.

#### 3. Upcoming in Series

All incomplete series the user has started, sorted by priority (actively-listening series
first). Each entry shows series position, release date (from Audnexus), and current
availability on MAM. Unreleased entries show a countdown. This panel feeds the Predictive
Series Syncing engine.

#### 4. Free Discoveries (Freeleech Scavenger)

Populated whenever a MAM freeleech window is active. Strong profile matches are
auto-grabbed silently and appear directly in the library — this panel shows only
**borderline matches** that did not meet the auto-grab threshold and are waiting for
optional user approval. Each card shows the freeleech window end time, a "FREELEECH"
badge, a personalised pitch, and a timing warning if the window is closing soon.
Approved torrents are snatched, seeded for upload credit, and tagged as "Free Discovery"
in ABS.

#### 5. In Library — Unread

Books already present in ABS but not yet started. Ensures there is always something ready
to listen to next. Cards use the same format as Suggested Next Listens. Windlass alerts if
this panel is empty and the Download Queue is also empty.

#### 6. User Profile Dashboard

A dedicated control panel exposing the user's full taste profile as it exists in
`profile_signals` and `mood_state`. Organised into four panes:

- **Tag scores** — sliders grouped by category (genre, mood, tone, style,
  content_warning, length, format, protagonist). Every slider is directly editable.
- **Author & narrator scores** — same -100→+100 sliders.
- **Current mood** — displays the active `inferred_context` string and the current
  Circumplex coordinates (`circumplex_pleasure` / `circumplex_activeness`) translated to
  their semantic labels (e.g. "pleased · inactive") so the user can see why the queue has
  shifted. Shows any active explicit override tag deltas with their remaining decay strength.
  The user can clear the inferred mood, set a new explicit override via the Circumplex grid
  + vibe text input, or view which subprofile (if any) is currently active.
- **Tag registry** — lists all tags in the canonical registry. User can rename, merge, or
  retire tags. Any tag edit propagates immediately to all book records and profile scores.

Manual edits to sliders take effect immediately on the next queue pick. The LLM does not
automatically revert manual edits — they are treated as authoritative until a future Learn
call produces a conflicting delta, at which point the user is shown a diff and asked to
confirm.

#### 7. Reading Ledger & Reviews

A historical, searchable catalog displaying all data from the `reading_ledger` and `reviews`
tables.

- Users can revisit old books, read their past free-text reviews, and retroactively adjust
  ratings.
- **Optional Re-calibration:** When a user edits a past review or rating, Windlass does
  _not_ automatically overwrite the profile. Instead, it presents a prompt: _"Do you want
  to re-calibrate your AI profile based on these changes?"_

---

## 10. Mobile Companion App (iOS)

A Progressive Web App (PWA) that serves as the primary mobile interaction surface —
handling the review-and-discovery loop, Active Queue management, and push notifications.
Audio playback is delegated to **Plappa**, a native Swift ABS client that delivers
reliable progress syncing and hands-free continuous playlist playback.

### 10.1 Architecture

**PWA:** The Windlass web UI (§9) is built as an installable PWA. On iOS the user visits
the Windlass URL in Safari and taps "Add to Home Screen." The installed PWA receives Web
Push notifications and maintains a live SSE connection to the Windlass server when open.

**Player:** [Plappa](https://github.com/leoklaus/plappa) is the recommended iOS audiobook
player. Built in native Swift/SwiftUI, it delivers:
- Reliable background progress syncing to ABS (critical for session tracking and webhooks)
- Continuous ABS playlist playback — plays the Active Queue hands-free
- Deep-link URL scheme for one-tap play from the PWA
- Offline download, CarPlay, Apple Watch, and lock screen controls

**Connectivity:** All PWA ↔ Windlass communication travels over Tailscale. Web Push
payloads carry only a silent wake signal; actual notification content is fetched from the
Windlass server directly over Tailscale. Apple's servers see a device wakeup timestamp —
nothing else.

### 10.2 Use-Case Split

| Surface | Purpose |
|---|---|
| **iPhone PWA** | Notifications, rating finished books, Active Queue management, sleep recovery, mood queries |
| **Plappa** | All audio playback, offline downloads, CarPlay, lock screen, continuous queue playback |
| **Desktop PWA / browser** | Mission control — monitor pipeline, manage downloads, tune settings, reorder Active Queue |

### 10.3 Key Screens

#### Active Queue Manager

The primary mobile screen. Shows the current 3-slot Active Queue (the ABS playlist Plappa
plays), the current mood context string, and downloaded-but-not-yet-queued books.

- **Mood context banner:** displays `mood_state.inferred_context` with a **"Change mood"**
  button that opens the hybrid mood input (Circumplex 5×5 grid + optional vibe text —
  same flow as Queue View)
- Drag-and-drop slot reorder (reordering a non-pinned slot implicitly pins it)
- **Build My Queue** button — opens the wizard (§7.7) for guided queue building
- Each card shows: cover, title, narrator, duration, AI blurb (`active_queue.reason`), tags, 📌 if pinned
- Card actions: **Pin / Unpin** · **Remove** · **Already Read**
- **Play:** deep-links into Plappa with the Active Queue loaded

**DNF triggered** → Universal Review Card opens, then the three-option What's next? screen
(Skip / Fresh start / Let me guide you) as described in §7.3.

#### Review Card

Opened from the "Book Finished" `Action` notification or the Reading Ledger. Uses the
Universal Review Component (star rating + free-text). The next book in the Active Queue
is already playing — the review is asynchronous and can always be completed later.

#### Action Center (Mobile)

Condensed pipeline view: Download Queue, Upcoming in Series, Free Discoveries. Supports
card approvals and queue reordering without opening the desktop UI.

#### Event Log (Desktop)

A chronological, searchable log of every significant action Windlass has taken, drawn from
the `events` table. Each row shows timestamp, source rule/feature, action, and the
affected book (linked to its card). Filterable by source and action type. Read-only — the
event log is an audit trail, not an inbox. Useful for understanding why a book was grabbed,
why a slot was replaced, or tracing the enrichment pipeline for a specific title.

### 10.4 Notification Behaviour

When the PWA is open, the SSE connection delivers alerts as in-app modals immediately —
no push notification needed. Key in-app transitions:

- **Book finished** → Review Card opens automatically
- **Review submitted** → Active Queue Manager with next book highlighted
- **DNF triggered** → Universal Review Card opens automatically, followed by the What's next? screen (Skip / Fresh start / Let me guide you)

When the PWA is closed, Web Push delivers `Alert` and `Action` notifications to the iOS
lock screen (see §2 Notification Architecture). `Action` button support depends on the
browser; on iOS Web Push the notification degrades to `Alert` — the same choices are
available as UI buttons when the card is opened.

### 10.5 Sleep Recovery Integration

The **"I fell asleep"** button is surfaced in the Review Card and in the Active Queue
Manager. Tapping it opens the Sleep Recovery prompt (§8.4) inline.

---
