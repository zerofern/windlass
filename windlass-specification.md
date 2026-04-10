copilot --resume=53226173-92e3-4747-94ae-ba87b8a49e88

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

All persistent state is stored in a local SQLite database (`windlass.db`). There is no
flat JSON state file. SQLite is appropriate for this workload: Windlass is single-user,
single-writer, and the database is projected to stay under 10 MB (excluding file-backed
artifacts) indefinitely. WAL mode is enabled.

Large per-book artifacts are stored as flat files under `windlass_data/` rather than as
SQLite blobs, keeping the database lean and making per-book deletion trivial:

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
| `books`               | Every title Windlass knows about. Library status lifecycle (`known → queued → downloading → seeding → completed`), source (`manual_abs`, `windlass_download`, `ai_suggestion`, `freeleech`), Audnexus ASIN, Hardcover ID, ABS item ID, `epub_status` (`searching` / `found` / `not_found`), `tag_scores_json` (tag intensity scores -100→+100; top 20–30 tags also denormalised as real columns for indexed dot-product queries), `enrichment_stage` (`discovery` / `post_download_lite` / `post_download_full`), `enrichment_summary_path` (path to file-backed enrichment summary, null until Stage 2), `reason` (LLM-generated blurb for why this book suits the user; overwritten on re-evaluation). A book record survives disk deletion. |
| `metadata_cache`      | Read-through cache for external API responses. Keyed by `(source, external_id)` where `source` is `audnexus` or `hardcover` and `external_id` is the ASIN or Hardcover ID. Stores the raw `response_json` and `fetched_at` timestamp. TTL: Audnexus 30 days (stable data), Hardcover 7 days (community reviews change frequently). Eliminates redundant API calls across Stage 1, Stage 2, and all LLM context assembly. |
| `tags`                | Canonical tag registry. `id` (slug), `canonical_name`, `category` (`genre` / `mood` / `tone` / `style` / `content_warning` / `length` / `format` / `protagonist`), `description`, `source` (`audnexus` / `hardcover` / `llm_mint`), `status` (`active` / `deprecated`). Controls the tag vocabulary — see §7.6. |
| `series`              | Series identity and health (Audnexus data, user started/following flags). `engagement_trend_json`: array of `{book_number, rating, completion_ratio, slog_events}` appended after each series book review. Used for series drop-off detection. |
| `torrents`            | File data once a download starts: qBittorrent hash, seed time, HnR status, ratio, disk path. |
| `download_queue`      | Thin table: books actively in the approval/download funnel only. `status` lifecycle: `pending_review → approved → monitoring → downloading`. `priority`: `critical` (series continuation) / `high` (strong profile match, freeleech) / `normal` / `low`. `freeleech_window_end` (nullable — elevates urgency when set). `enrichment_confidence` (float). Row deleted once `books.library_status` advances to `seeding`. |
| `active_queue`        | The 3-slot ABS playlist Windlass manages. `slot` (1–3), `book_id` FK to `books`, `pinned` (bool — user-locked slot; never auto-replaced), `reason` (the Decide call's blurb for this pick), `mood_snapshot_json` (snapshot of `mood_state` at time of selection). Pinned slots survive mood re-evaluations. When a pinned book finishes, the pin is consumed and the slot returns to Windlass control. |
| `reading_ledger`      | One row per listening attempt (supports re-reads). `started_at` (first playback session), `finished_at` (ABS completion webhook), `completion_ratio` (actual calendar days ÷ expected days, computed at finish), `mood_snapshot_json` (mood state at listen-start). Retained permanently after disk deletion. |
| `reviews`             | User feedback rows keyed by ledger entry: completion review, optional midway note, DNF autopsy. Retained permanently. |
| `slog_detector`       | Pacing stall detection state per active ledger entry. Purged when the ledger entry is closed (finished or DNF). |
| `series_check_ins`    | Records of the 60–75% series check-in: what was offered, what the user chose. |
| `profile_signals`     | One row per scored dimension. `dimension_type` (`tag` / `author` / `narrator`), `dimension_id` (canonical tag slug or name), `score` (integer -100→+100). Updated after every review via Learn call delta. |
| `mood_state`          | Single-row table (replaced on each update). `inferred_modifiers_json` (tag score deltas from inference), `explicit_override_json` (user-set modifiers with per-pick decay multiplier at 0.65×, dropped when `\|score\| < 5`), `inferred_context` (human-readable explanation shown in Queue View and Panel 6), `computed_at` timestamp. |
| `events`              | Internal audit log. One row per significant system action. `source` (which rule or feature triggered it — e.g. `freeleech_scavenger`, `mood_inference`, `series_continuation`, `stage2_enrichment`), `action` (e.g. `book_grabbed`, `slot_replaced`, `epub_found`), `book_id` (nullable FK), `detail_json` (structured context). Read-only — never modified after insert. **Retention: 90 days rolling.** Visible in the desktop UI Event Log panel. Distinct from `alerts` (which are user-facing and actionable). |
| `alerts`              | Fired alerts. UUID primary key for notification deep-links. Severity, timestamp, triggering event, system state snapshot. **Retention: 30 days rolling.** |
| `playback_sessions`   | One row per play/pause event (or scheduled ABS position poll) per book. `start_time`, `start_position_sec`, `end_time`, `end_position_sec`, `device_id`, `time_of_day_bucket` (`morning` / `afternoon` / `evening` / `night`), `day_of_week`, `source` (`webhook` / `poll`). Used for Sleep Recovery, slog detection, and mood inference. Retained permanently (required for seasonal pattern queries). |
| `sync_artifacts`      | Metadata row for a book's forced-alignment file. `book_id` FK, `alignment_path` (path to `windlass_data/sync/{book_id}.json`), `state` (`pending_alignment → aligned`). Only present when an epub counterpart exists. The file is deleted with the book; this row is deleted at the same time. |
| `context_chunks`      | Hierarchical Act summaries per book generated JIT as the user progresses. FK to `books`. Stores `act_index`, `plot_advancements`, `character_roster`, and `world_lore` as structured JSON. **Retention: deleted 24 hours after `reading_ledger.finished_at`, or after a "Previously On" recap has been generated — whichever is later.** |

### JIT (Just-In-Time) Context Injection

Each LLM call receives only the data relevant to its task — the full context contract for
each call type is defined in §7.6. The general principle: Windlass queries SQLite for a
small, hyper-relevant payload rather than passing the entire reading history.

### External Meta-Scraping

**Audnexus** (`api.audnex.us`) provides blurbs, series ordering, tags, and release dates.
**Hardcover.app** provides written user reviews and social metrics. Both are bundled into
the RAG payload before any LLM call.

---

## 4. MAM Compliance & Torrent Management

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
  - _No Partials:_ Forces qBittorrent to download 100% of torrent contents.
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

- **Base Rules:** Evaluates seeders and Freeleech status. The base score for each
  candidate starts at 0 and is adjusted by Custom Format Weight rules.
- **Custom Format Weights (Radarr-Style):** Users define custom score adjustments in the
  Action Center using Regex or plain keyword rules matched against the torrent title,
  uploader, and format fields (e.g., `+50` for "Ray Porter", `−100` for "Abridged",
  `+25` for "Graphic Audio"). Rules are evaluated in priority order and combined additively
  with the base score. These weights are the primary mechanism by which the auto-grabber
  selects the correct release without user approval — a narrator preference or format
  rejection defined here directly controls which torrents are snatched automatically.

  **Default rules (pre-installed, user-editable):**

  | Rule | Score | Rationale |
  |---|---|---|
  | Format: `m4b` | `+0` | Baseline — preferred format |
  | Format: `mp3` | `−100` | Excluded by default |
  | Format: `m4a`, `ogg`, other audio | `−100` | Excluded by default |
  | Title contains `Abridged` | `−100` | MAM rule compliance |
  | Language: `English` | `+50` | Prefer English editions by default |
  | Language: not `English` | `−50` | Deprioritise non-English editions |

  Narrator preference is not pre-installed — users add their own rules (e.g. `+80` for
  "Ray Porter", `+60` for "Kate Reading"). This is the primary mechanism for resolving
  between multiple editions of the same work.

  Scores are integers on the same **-100 → +100 scale** used throughout Windlass. A
  candidate scoring below **0** after all rules are combined is not auto-grabbed. When
  multiple torrents match the same work, the highest-scoring one is selected — only one
  torrent is ever downloaded per work.

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
     using `books.tag_scores_json × (profile_signals + mood_state modifiers)`. The top 10
     candidates go to a Decide call (§7.6), which reasons about contrast, narrative variety,
     and mood fit — returning a ranked list with a `reason` per pick. The top pick is
     promoted; its `reason` becomes the notification blurb.

  **Non-series additions** fire an `Action` (`High`) notification:
  > _"[Book] has been added to your queue."_
  > Blurb (the Decide call's `reason` field, generated against current mood)
  > **Keep it** · **Swap out** · **Already Read** · **Change mood**

  Ignoring the notification keeps the book in the queue. **Already Read** opens the
  Universal Review Component inline, logs the entry to the `reading_ledger`, and
  immediately triggers a replacement pick. **Change mood** updates `mood_state` with an
  explicit override and re-runs the Decide call.

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

Windlass pulls candidates from five sources. All feed into the same enrichment and
monitoring queue pipeline.

| Source | Mechanism | Cadence |
|---|---|---|
| **User-initiated** | Universal Input Box (see below) | On demand |
| **MAM new additions** | Poll MAM audiobook catalogue sorted by date added | Hourly |
| **Hardcover trending** | GraphQL API: trending, upcoming, popular lists | Daily |
| **Hardcover mood/tag browse** | GraphQL API: filtered to user's top-scored genres and moods | Daily |
| **Series continuation** | Predictive Series Syncing (§6) — bypasses enrichment, goes direct to `priority: critical` | Event-driven |

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

#### Stage 1 Enrichment (Discovery-Time)

Runs on every new candidate before it enters the monitoring queue. Fast and cheap — many
books pass through here.

**Input:** book description + Audnexus genre/narrator tags + Hardcover mood tags + up to 5
community review excerpts from Hardcover.

**Output (stored in `books.tag_scores_json`):**
- Intensity scores (-100→+100) for all applicable tags across all categories
- `enrichment_stage: discovery`, `enrichment_confidence: low/medium`
- A rough profile match score against the current `profile_signals`

Strong matches (above the **Acquisition Confidence Threshold** — a configurable score
with a sensible default, adjustable in the Action Center settings) auto-enter the
monitoring queue. Borderline matches surface in Panel 1 for user review. Non-matches are
discarded (but the `books` record is retained so the same title is not re-evaluated on the
next poll).

**Discovery dispatch table:**

| Score vs threshold | Action |
|---|---|
| ≥ threshold | Auto-approved → monitoring queue, no notification |
| 50–threshold | Shown in Panel 1 (Suggested Next Listens) for user approval |
| < 50 | Discarded silently; `books` record retained |

*During the cold-start period (fewer than 20 reviewed books in `reading_ledger`), the
threshold is automatically raised so that more candidates surface in Panel 1 rather than
auto-grabbing — profile confidence is too low to trust silent auto-approval.*

**Epub acquisition:** whenever a book is approved for download, Windlass simultaneously
searches MAM for the matching epub. If found, the epub is queued at `priority: high`
alongside the audiobook — epubs are 2–5 MB and unlock Stage 2 full enrichment, forced
alignment, Glossary, Sleep Recovery, and series recaps. `books.epub_status` is updated to
`found` or `not_found`. If no epub is found the m4b is downloaded regardless and Stage 2
runs Path B (lite enrichment from reviews and metadata). Epub absence is never a blocker.

#### The Monitoring Queue

Approved books sit in `download_queue` at `status: monitoring` until Windlass has
sufficient resources to download them. The drain loop evaluates:

1. **Disk space floor** not breached
2. **MAM ratio** healthy — or book is freeleech (`freeleech_window_end` set)
3. **Pipeline depth** not already deep (freeleech bypasses this check)
4. **Priority order:** `critical` (series) → `high` (strong match / freeleech) →
   `normal` → `low`

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

- **The Interface:** A standard 1 to 5 star rating scale, accompanied by a single free-text
  box prompted with _"What did you think?"_ (or _"What went wrong?"_ during a Bailout).
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
button opens a vibe query input directly from this display.

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

| Category           | Examples                                                               |
| ------------------ | ---------------------------------------------------------------------- |
| `genre`            | `hard_scifi`, `space_opera`, `romantasy`, `epic_fantasy`, `litrpg`    |
| `mood`             | `funny`, `hopeful`, `dark`, `tense`, `cozy`, `atmospheric`, `emotional` |
| `tone`             | `dry_wit`, `banter_heavy`, `slow_burn`, `action_packed`, `satirical`  |
| `style`            | `puzzle_solving`, `character_driven`, `world_building_heavy`          |
| `content_warning`  | `sexual_content`, `graphic_violence`, `death`, `trauma`, `war`        |
| `length`           | `short` (<8h), `medium` (8–15h), `long` (15–25h), `epic` (>25h)      |
| `format`           | `standalone`, `series_complete`, `series_ongoing`                     |
| `protagonist`      | `solo_protagonist`, `ensemble_cast`, `morally_grey`, `found_family`   |

**Sourced tags** come directly from Audnexus and Hardcover, normalised to canonical slugs
at acquisition time. **LLM-enriched tags** are derived from the book description at
acquisition time — the LLM assigns tags from categories that the source APIs do not cover
(tone, style, protagonist, detailed length).

**Controlled growth:** If the LLM encounters a quality no existing tag covers, it proposes a
new tag with a `canonical_name` and `description`. Windlass runs a normalisation check
against the existing registry before minting — a proposed tag is only added if it is
genuinely distinct. Users can review, rename, merge, or retire tags via Panel 6.

#### Profile Scores

The user profile is a flat table of scored dimensions in `profile_signals`. Each row is one
dimension: a tag, an author, or a narrator. Scores range from **-100 to +100** with 0
meaning no opinion:

| Score range | Meaning |
|---|---|
| -100 → -60 | Hard avoid / near-dealbreaker |
| -59 → -20 | Lean away |
| -19 → +19 | Neutral |
| +20 → +59 | Prefer |
| +60 → +100 | Love it |

There are no separate "hard constraint" and "soft preference" tables — a `content_warning`
tag scored at -85 is functionally a dealbreaker. The same scale covers everything.

#### The Three Call Types

**Learn** — runs after every review submission.

*Input:*
1. Full `profile_signals` table (all current scores)
2. The new review: book's tag scores + star rating + free-text
3. `completion_ratio` from `reading_ledger` (fast finish amplifies signal; slow finish dampens it)
4. `mood_snapshot_json` stored at listen-start (prevents mood-inflated ratings from over-writing the base profile)
5. Last 5 finished books: tags + rating + one-line review
6. Last 3 DNF books: tags + stated reason

*Output:* a JSON delta of only the dimensions that changed, e.g.
`{"puzzle_solving": +8, "emotional": -5, "narrator:Ray Porter": +3}`.
Windlass merges the delta into `profile_signals` and stores the previous version for
Panel 6's optional re-calibration feature.

---

**Mood Inference** — runs at every queue pick and after every review.

*Input:*
1. Current date and month, with explicit prompt instruction to consider season as a
   potential factor — only if the data supports it, not as an assumption
2. Cross-book velocity delta: total listening hours in the last 7 days vs the previous 7 days
3. Time-of-day session pattern: which `time_of_day_bucket` slots dominate this week vs history
4. Last 5 finished books + last 3 DNF books (tags, ratings, review text)
5. Same-period historical reviews: all reviews where `finished_at` falls within ±4 weeks of
   today's calendar date in any prior year — included with a recency-weighting instruction
   so the LLM can detect (or dismiss) seasonal patterns without algorithmic pre-processing
6. Any active explicit vibe input or Vacation Mode flag

*Output:*
```json
{
  "inferred_modifiers": { "short": 35, "emotional": -20, "dark": -30 },
  "inferred_context": "Listening time down ~60% this week — favouring shorter, lower-commitment picks",
  "explicit_override_decay": 0.65
}
```

Inferred modifiers are recomputed fresh at every queue pick — they are never stored with
a decay function. **Explicit overrides** (vibe query, "Change mood" button) are stored
separately in `mood_state` and multiplied by 0.65 after each queue pick, dropping off the
table when `|score| < 5`. This gives explicit user intent a natural 4–5 pick fade without
a jarring cutoff.

The `inferred_context` string is displayed in Panel 6 so the user always knows why the
queue shifted. It is never shown as a notification — it is ambient information.

---

**Decide** — runs at every queue pick, freeleech scoring, vibe query, and DNF palate
cleanser request.

*Input:*
1. Full `profile_signals` table
2. Current `mood_state` (inferred modifiers + any active explicit override, combined)
3. Task context: candidate book list with their tag scores, or a free-text vibe query
4. For queue picks: pipeline depth tier

*Output:*
```json
{
  "picks": [
    {
      "book_id": "abc123",
      "score": 0.91,
      "reason": "Matches your appetite for dry wit and brisk pacing after a heavy fantasy run. Short enough for a work week."
    }
  ]
}
```

The `reason` field is the blurb. There is no separate blurb generation call — every Decide
call produces both a decision and an explanation in one response. If the queue addition is
silent (series continuation), `reason` is stored in `book_blurbs` but never surfaced. If it
fires an Action notification, `reason` is the notification body. The routing is Windlass's
decision, not the LLM's.

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
As soon as the epub is on disk, the main Windlass server runs an async enrichment job:
1. The epub text is chunked by act and sent to the configured LLM provider (§7.5) with
   a summarisation prompt. The output — a structured act-by-act prose summary — is stored
   stored at the path in `books.enrichment_summary_path` (`windlass_data/enrichment/{book_id}.json`). Never shown to the user.
2. A second LLM call receives the full summary and produces detailed tag intensity scores,
   updating `books.tag_scores_json` with high-confidence scores across all categories.
3. `enrichment_stage` is set to `post_download_full`. The book becomes eligible for Active
   Queue promotion immediately — it does not wait for the Worker Node.

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

In both cases, `books.tag_scores_json` is updated in place. The book then becomes eligible
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
- **Current mood** — displays the active `inferred_context` string so the user can see
  why the queue has shifted. Shows any active explicit override with its remaining decay
  strength. The user can clear the inferred mood or set a new explicit override here.
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
  button that opens an inline vibe query input
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
