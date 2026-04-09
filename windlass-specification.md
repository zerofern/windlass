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
runs silently, makes decisions autonomously, and nudges the user via Gotify at exactly the
right moment with a deep-link to a focused UI card. The web UI (Action Center) is a
**pipeline oversight tool** — for users who want to check what is queued, adjust priorities,
or add something manually. It is not the primary interaction surface.

---

## 2. Architectural Foundation

- **Paradigm:** Strict Functional Core, Imperative Shell (FCIS) / Sans I/O.
  - *Functional Core:* A pure, synchronous state machine that makes all decisions without
    side effects. Receives an `Event`, returns `(SystemState, Vec<Action>)`.
  - *Imperative Shell:* The async Tokio layer that executes API calls, reads files, manages
    Docker sockets, and feeds events back to the Core.
- **Dynamic Docker Discovery:** Automatically identifies dependent containers attached to
  the `service:gluetun` network namespace via bollard.
- **Resilient Network Sync:** Detects VPN drops, frozen NATs, and silent port-sync
  failures. Automatically coordinates stack restarts.
- **Automated Crash Dumps:** Extracts the last 100 log lines from the VPN and all
  dependent containers into a unified dump file upon critical failures.
- **VPN IP Compliance (MAM Rule 1.2):** Gluetun is locked to a single static server
  registered with MAM staff. Windlass monitors the VPN IP and alerts on unexpected changes.

---

## 3. Data Persistence

All persistent state is stored in a local SQLite database (`windlass.db`). There is no
flat JSON state file.

### Table overview

`books` is the canonical library record for every title Windlass knows about — whether it
downloaded the file, discovered it in ABS from a manual add, or just fetched metadata while
evaluating a series. Everything else hangs off `books`.

| Table | Contents |
|---|---|
| `books` | Every title Windlass knows about. Library status lifecycle (`known → queued → downloading → seeding → completed`), source (`manual_abs`, `windlass_download`, `ai_suggestion`, `freeleech`), Audnexus ASIN, Hardcover ID, ABS item ID. A book record survives disk deletion. |
| `series` | Series identity and health (Audnexus data, user started/following flags) |
| `torrents` | File data once a download starts: qBittorrent hash, seed time, HnR status, ratio, disk path |
| `download_queue` | Thin table: books actively in the approval/download funnel only. Holds priority, stage (`suggested` / `approved` / `upcoming`), AI score, MAM torrent ID. Row deleted once `books.library_status` advances to `downloading`. |
| `reading_ledger` | One row per listening attempt (supports re-reads). Retained permanently after disk deletion. |
| `listening_progress` | Sub-daily poll snapshots from ABS. Used for slog detection and pipeline depth calculation. |
| `reviews` | User feedback rows keyed by ledger entry: completion review, optional midway note, DNF autopsy. |
| `book_blurbs` | LLM-generated pitch blurbs with FK to `books` and a `context_snapshot_json` field so we know what the model saw when it generated the text. |
| `slog_detector` | Pacing stall detection state per active ledger entry. |
| `series_check_ins` | Records of the 60–75% series check-in: what was offered, what the user chose. |
| `profile_preferences` | Stable identity layer: flexible tag-style rows (e.g. `narrator:Ray Porter = love`, `genre:LitRPG = avoid`). |
| `profile_signals` | Dynamic LLM-updated weights per dimension. Updated after each listening session. |
| `alerts` | All fired alerts. UUID primary key for Gotify deep-links. Severity, timestamp, triggering event, system state snapshot. |

### JIT (Just-In-Time) Context Injection

Rather than passing the entire reading history to the LLM, the shell queries SQLite for the
5 most recently read books and 5 highest-rated books in the matching genre, injecting only
this hyper-relevant data into the prompt.

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
  continuously polls qBittorrent for torrents that have *not yet* reached 72 hours of seed
  time. If the active unsatisfied count approaches the limit, all new automated downloads
  are paused. Furthermore, Windlass monitors qBittorrent's global maximum active,
  downloading, and seeding limits. To prevent H&R violations caused by qBittorrent parking
  torrents when limits are reached, Windlass actively orchestrates the queue, temporarily
  pausing fully satisfied torrents to guarantee unsatisfied torrents remain actively seeding.
- **MAM HnR Compliance Monitor (Rules 2.5 & 2.7):**
  - *No Partials:* Forces qBittorrent to download 100% of torrent contents.
  - *HnR Lock:* Auto-eviction is mathematically prohibited from deleting any torrent that
    has downloaded data until `seed_time ≥ 72 hours`.
  - *Safe Deletion:* Stalled or dead torrents are only automatically deleted and
    blacklisted if they have downloaded exactly 0 bytes.
