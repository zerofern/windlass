# Librarian Readiness Work

This document tracks the work needed to move Windlass from a pure operator
(VPN/qBit/MAM plumbing) into a librarian: a system that autonomously snatches
torrents from a curated source and organises completed downloads into a media
library for downstream consumers like Audiobookshelf.

The scope here is the librarian/downloader layer only: the autograbber and the
linker. Operator-readiness primitives (admission control, HnR lock, no-partials,
upload health, account heartbeat) are consumed via cross-references — this doc
never re-owns them.

Inspiration for the initial behaviour is the user's existing MLM setup. The
first librarian milestone is intentionally minimal: bookmarks-only autograb,
symlink-only linking, ABS metadata push. Configurable formats, multi-grabber
config, the cleaner, and the broader format-preference duplicate handling are
deferred until the minimal path is paved.

## Goal

A Windlass librarian should be usable soon for the user's actual library:

- Periodically scan the user's MAM bookmarks and snatch the new ones safely.
- Choose the cheapest available cost (VIP/freeleech first, then a freeleech
  wedge, never a ratio hit).
- Refuse to snatch under any operator-unsafe condition (admission gate from
  operator-readiness §29).
- Symlink completed downloads into a per-category library directory mirroring
  the user's MLM layout (`Audiobooks` → audiobook library, `Ebooks` → ebook
  library).
- Generate ABS-compatible metadata and push it to the user's Audiobookshelf
  instance on every link.
- Detect when qBit, MAM, or the library on disk drifts out of sync, and
  surface (not silently swallow) the mismatch.

## Scope and Deferrals

**In scope (this doc):**

- Bookmark autograb (single hardcoded grabber).
- Category-based linker with two libraries (Audiobooks, Ebooks).
- ABS integration.
- Linker state-drift tracking.

**Explicitly deferred (not in this milestone):**

- Cleaner (library-side duplicate removal). MLM's `cleaner.rs` logic.
- Format-preference duplicate handling at grab time (`A3` in discussion).
- Other autograb sources (`Freeleech`, `New`, `Uploader`, `Mine`, Goodreads).
- Metadata-only autograb modes.
- Per-grabber config (`[[autograb]]` blocks). The librarian runs one
  hardcoded bookmark grabber for now.
- Hardlink/copy link methods. Symlink-only.

The deferred items each warrant their own stories when reopened.

## Implementation Order

Implement these stories one at a time, in this order:

1. Hardcoded bookmark autograb with VIP/freeleech/wedge cost rule.
2. Bookmark autograb passes the operator §29 admission gate.
3. qBit completion watch → library match by category.
4. Library path layout + best-format file selection.
5. Symlink-only link method.
6. ABS metadata generation and API push.
7. Linker state-drift tracking (`RemovedFromMam`, library mismatches,
   category drift sync).

Stories 1-2 are the autograbber. Stories 3-7 are the linker.

The work introduces a new crate, `windlass-librarian-core`, holding two
sans-I/O machines: `AutograbMachine` (stories 1-2) and `LinkerMachine`
(stories 3-7). Both run on the existing generic service runtime.

## Cross-References

- Operator-readiness §19 (HnR seed-time lock) — every librarian-emitted
  delete-equivalent action must compose with QBIT-8.
- Operator-readiness §21 (no-partials) — every snatched torrent must download
  every file, enforced in qBit core. The librarian never overrides this.
- Operator-readiness §25 (unsatisfied-quota gate) — librarian admission must
  consult `unsatisfied_quota_full()`.
- Operator-readiness §26 (upload-health gate) — librarian admission must
  consult `upload_health_ok(freeleech)`.
- Operator-readiness §29 (fail-closed download admission control) — the
  composite predicate, into which librarian story A2 wires the bookmark grabber.
  A2 takes ownership of the three librarian-side gates §29 currently defers
  (`already-snatched`, `collection-skip`, `freeleech-timing-fits`).
- `docs/invariants.md` — all newly-implemented invariants here graduate into
  the operator invariant catalog with `AG-N` (autograb) and `LNK-N` (linker)
  tags.

## Story: Hardcoded Bookmark Autograb With Cost Rule

Status: To Do

### Problem

