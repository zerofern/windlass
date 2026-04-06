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

### Core tables

| Table | Contents |
|---|---|
| `user_profile` | Core identity, dealbreakers, dynamic genre weights |
| `reading_ledger` | Completed books, user ratings, parsed DNF reasons |
| `download_queue` | Pending torrents, AI scores, metadata |
| `alerts` | All fired alerts with ID, severity, timestamp, triggering event, system state snapshot |

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
- **Unsatisfied Quota Manager (Rule 2.8):** The core tracks the user's Class Limit (e.g.,
  50 for User, 100 for Power User). Windlass continuously polls qBittorrent for torrents
  that have *not yet* reached 72 hours of seed time. If the active unsatisfied count
  approaches the limit, all new automated downloads are paused until slots free up.
- **MAM HnR Compliance Monitor (Rules 2.5 & 2.7):**
  - *No Partials:* Forces qBittorrent to download 100% of torrent contents.
  - *HnR Lock:* Auto-eviction is mathematically prohibited from deleting any torrent that
    has downloaded data until `seed_time ≥ 72 hours`.
  - *Safe Deletion:* Stalled or dead torrents are only automatically deleted and
    blacklisted if they have downloaded exactly 0 bytes.
- **Upload Health Math (Rule 1.4):** Enforced before queueing new downloads:
  - Global Ratio must remain ≥ 2.0 (well above the 1.0 minimum).
  - Upload credit buffer must remain ≥ 25 GB.
- **Disk Auto-Eviction:** Monitors the mounted volume. If free space drops below a defined
  threshold, Windlass automatically deletes the oldest HnR-satisfied torrents.

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
- **Predictive Series Syncing:** When a user is actively listening to a series, the next
  entry is automatically queued and downloaded in the background — no notification, no
  approval required. The goal is that book N+1 is always ready before the user finishes
  book N. At 60–75% progress through the current book, Windlass sends a brief check-in
  (deep-link to a UI card): *"You're getting close to the end of [Book]. Still enjoying the
  series? Book N+1 is queued and ready."* If the user signals they're done with the series,
  auto-queuing stops.
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

#### The DNF "Autopsy" Interview

> Self-updating profile weights through conversational feedback.

**Execution:** After a DNF, Windlass sends a Gotify notification with a deep-link to a
feedback card in the UI: *"What went wrong with [Book Title]?"* The user opens the card and
types a short response (e.g., *"Just a pacing slog, no real plot."*). The LLM parses this,
updates the negative/positive weights in `user_profile`, and refines future search parameters.

#### The Post-Book Debrief

> Captures immediate user sentiment upon finishing a book.

**Execution:** When the ABS webhook fires a "Completed" event, Windlass sends a Gotify
notification with a deep-link to a rating card: *"You finished [Book Title]. Quick rating
and any thoughts?"* The user opens the card, rates 1–5 stars, and optionally adds a note.
The LLM parses the note, logs the rating into `reading_ledger`, and extracts specific
feedback to tweak `user_profile` weights for future RAG prompts.

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

#### The Monthly MAM "Freeleech" Scavenger

> Maximises MAM economy (ratio/buffer) while discovering zero-risk reads.

**Execution:** A cron job fires on the 1st of every month to scrape the MAM "Free Books of
the Month" list. Since freeleech costs zero ratio, the LLM receives relaxed instructions:
*"Discard absolute dealbreakers, but aggressively queue anything that aligns with
'Competence Porn' or 'Sci-Fi/Fantasy' preferences."* Approved torrents are snatched,
seeded for upload credit, and added to a "Free Discovery" collection in ABS — each with a
personalised "Sell It To Me" pitch.

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
- **Client Validation (MAM Rule 2.3):** Checks the qBittorrent version against the approved
  client list and displays a warning in the UI if an unsupported/beta version is detected.
- **Gotify Integration:** Delivers push notifications for critical NAT errors, series
  check-ins, and prompts to review finished media. Each notification deep-links to the
  relevant UI card. All alerts are persisted to the database with a unique ID, severity,
  timestamp, triggering event, and system state snapshot.
- **Real-time Updates:** The UI subscribes to a Server-Sent Events (SSE) stream for live
  state updates and event/action history. Commands (manual reset, queue actions) are sent
  via REST POST.

### Onboarding

On first boot, Windlass performs two-step profile initialisation:

1. **ABS Library Analysis:** Scans the existing Audiobookshelf library to infer genre
   preferences, preferred lengths, and narrator affinities from what is already present.
2. **Short Questionnaire (5 questions max):** Displayed on first open of the UI to capture
   dealbreakers, preferred genres, and tone preferences that the library scan cannot infer.

Subsequent DNF Autopsies and Post-Book Debriefs continuously refine the profile over time.

### Action Center

The Action Center is a **pipeline oversight panel** — not the primary interaction surface.
Most users will interact with Windlass primarily through Gotify notifications and never need
to open it. It exists for users who want to inspect the current state of their pipeline,
adjust priorities, or add something manually.

It is organised into five panels.

#### 1. Suggested Next Listens

AI-curated recommendations based on `user_profile`, reading history, and current mood
signals. Also the landing zone for books discovered via the universal input box (URL paste,
search, vibe query). Each card in the list shows book cover, title, author, narrator,
duration, format badge, and series health badge (if applicable). **"Sell It To Me" pitches
are not shown in the list** — they are generated fresh when the user opens a specific card
or receives a notification, ensuring the pitch reflects current context.

Actions per card: **Approve** (sends to Download Queue) · **Reject** · **Snooze**

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

---

## 9. Deferred Features

Features confirmed for the roadmap but not yet scheduled for implementation.

- **The "Novella Navigator" (Smart Reading Order):** Determines if fractional series entries
  (e.g., Book 1.5) are essential lore or skippable cash-grabs. Deferred until ABS +
  Audnexus integration is fully stable.
- **Custom Format Weights (Radarr-Style):** User-defined regex or keyword score adjustments
  in the UI (e.g., +50 for "Ray Porter", −100 for "Abridged"). Deferred until the scoring
  engine is in place.