- **The Vault Guardian:** Windlass monitors the MAM Millionaires Vault. When a new vault
  cycle reaches 20,000,000 BP, Windlass checks if the user's global ratio is ≥ 1.05. If
  eligible, it fires a Gotify notification: *"The Millionaires Vault is open. Click here to
  donate 2,000 BP and secure your Freeleech Wedges."* To strictly comply with MAM's rules
  against automated scripts, the system will never execute the donation via headless
  background scripts; it requires the user's explicit click via the notification deep-link.
- **qBittorrent Configuration Validator & Auto-Tuner:** Windlass does not just send
  torrents to the client; it actively manages the client's internal configuration via the
  WebAPI to ensure optimal throughput and strict tracker compliance. Enforcement is tiered
  by risk level:
  - *Port Forwarding:* Always silently auto-updated as part of the core VPN sync loop. No
    notification required.
  - *Privacy Settings — DHT, PeX, Local Peer Discovery (Rule 6.1):* These carry an
    immediate ban risk on private trackers. If any are detected as enabled, Windlass
    auto-reverts them immediately and fires a Gotify notification: *"DHT was re-enabled in
    qBittorrent — I've corrected it."* The intervention is logged. This does not wait for
    user confirmation.
  - *Queue Limits — `max_active_downloads`, `max_active_uploads`, `max_active_torrents`:*
    Windlass first attempts to work around restrictive limits via queue orchestration
    (pausing satisfied torrents, reordering priorities) without touching qBittorrent's
    config. If the limits are so low that orchestration cannot prevent an H&R violation,
    Windlass escalates: it auto-corrects the setting and fires a high-priority Gotify alert
    explaining exactly what was changed and why — *"Your max active torrents was set to 5.
    With 12 unsatisfied torrents, H&R violations were unavoidable. I've raised it to 25."*
- **Upload Health Math (Rule 1.4):** Enforced before queueing new downloads:
  - Global Ratio must remain ≥ 2.0 (well above the 1.0 minimum).
  - Upload credit buffer must remain ≥ 25 GB.
- **Disk Space Management:** Monitors the mounted volume continuously. Disk management
  operates at two levels:

  *Automatic (silent):* If free space drops below a hard floor threshold, Windlass
  immediately auto-evicts the lowest-value HnR-satisfied torrents (completed + low rating
  + longest time since last play) without user input. This is the emergency brake.

  *User-directed (proactive):* When projected free space over the next month (based on
  expected downloads) drops below a configurable buffer, Windlass sends a Gotify
  notification with a deep-link to a deletion suggestion card:

  > *"Windlass has 47 GB free. To comfortably fit this month's queue, we need ~80 GB.*
  > *Here are the best candidates to remove — confirm to free the space."*

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

- **Base Rules:** Evaluates seeders, Freeleech status, and file format (preferring M4B).
- **Custom Formats (Radarr-Style):** Users define custom Regex or keyword weights in the UI
  (e.g., +50 if title contains "Ray Porter", −100 if title contains "Abridged").

---

## 6. Media & Series Intelligence

- **Audiobookshelf (ABS) Sync:** Continuously polls ABS for playback progress and triggers
  library scans upon completed downloads.
- **Pipeline Depth Management:** Windlass continuously tracks pipeline depth — the total
  hours of approved, ready-to-play content across the Download Queue and the In Library
  (Unread) panel. This is the primary metric governing acquisition aggressiveness:

  | Depth | State | Acquisition behaviour |
  |---|---|---|
  | < 1 week of listening | Thin | Aggressive — push curated recs, grab all strong freeleech matches |
  | 1–4 weeks | Healthy | Normal — curated recommendations only |
  | > 4 weeks | Deep | Conservative — only exceptional scores or pure freeleech |

  *"1 week of listening" is computed from the user's rolling average listening velocity.*

- **Predictive Series Syncing:** For a series the user has already started and rated
  positively, the next entry is automatically queued and downloaded in the background with
  no approval required. The check-in timing is **dynamic** — it fires when the estimated
  time remaining in the current book equals the time needed to acquire the next book plus
  a safety buffer. In practice:
  - Next book already on disk → check-in at ~1 day estimated remaining (confirmation only)
  - Next book needs downloading → check-in at ~2 days estimated remaining
  - Next book availability unknown → check-in at ~3 days estimated remaining
  - Hard limits: never before 30% remaining; never after 90% remaining

  The check-in is a Gotify notification with a deep-link to a card offering:
  **Continue series** · **Pause series** · **Skip to Book N+2** · **Find me something else**

  If the user ignores the check-in, Windlass assumes continuation and ensures the next book
  is ready. The check-in is only decision-critical when the next book has not yet been
  pre-fetched.