Windlass has no autograbber today: every snatch is operator-initiated. To
become useful as a librarian for the user's actual library, it needs a
recurring path that pulls candidates from the user's MAM bookmarks and
snatches the eligible ones.

The user's existing MLM config defines a single bookmark autograbber with a
`try_wedge` cost and a 2 GiB size cap, plus category-by-media-type rules
(audio → `Audiobooks`, ebook → `Ebooks`). The first milestone hardcodes
exactly this shape — no `[[autograb]]` config blocks, no choice of search
type, no per-grabber buffers. One grabber, one set of rules.

### User Story

As the operator user, I want Windlass to periodically scan my MAM bookmarks
and autonomously snatch each new bookmarked torrent at the cheapest available
cost (free first, then a freeleech wedge, never a ratio hit), so I don't have
to manually click through bookmarks to download them.

### Acceptance Criteria

- A new `windlass-librarian-core` crate defines an `AutograbMachine`
  implementing `Machine`, running on the generic service runtime.
- The machine schedules a recurring `BookmarkScan` timer (self-perpetuating
  chain, like qBit `TorrentRefresh` and MAM `KeepAlive`). Interval configurable;
  default 30 minutes.
- On `TimerFired(BookmarkScan)`, the machine emits one
  `AutograbAction::ListBookmarks` action and re-schedules the timer
  unconditionally (chain cannot die from a dropped event).
- The shell hits the MAM bookmarks endpoint and returns
  `BookmarksFetched { candidates: Vec<BookmarkCandidate> }`.
- For each candidate, the machine applies a **size filter** first: skip if
  `size > 2 GiB`.
- Then applies the **cost rule** in order:
  1. If `vip == true || personal_freeleech == true || global_freeleech == true`:
     emit `AutograbAction::SnatchTorrent { mam_id, cost: FreeOrVip }`.
  2. Else if `wedges_available > 0 && !already_wedged`: emit
     `AutograbAction::ApplyWedgeThenSnatch { mam_id }`.
  3. Else skip the candidate (no ratio hit).
- For each candidate that will be snatched, the machine emits a
  **category-by-media-type** action: `AutograbAction::SetCategoryOnSnatch
  { mam_id, category: "Audiobooks" | "Ebooks" }`. Audiobook torrents
  (`media_type == Audio`) get `"Audiobooks"`; ebook torrents
  (`media_type == Ebook`) get `"Ebooks"`. Torrents with both formats follow
  the audio side per the user's MLM convention.
- The librarian does **not** invent its own qBit add path; it routes through
  the qBit core's existing add-torrent path (story 29 of operator-readiness
  introduces `Action::AddTorrent`; this story consumes it).
- Add invariants to `docs/invariants.md` and cover them with property tests.

Core invariants (property tests):

```
# AG-1: every snatch follows the cost rule
for any emitted Action::SnatchTorrent { mam_id, cost } for candidate c:
  cost == FreeOrVip  =>  c.vip || c.personal_freeleech || c.global_freeleech
  cost == Wedged     =>  !(c.vip || c.personal_freeleech || c.global_freeleech)
                          && wedges_available > 0
                          && !c.already_wedged

# AG-2: size cap is total
for any emitted Action::SnatchTorrent { mam_id, .. } for candidate c:
  c.size <= 2 * 1024 * 1024 * 1024

# AG-3: category-by-media-type is total
for any emitted Action::SetCategoryOnSnatch { mam_id, category } for candidate c:
  c.media_type == Audio  =>  category == "Audiobooks"
  c.media_type == Ebook  =>  category == "Ebooks"

# AG-4 [liveness]: the bookmark-scan chain always re-schedules
TimerFired(BookmarkScan) always emits exactly one ScheduleTimer { BookmarkScan }.
```

### Implementation Notes

- The cost rule is intentionally hardcoded. When the user wants
  configurability later, this story's rule becomes the default for an
  `AutograbCost::Auto` enum value.
- The 2 GiB cap matches the user's current MLM config; raising/configuring it
  is a follow-up. Keep the constant named (`MAX_BOOKMARK_SIZE_BYTES`) for easy
  later promotion.
- The `already_wedged` check needs the MAM API surface to report whether the
  current user has already applied a wedge to this torrent. If the MAM client
  doesn't expose this yet, it must be added before this story can land.
- Audiobookshelf-specific categorisation lives here, not in the linker. The
  linker just consumes the qBit category and maps it to a library directory
  (story L1).

## Story: Bookmark Autograb Passes The §29 Admission Gate

Status: To Do

### Problem

Story A1's cost rule decides *whether the torrent is affordable*. It does not
decide *whether it's safe to snatch right now*. Without an admission gate, the
autograb could fire while the VPN is down, the qBit listen port is wrong, MAM
is unreachable, the unsatisfied quota is exhausted, or upload health is below
threshold — exactly the kinds of unsafe states operator-readiness §29 was
designed to fail closed on.

Operator-readiness §29 defines a single composite admission predicate. Three
of its named gates are explicitly listed as "owned by downloader/librarian
discovery work; consumed here as an external gate":

- `already-snatched` — `candidate.my_snatched == false`.
- `collection-skip` — `numfiles <= 20` unless `source == ManualMamUrl`.
- `freeleech-window-fits` — for freeleech candidates, `now +
  est_download_duration + safety_buffer <= freeleech_window_end`.

This story is where those three gates live, and where the bookmark autograb
gets wired through the §29 predicate.

### User Story

As the operator user, I want every autograb snatch to pass the same fail-closed
admission gate that any other autonomous snatch would pass, so the librarian
cannot snatch under an unsafe operator condition for any reason.

### Acceptance Criteria

- The `AutograbMachine` does not emit `Action::SnatchTorrent` (or the
  equivalent routed action) unless the operator §29 composite predicate holds
  for the candidate. If the predicate is false, the candidate is skipped for
  this cycle.
- The three previously-deferred gates are implemented and owned here:
  - **already-snatched**: pulled from the MAM bookmark response's
    `my_snatched` flag; skip when true.
  - **collection-skip**: pulled from the bookmark's `numfiles` field; skip when
    `numfiles > 20`. The `source == ManualMamUrl` exemption is irrelevant for
    bookmarks (a bookmark is not a manual URL) — automatic bookmark snatches
    always enforce the cap.
  - **freeleech-window-fits**: for `global_freeleech` or `personal_freeleech`
    candidates, the librarian computes
    `now + est_download_duration + safety_buffer <= freeleech_window_end` from
    the bookmark's freeleech metadata; skip when the window would expire
    mid-download. `safety_buffer` is configurable; default 30 minutes.
- The remaining §29 gates (`upload_health_ok`, `unsatisfied_quota_full`,
  `qbit_privacy_clean`, `qbit_port_synced`, `mam_health == Healthy`,
  `vpn_ip_compliant`) are consumed via the operator core's admission
  predicate; the librarian does **not** re-implement them.
- When a gate blocks, the librarian emits the operator-allowed non-snatch
  outcome: `Activity` log entry naming which gate blocked, and (for persistent
  blocks like quota-full) an alert path consistent with the operator-side
  alerts. No autonomous snatch is ever emitted.
- Update operator-readiness §29 to back-reference this story for the three
  deferred gates.
- Add invariants to `docs/invariants.md` and cover them with property tests.

Core invariants (property tests):

```
# AG-5: composite admission gate is fail-closed
for any emitted Action::SnatchTorrent for candidate c:
  upload_health_ok(c) && under_quota() && qbit_privacy_clean()
   && qbit_port_synced() && mam_health == Healthy && vpn_ip_compliant()
   && !c.my_snatched && c.numfiles <= 20
   && freeleech_window_fits(c)

# AG-6: any single librarian-owned gate false => no snatch
if c.my_snatched || c.numfiles > 20 || !freeleech_window_fits(c)
then no emitted action is Action::SnatchTorrent for c
```

### Implementation Notes

- This story blocks on operator-readiness §29 being implemented (the
  composite predicate must exist in a reusable form). If §29 is not yet ready
  when A2 is started, defer A2 and run A1 in dry-run/observation mode in the
  meantime.
- The three librarian-owned gates are simple field reads from the MAM
  bookmark response. Their property tests are total (any combination of
  state and gate inputs).
- `est_download_duration` for the freeleech-window check can be a rough
  upper-bound estimate (torrent size / a configured pessimistic throughput
  floor). Better estimates can come later.