- **Finish-Book Notification:** When ABS marks a book complete, a single Gotify notification
  delivers both the debrief prompt and the "what's next" suggestion:

  > *"You finished [Book Title].*
  > ★ [tap to rate]*
  > *Next up: [Book N+1] · [duration] · [format] · ready to play.*
  > **[Continue]   [I want something different]**"

  The "Sell It To Me" pitch for the next book is generated here, fresh, using the just-
  completed book as context. If nothing is queued, the notification prompts the user to
  open the vibe query instead.

- **Audnexus API Integration:** Provides perfect series ordering, blurbs, and release dates
  from the public `api.audnex.us` endpoint (wraps Audible's database).
- **Auto-Queuing:** For a series the user has already started and rated positively, Windlass
  silently queues and downloads successive entries without requiring approval. For a new or
  untrusted series the book appears in the Action Center for approval first.
- **Release Calendar:** Tracks upcoming release dates for incomplete series via Audnexus
  and displays them in the Action Center's "Upcoming in Series" panel.

---

## 7. The AI Librarian Engine

### 7.1 Pre-Download Intelligence

#### Universal Input Box

> A single entry point for all book discovery — search, paste, or describe.

The Action Center header contains a single universal input field. The system auto-detects
intent:

- **Direct MAM torrent URL:** Queued immediately with no LLM call. The book card is created
  from Audnexus metadata and placed directly in the Download Queue.
- **Audible or ABS URL:** Metadata is resolved, Series Health is run if it is
  Book 1 in a series, a "Sell It To Me" pitch is generated when the card is opened, and
  the card appears in Suggested Next Listens for approval.
- **Author / title search:** Queries MAM, returns ranked results. User picks one — same
  flow as URL paste.
- **Vibe query** (e.g. *"short snarky sci-fi under 10 hours"*): LLM picks the best match
  from MAM or the existing ABS library. Result card appears in Suggested Next Listens.

While the LLM is processing, the card shows an "Analysing…" state and populates in place.

#### The "Series Health & Slog" Forecaster

> Protects the user from investing time in dead, meandering, or genre-shifting series.

**Execution:** When a Book 1 URL is pasted, the shell fetches metadata for Book 1, the
middle book, and the latest published book. The LLM analyses aggregated reviews for the
entire series and returns a `SeriesHealthReport` JSON.

**UI Integration:**
- Alerts if the series is incomplete and abandoned *(anti-Name of the Wind protocol)*
- Flags "Authorial Drift" if later books radically shift tone or genre
- Generates a visual "Pacing Map," highlighting middle books in yellow or red if critical
  consensus deems them a slog

#### The "Sell It To Me" Custom Pitch

> Replaces generic publisher blurbs with personalised, mood-aware justifications.

**Timing:** Pitches are generated **just-in-time at the moment of decision** — never
pre-stored and served cold. A pitch generated weeks before the user encounters a book does
not account for their current mood, what they just finished, or how much energy they have.

**Context injected at generation time:**
- The last 1–2 books the user finished and their ratings
- Current listening velocity (high = in the zone; low = might need a change of pace)
- Time of day and approximate season
- How long the book has been waiting in the queue

**Delivery:** Pitches appear in Gotify notification cards (series check-ins, recommendations,
freeleech alerts) and in individual book cards when opened in the Action Center. They are
*not* shown in the queue list view — that surface is for pipeline management, not decisions.

#### Vibe-Based Query Engine (The "Palate Cleanser" Search)

> Natural language search across the media library.

**Execution:** A text box in the UI accepts prompts like *"I need a short, snarky sci-fi
palate cleanser under 10 hours."* The LLM parses the prompt, scans the existing ABS library
(or upcoming MAM releases), and returns the single best match.

---

### 7.2 Active Listening Support

#### The Glossary Generator

> An on-demand, spoiler-free cheat sheet for dense sci-fi/fantasy world-building.

**Execution:** When a user is confused by factions or physics (e.g., in *Blindsight*), they
click "Generate Glossary." The LLM is strictly prompted with the user's current chapter
progress and generates a structured Dramatis Personae and term glossary — barring any plot
points beyond the user's current timestamp.

#### "Previously On…" Series Recaps

> Refreshes the user's memory when starting a sequel after a long real-world gap.

**Execution:** When Windlass queues the next book in a series, it triggers a background job
to summarise the previous books. The LLM writes a punchy, 3-paragraph recap tailored to the
user's preferred tropes (e.g., focusing on political maneuvering).

**Delivery:** The recap is displayed as a card in the Action Center when book N+1 is queued,
and sent as a Gotify notification with a deep-link. It is *not* injected into ABS metadata —
the recap lives in Windlass' own domain and can be regenerated or dismissed at any time.

---

### 7.3 Post-Listening & Recovery

#### The DNF "Bailout" Protocol

> Automated momentum recovery after abandoning a massive epic.

**Execution:** Triggered by an ABS webhook when a book is marked DNF. The core immediately
searches the library/tracker for a highly-rated, sub-12-hour, fast-paced standalone
("Competence Porn"). It downloads it automatically and sends a Gotify notification: *"I see
you bounced off that epic. I've queued up a fast-paced palate cleanser to get your momentum
back."*

#### The Universal Review Component

> A standardized, single-interface review system used across all interactions to ensure a
> consistent user experience and clean data ingestion.

**Execution:** Whether a user finishes a book, triggers a DNF, completes the onboarding
wizard, or marks a suggested book as "Already Read," they are presented with the exact same
UI card.

- **The Interface:** A standard 1 to 5 star rating scale, accompanied by a single free-text
  box prompted with *"What did you think?"* (or *"What went wrong?"* during a Bailout).
- **The Pipeline:** The raw text and rating are saved permanently to the `reviews` and
  `reading_ledger` tables. The LLM ingests this payload, extracts the underlying sentiment,
  and automatically adjusts the dynamic weights in the `profile_signals` table. For finished
  or DNF'd books, submitting the review seamlessly transitions the user to the "what's next"
  suggestion or Bailout protocol. If pipeline depth is below the healthy threshold after a
  book is marked complete, acquisition aggressiveness increases immediately.

#### The Listening Velocity Monitor (The "Slog Detector")

> Proactively detects waning interest based on listening habits — before an official DNF.

**Execution:** The shell polls the ABS API daily to calculate "Listening Velocity" (average
minutes per day per book). If velocity on a specific book drops significantly below the
user's baseline for 3+ consecutive days, the core flags a `Pacing_Stall` state.

**LLM Magic:** Windlass sends a Gotify notification with a deep-link to a UI card:
*"Your listening pace on [Book Title] has dropped by 80%. Is it dragging?"* The card
presents three options:

| Option | Meaning | System behaviour |
|---|---|---|
| "See what's ahead (spoiler-free)" | Evaluating the book | LLM assesses upcoming pacing and advises |
| "Trigger Bailout Protocol" | Done with this book | Mark DNF, find a palate cleanser |
| "I'm just busy right now" | Life, not the book | Snooze the detector for this book for 5 days; no profile weight changes |

After a "just busy" response, the detector will not re-fire for that book for 5 days.

---

### 7.4 Tracker Economy

#### The MAM "Freeleech" Scavenger

> Maximises MAM economy (ratio/buffer) while discovering zero-risk reads.

**Execution:** When MAM publishes a new free books list, Windlass scrapes it and evaluates
each title against `user_profile`. Pipeline depth determines the **selectivity threshold**
for what counts as a good enough match — it is not a cap on quantity. If the LLM scores 11
books as strong matches in a given month, all 11 are queued. A great month should be fully
exploited: next month might only yield 2 good titles, so a healthy buffer now protects
against that.

- **Thin pipeline** (< 1 week): threshold is relaxed — queue borderline matches too
- **Healthy pipeline** (1–4 weeks): curated — only strong profile matches
- **Deep pipeline** (> 4 weeks): conservative — only exceptional matches

Approved torrents are snatched, seeded for upload credit, and added to a "Free Discovery"
collection in ABS. The Freeleech Scavenger panel in the Action Center shows the current
batch; the user can override individual approvals before snatching begins.

---

### 7.5 Supported LLM Engines (Privacy-First)

Both options guarantee zero data training.

| Option | Engine | Cost | Notes |
|---|---|---|---|
| **A (Recommended)** | Cloudflare Workers AI | Free (10,000 Neurons/day) | OpenAI-compatible endpoints; Llama 3.3 70B or DeepSeek R1 |
| **B** | Google AI Studio (Gemini) | ~$0.25/month | Natively supports Structured Output JSON schemas |

---

## 8. The Control Plane (Web UI)

- **Embedded Web UI:** A responsive dashboard (mobile + desktop) served directly from the
  Rust binary via axum and rust-embed. Built with React + Vite + shadcn/ui.
- **Gotify Integration:** Delivers push notifications for critical NAT errors, series
  check-ins, and prompts to review finished media. Each notification deep-links to the
  relevant UI card. All alerts are persisted to the database with a unique ID, severity,
  timestamp, triggering event, and system state snapshot.
- **Real-time Updates:** The UI subscribes to a Server-Sent Events (SSE) stream for live
  state updates and event/action history. Commands (manual reset, queue actions) are sent
  via REST POST.

### Onboarding: The Librarian Interview

On first boot, Windlass performs a guided profile initialisation:

1. **Library Import & Rating Wizard:** Windlass scans the existing Audiobookshelf library.
   It presents the user with a UI to quickly process their existing books. The user can
   assign a star rating and leave a free-text review (using the Universal Review Component)
   for any title they wish.
2. **Dealbreakers & Preferences:** The wizard explicitly asks the user for hard boundaries,
   dealbreakers, and core preferences (e.g., "No LitRPG", "Must have a single narrator").
3. **LLM Profile Generation:** The LLM ingests all the free-text comments, ratings, and
   dealbreakers to generate the initial `profile_signals` database weights.

### Action Center

The Action Center is a **pipeline oversight panel** — not the primary interaction surface.
Most users will interact with Windlass primarily through Gotify notifications and never need
to open it. It exists for users who want to inspect the current state of their pipeline,
adjust priorities, or add something manually.

It is organised into seven panels.

#### 1. Suggested Next Listens

AI-curated recommendations based on `user_profile`, reading history, and current mood
signals. Also the landing zone for books discovered via the universal input box (URL paste,
search, vibe query). Each card in the list shows book cover, title, author, narrator,
duration, format badge, and series health badge (if applicable). **"Sell It To Me" pitches
are not shown in the list** — they are generated fresh when the user opens a specific card
or receives a notification, ensuring the pitch reflects current context.

- **The "Already Read" Workflow:** To easily build history without downloading known books,
  every AI-curated card features an **"Already Read"** action button alongside *Approve*,
  *Reject*, and *Snooze*. Users can also paste an external URL (e.g., Audible) into the
  Universal Input Box and tag it as "Already Read".
- **Immediate Capture:** Clicking "Already Read" instantly opens the Universal Review
  Component. The rating and text are injected directly into the `reading_ledger`, and the
  LLM uses it to immediately refine the user's profile.

Actions per card: **Approve** (sends to Download Queue) · **Reject** · **Snooze** · **Already Read**

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

Populated on the 1st of each month with MAM's free books. Each card carries a "FREELEECH"
badge and a personalised pitch. The user queues individual titles or dismisses the panel.
Approved torrents are snatched, seeded for upload credit, and added to a "Free Discovery"
collection in ABS.

#### 5. In Library — Unread

Books already present in ABS but not yet started. Ensures there is always something ready
to listen to next. Cards use the same format as Suggested Next Listens. Windlass alerts if
this panel is empty and the Download Queue is also empty.

#### 6. User Profile Dashboard

A dedicated control panel exposing the user's LLM profile exactly as it exists in the
database. It displays the core `profile_preferences` (tag-style rows) and `profile_signals`
(dynamic weights per dimension). Users can manually edit these raw key-value pairs or
sliders directly. This ensures the user is always in total control of the AI's logic,
without relying exclusively on inferred data.

#### 7. Reading Ledger & Reviews

A historical, searchable catalog displaying all data from the `reading_ledger` and `reviews`
tables.

- Users can revisit old books, read their past free-text reviews, and retroactively adjust
  ratings.
- **Optional Re-calibration:** When a user edits a past review or rating, Windlass does
  *not* automatically overwrite the profile. Instead, it presents a prompt: *"Do you want
  to re-calibrate your AI profile based on these changes?"*

---

## 9. Deferred Features

Features confirmed for the roadmap but not yet scheduled for implementation.

- **The "Novella Navigator" (Smart Reading Order):** Determines if fractional series entries
  (e.g., Book 1.5) are essential lore or skippable cash-grabs. Deferred until ABS +
  Audnexus integration is fully stable.
- **Custom Format Weights (Radarr-Style):** User-defined regex or keyword score adjustments
  in the UI (e.g., +50 for "Ray Porter", −100 for "Abridged"). Deferred until the scoring
  engine is in place.