## Story: qBit Completion Watch → Library Match By Category

Status: To Do

### Problem

To organise the library, the operator must notice when a torrent has finished
downloading and decide whether — and where — to link its files. Today the
operator's qBit core tracks torrent presence and HnR state but has no concept
of "the download just completed; act on it once". The librarian also has no
notion of where a completed torrent should land on disk.

The user's MLM config maps qBit categories to library directories:
`Audiobooks` → `/mnt/Data/Library/Audiobooks`, `Ebooks` → `/mnt/Data/Library/
Ebooks`. The librarian's first behaviour is to mirror that.

### User Story

As the operator user, I want Windlass to notice when one of my qBittorrent
torrents finishes downloading and figure out which of my library directories
it belongs in based on its qBit category, so I never have to manually move
finished downloads.

### Acceptance Criteria

- `windlass-librarian-core` defines a `LinkerMachine` running on the generic
  service runtime, subscribing to qBit `Torrents` publishes.
- The machine maintains per-known-torrent state: hash, qBit category,
  `progress`, `library_state: NotLinked | Linked { library_path } |
  Failed { reason }`.
- On a `TorrentsListed` event from qBit, for each torrent with
  `progress == 1.0` and `library_state == NotLinked`, the machine resolves a
  target library by category lookup against a configured map:
  - `Audiobooks` → configured audiobook `library_dir`.
  - `Ebooks` → configured ebook `library_dir`.
- If the category does not match any configured library, the machine emits no
  link action and records `library_state = NotLinked` with a
  `library_mismatch_reason: NoLibrary { category }` field surfaced for the UI
  (further detail belongs to story L5).
- When a library matches, the machine emits exactly one
  `LinkerAction::PlanLink { hash, library_dir }` action, which carries the
  candidate to story L2's path-and-format selection step.
- Library configuration lives in `LinkerConfig` and is the **only**
  librarian-side configuration this story introduces: two entries, one per
  category. No per-tag filters, no allow/deny lists — yet.
- Add invariants to `docs/invariants.md` and cover them with property tests.

Core invariants (property tests):

```
# LNK-1: completion is the link trigger
for any emitted LinkerAction::PlanLink { hash, .. } for torrent t:
  t.progress == 1.0
  t.library_state == NotLinked

# LNK-2: category-to-library mapping is total
for any emitted LinkerAction::PlanLink { hash, library_dir } for torrent t:
  t.category == "Audiobooks"  =>  library_dir == config.audiobook_library_dir
  t.category == "Ebooks"      =>  library_dir == config.ebook_library_dir

# LNK-3: unknown categories never link
if t.progress == 1.0 && t.category not in {"Audiobooks", "Ebooks"}
then no emitted action is LinkerAction::PlanLink { hash: t.hash, .. }
```

### Implementation Notes

- Re-linking an already-linked torrent (e.g. after a library move) is
  handled by story L5, not here. This story emits at most one `PlanLink` per
  torrent's lifetime; the chain that re-triggers on configuration drift is L5.
- Linking is benign — symlinks don't interfere with seeding — so completion
  link is **not** gated by HnR. The HnR seed-time lock guards *deletion*, not
  *linking*.
- The qBit category mapping mirrors the user's MLM convention. Adding more
  categories or per-tag filters can come as follow-up stories without
  reshaping this one.

## Story: Library Path Layout + Best-Format File Selection

Status: To Do

### Problem

A `PlanLink { hash, library_dir }` action from story L1 names *which* library
the torrent belongs in. It does not say *where inside that library directory*
the files should appear, nor *which* files should be linked. MLM solves both
with a single rule: a hierarchical `Author/Series/…` directory, and one
best-format-per-list file selection (one audio file, one ebook file).

Without this story, the linker has nothing to put down on disk.

### User Story

As the operator user, I want each linked torrent's library entry to appear
under `Author/Series/Series #N - Title [{Narrator}]/`, with exactly one audio
file and one ebook file linked from the torrent's contents, so my library
matches the structure Audiobookshelf and other consumers expect.

### Acceptance Criteria

- The linker computes a `library_path` for the torrent of the form:
  - Audiobook with series:
    `library_dir/Author/Series/Series #N - Title {Narrator}/`
  - Audiobook without series:
    `library_dir/Author/Title {Narrator}/`
  - Ebook with series:
    `library_dir/Author/Series/Series #N - Title/`
  - Ebook without series:
    `library_dir/Author/Title/`
  - `{Narrator}` is omitted when the configured
    `exclude_narrator_in_library_dir` flag is true (mirrors MLM's flag for
    booktree compatibility).
- The linker selects files from the torrent contents using ordered preference
  lists:
  - `audio_types = ["m4b", "m4a", "mp4", "mp3", "ogg"]` (default order).
  - `ebook_types = ["cbz", "epub", "pdf", "mobi", "azw3", "azw", "cbr"]`
    (default order).
- At most one file from each list is linked. An audiobook with both `m4b`
  and `pdf` links both (one from each list). An ebook with both `epub` and
  `mobi` links only the `epub`.
- Disc-number patterns (`CD\s*\d+`, `Disc\s*\d+`, `Disk\s*\d+`) in source
  filenames are preserved verbatim under the `library_path` so multi-disc
  audiobooks remain navigable.
- The action emitted is `LinkerAction::CreateSymlinks { hash, library_path,
  files: Vec<LibraryFile { source: PathBuf, target: PathBuf }> }`. Symlink
  execution is the subject of story L3.
- Add invariants to `docs/invariants.md` and cover them with property tests.

Core invariants (property tests):

```
# LNK-4: at most one audio + one ebook file per torrent
for any emitted LinkerAction::CreateSymlinks { hash, files }:
  count(f in files where mime(f) == Audio) <= 1
  count(f in files where mime(f) == Ebook) <= 1

# LNK-5: format selection respects the preference order
for any emitted LinkerAction::CreateSymlinks { hash, files } for torrent t:
  let selected_audio = files.find(Audio)
  in selected_audio is None
     or rank(ext(selected_audio), audio_types)
         == min(rank(ext(f), audio_types) for f in t.files if mime(f) == Audio)

# LNK-6: library path is deterministic from meta
for any emitted LinkerAction::CreateSymlinks { hash, library_path, .. }:
  library_path == compute_library_path(t.meta, config)
```

### Implementation Notes

- The exact `{Narrator}` formatting and series-numbering padding should match
  MLM byte-for-byte so the user's existing library, if migrated, lines up.
- The format-preference lists are not user-configurable in this milestone
  (the defaults match MLM's defaults). Making them configurable is a
  follow-up.
- "Multi-format ebook torrents only link the best format" is the source of
  the deferred `cleaner` work: the same rule applied across torrents, not
  within one torrent. We never re-implement it here.

## Story: Symlink-Only Link Method

Status: To Do

### Problem

Once the linker knows *what* files go *where*, it must actually place them.
MLM supports a method ladder (`hardlink_or_copy`, `hardlink_or_symlink`,
`copy`, `symlink`). For the first librarian milestone the user chose
symlinks only: simpler to reason about, no cross-mount surprises, and the
qBit data files keep being the source of truth for seeding.

### User Story

As the operator user, I want the linker to create the library entries as
symlinks pointing at the qBit-managed files, so seeding continues
uninterrupted and library reorganisation never touches the original
download files.

### Acceptance Criteria

- The linker's only file-placement method is `symlink`. No hardlink, no
  copy, no fallback ladder.
- For each `LibraryFile { source, target }` in a `CreateSymlinks` action, the
  shell creates `target` as a symlink to `source`. Parent directories are
  created as needed.
- If `target` already exists and points at `source`, the operation is a
  no-op (idempotent re-link).
- If `target` already exists and points elsewhere, or is a regular file, the
  shell emits `LinkerEvent::LinkFailed { hash, reason: TargetCollision }`
  and the linker records `library_state = Failed { reason }` for the
  torrent. No overwrite.
- If symlink creation fails for any other reason (permissions, missing
  parent, filesystem read-only), the shell emits
  `LinkerEvent::LinkFailed { hash, reason }` with the underlying message.
- On full success, the shell emits `LinkerEvent::LinkSucceeded { hash,
  library_path, files }` and the linker records `library_state = Linked
  { library_path }`.
- Add invariants to `docs/invariants.md` and cover them with property tests.

Core invariants (property tests):

```
# LNK-7: success and failure are mutually exclusive per attempt
for one PlanLink/CreateSymlinks attempt for hash h:
  exactly one of LinkSucceeded { hash: h, .. } or LinkFailed { hash: h, .. }
  reaches the machine.

# LNK-8: LinkFailed transitions to Failed, LinkSucceeded to Linked
on LinkerEvent::LinkSucceeded { hash, library_path }:
  post.library_state(hash) == Linked { library_path }
on LinkerEvent::LinkFailed { hash, reason }:
  post.library_state(hash) == Failed { reason }
```

### Implementation Notes

- Because there is no hardlink fallback, this milestone explicitly requires
  the user's library_dir to be on a filesystem that allows symlinks pointing
  outside it (typical for `/mnt/Data/Library/...` Linux setups). The README
  for the librarian should call this out.
- Adding `hardlink_or_copy` later is a follow-up story; this story's
  invariants do not encode "method = symlink forever" — they encode "any link
  emitted by this milestone's machine is a symlink". When more methods land,
  this invariant becomes "the emitted method matches `LinkerConfig.method`".

## Story: ABS Metadata Generation And API Push

Status: To Do

### Problem

Once the library entry exists on disk, Audiobookshelf can pick it up — but
its auto-detected metadata is often weaker than what MAM already knows.
MLM solves this by writing a `metadata.json` next to the linked files and
also calling the ABS API to update the book's metadata.

Without this story, the librarian's linked entries appear in ABS with
whatever the filename scanner managed to infer, ignoring the rich MAM
metadata we already hold.

### User Story

As the operator user, I want every torrent the linker places into the library
to also push its MAM-sourced metadata to Audiobookshelf, so my ABS library
shows accurate titles, authors, narrators, series, descriptions, and covers
without manual editing.

### Acceptance Criteria

- After a `LinkSucceeded` event, the linker emits two follow-up actions for
  the eligible torrent:
  - `LinkerAction::WriteAbsMetadata { hash, library_path, meta }` — writes
    `library_path/metadata.json` from MAM metadata.
  - `LinkerAction::PushAbsBook { hash, meta }` — calls the configured ABS
    API to update the book.
- `WriteAbsMetadata` is idempotent: if `metadata.json` already exists, it is
  merged (existing keys preserved unless the new MAM metadata supersedes
  them), then re-written.
- ABS API configuration lives in `LinkerConfig::audiobookshelf:
  Option<AbsConfig { url, token, audiobook_library_id, ebook_library_id }>`.
  When unset, both ABS actions are skipped silently (no error).
- The shell handler for `PushAbsBook` returns either
  `LinkerEvent::AbsBookUpdated { hash }` or `LinkerEvent::AbsUpdateFailed
  { hash, reason }`. The linker records `abs_state: NotPushed | Pushed |
  Failed { reason }` per torrent.
- ABS failures do **not** invalidate the on-disk link: a Failed ABS push
  leaves `library_state = Linked { .. }` untouched. The two states are
  independent.
- The metadata generation logic mirrors MLM's `audiobookshelf::create_metadata`
  shape verbatim so an existing user library migrates without re-syncing
  every book.
- Add invariants to `docs/invariants.md` and cover them with property tests.

Core invariants (property tests):

```
# LNK-9: ABS actions are emitted iff config is set
if config.audiobookshelf.is_some() && event == LinkSucceeded { hash, .. }
then emitted actions include WriteAbsMetadata { hash, .. }
                          and PushAbsBook { hash, .. }
if config.audiobookshelf.is_none()
then no emitted action is WriteAbsMetadata or PushAbsBook

# LNK-10: ABS failure does not invalidate the on-disk link
on AbsUpdateFailed { hash, .. }:
  post.library_state(hash) == pre.library_state(hash)
```

### Implementation Notes

- Cover image fetching belongs here — MLM treats the cover as part of the
  metadata write. Reuse the MAM client's cover URL if available; otherwise
  generate from torrent contents.
- The MLM-compatible metadata shape includes title, author, narrator, series,
  series number, description, publisher, published year, language, ISBN,
  and ASIN. Match the field names MLM uses so ABS reads both sources
  consistently.

## Story: Linker State-Drift Tracking

Status: To Do

### Problem

Once a torrent is linked, the librarian's view can silently desync from
qBit, MAM, or disk reality:

- qBit's tracker may report the torrent as no longer registered with MAM
  (the source bookmarked it, then the uploader pulled it). The linker should
  notice and mark the torrent `RemovedFromMam`.
- The configured `library_dir` for a category may change, leaving every
  linked entry under the old path.
- The qBit category for a torrent may be edited manually by the user,
  invalidating the path layout the linker computed at link time.
- The torrent's meta on MAM may be updated (title, series number) and the
  current `library_path` no longer matches what L2 would now compute.

Without explicit drift detection, the librarian "linked once, forgets". The
user has to spot these by hand.

### User Story

As the operator user, I want the librarian to detect when its linked library
entries drift out of sync with qBit, MAM, or the current configuration, and
surface that drift in the activity log so I can fix it, instead of silently
leaving stale library entries.

### Acceptance Criteria

- The `LinkerMachine` tracks per-torrent state: `library_path`, `category`,
  `mam_meta_fingerprint`, and `client_status`.
- On every `TorrentsListed` event, for each tracked torrent:
  - **Tracker check**: if the last tracker message is `"torrent not
    registered with this tracker"`, the linker sets
    `client_status = RemovedFromMam` and emits one
    `LinkerPublish::RemovedFromMam { hash }`. Republishes are suppressed
    after the first.
  - **Category drift**: if `t.category` (from qBit) differs from the linker's
    stored `category`, the linker records the new category and emits
    `LinkerPublish::CategoryDrift { hash, old, new }`. If the new category
    maps to a different library_dir, the torrent's `library_mismatch` is
    set to `NewLibraryDir { wanted: new_library_dir }`.
  - **Library-dir drift**: if `t.library_path` is not a prefix-of-or-equal-to
    the currently-configured library_dir for its category,
    `library_mismatch = NewLibraryDir { wanted: current_library_dir }`.
  - **Path layout drift**: if L2's `compute_library_path(t.meta, config)`
    no longer equals the stored `library_path`, `library_mismatch =
    NewPath { wanted }`.
- `library_mismatch` values are surfaced via a new `LinkerPublish::
  MismatchObserved { hash, mismatch }`. The domain core routes these to
  activity-log entries (one alert per fresh mismatch, suppressed while
  unchanged).
- No automatic re-linking is performed by this story — surfacing is the
  goal. Automatic re-link is a follow-up.
- Add invariants to `docs/invariants.md` and cover them with property tests.

Core invariants (property tests):

```
# LNK-11: RemovedFromMam is sticky and published once
on TorrentsListed with tracker_msg == "torrent not registered with this tracker"
   for torrent t with pre.client_status(t.hash) != RemovedFromMam:
  post.client_status(t.hash) == RemovedFromMam
  emitted publishes include exactly one LinkerPublish::RemovedFromMam { hash: t.hash }

on subsequent events for t while client_status(t.hash) == RemovedFromMam:
  no further LinkerPublish::RemovedFromMam { hash: t.hash } is emitted

# LNK-12: mismatch detection is total
for any emitted LinkerPublish::MismatchObserved { hash, mismatch }:
  mismatch != Matches  # the publish is only for actual mismatches

# LNK-13: rising-edge mismatch publish
LinkerPublish::MismatchObserved { hash, m } is emitted iff
  pre.library_mismatch(hash) != m  &&  post.library_mismatch(hash) == m
```

### Implementation Notes

- `mam_meta_fingerprint` is a hash of the MAM-side fields L2 consumes
  (title, series, series #, narrator). Recomputing on every observed-meta
  event is cheap and avoids tracking each field separately.
- The "no automatic re-link" deferral is deliberate: re-linking touches disk
  state, and getting the safety right (don't delete a real symlink the user
  added by hand, don't double-link) is its own story.
- `LinkerPublish::RemovedFromMam` does **not** trigger automatic deletion of
  the library entry. Removed-from-MAM is a *signal* — the operator may want
  to keep the file anyway. Deletion of removed-from-MAM library entries is
  a follow-up story.
