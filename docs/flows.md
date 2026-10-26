# Windlass flows

What Windlass does, one flow at a time. For each flow: what triggers it, what
Windlass does and promises, where the decisions live, how it is tested today, and
the end-to-end test it still needs. The feature briefs under
`.workflow/features/` hold the rules and their reasons; this page is the map.

## How Windlass is tested

| Level | What it runs | Where |
| --- | --- | --- |
| Core scenarios | The pure core alone, with time moved by the test | `tests/*_core.rs`, `tests/core.rs` |
| Shell units | One shell piece against a fake, such as the MAM client against the fake MAM | `src/shell/**` test modules |
| Integration | The runner and shell with real SQLite and fake services | `tests/integrations.rs`, `tests/runner.rs`, `tests/shell.rs`, `tests/cli.rs`, `tests/web.rs` |
| End to end | Windlass as deployed in a test: runner, dashboard, storage, fake MAM, fake qBittorrent, fake exit-IP service, driven over HTTP | `tests/mam.rs` |
| NixOS VM | The NixOS module with fake WireGuard servers, NAT-PMP, ntfy, and MAM's tracker host | `nix/tests/vpn.nix` (`just vm-test`, needs KVM) |

End-to-end tests use real time, so flows that wait minutes or hours (failover
spacing, the 30-minute connectability check, weekly reminders) are proven in the
core scenarios with simulated time, and end to end only for their wiring.

## Summary

| # | Flow | Core | End to end | VM |
| --- | --- | --- | --- | --- |
| 1 | Deploy on NixOS | – | – | Yes |
| 2 | Start, resume, upgrade storage, shut down | Yes | Yes (cli, web) | Yes |
| 3 | Dashboard, settings, and history | Yes | Yes (web) | Partly |
| 4 | Pause and resume MAM requests | Yes | Yes | – |
| 5 | Prove the tunnel works and fail over | Yes | Partly | Yes |
| 6 | Keep the forwarded port and qBittorrent's listen port | Yes | Yes | Yes |
| 7 | Keep the privacy boundary | – | – | Yes |
| 8 | Keep qBittorrent matching the registry | Yes | Yes | – |
| 8a | Keep torrents within the limits; never queue an unsatisfied seed | Yes | Yes | – |
| 9 | Use the API session and keep the cookie | Yes | Yes | – |
| 10 | Observe the IP and register the seedbox | Yes | Yes | Yes |
| 11 | Turn bookmarks into queued and snatched rows | Yes | Yes | – |
| 12 | Admit a download: gates and the wedge decision | Yes | Yes | – |
| 13 | Fetch and add a download | Yes | Yes | – |
| 14 | Keep running downloads free | Yes | Yes | – |
| 15 | Watch the account | Yes | **No** | – |
| 16 | Check MAM's client list (A5) | Yes | **No** | – |
| 17 | Keep qBittorrent connectable (A6) | Yes | **No** | – |
| 18 | Spend points: VIP and upload credit | Yes | Yes | – |
| 19 | Settle an uncertain purchase | Yes | Yes | – |
| 20 | Remind the owner weekly | Yes | – | – |
| 21 | Deliver notifications | Yes | Yes (web) | Yes |
| 22 | Restart and verify before acting | Partly | Yes | – |

## Running and operating

### 1. Deploy on NixOS

- **Trigger:** `services.windlass.enable` in the owner's NixOS configuration.
- **Does:** creates a network namespace whose only internet route is a ProtonVPN
  WireGuard tunnel; runs Windlass and qBittorrent inside it; forwards the
  dashboard to chosen LAN and Tailscale interfaces; relays notifications to one
  ntfy host; lets Windlass start only the configured VPN servers' units.
- **Where:** `nix/module.nix`.
- **Tested:** the VM test boots it, checks the namespace, the firewall, failover
  permissions, qBittorrent's binding and port, notifications, and MAM traffic.
- **Missing:** the VM test has not run since the MAM options were added; run
  `just vm-test` on a machine with KVM.

### 2. Start, resume, upgrade storage, shut down

- **Trigger:** `windlass serve initialise|resume <database>`; SIGTERM or Ctrl-C.
- **Does:** creates storage once (never overwriting), resumes it otherwise,
  upgrades older schemas, refuses newer ones, reads every saved part, and on
  shutdown finishes outstanding writes and history.
- **Where:** `core/startup.rs`, `shell/storage.rs`, `src/main.rs`.
- **Tested:** `tests/cli.rs`, `tests/shell.rs`, `tests/web.rs`
  (`older_storage_is_upgraded_and_newer_storage_is_refused`).

### 3. Dashboard, settings, and history

- **Trigger:** the owner opens the dashboard on the LAN or Tailscale.
- **Does:** shows live status; saves the API cookie, ntfy topic, points reserve,
  and history size, each confirmed only once stored; never shows secrets;
  refuses other hosts and other sites' forms; records every handled event in
  searchable history, trimmed to the limit.
- **Where:** `shell/web/`, `core/settings.rs`, `core/history.rs`.
- **Tested:** `tests/web.rs`, `tests/dashboard_core.rs`, page unit tests.

### 4. Pause and resume MAM requests

- **Trigger:** the header button; a fresh instance starts paused.
- **Does:** a pause stops every MAM request at once and is confirmed when saved;
  a resume takes effect only once saved. Work in flight is dropped and late
  answers ignored, except purchases and downloads, which are recorded.
- **Where:** `core/mam_pause.rs`, `core/mam/mod.rs`.
- **Tested:** `tests/core.rs`, `tests/web.rs`, `tests/mam.rs`, `tests/mam_core.rs`.

## VPN and network

### 5. Prove the tunnel works and fail over

- **Trigger:** startup, then every 15 minutes, sooner after a suspected problem.
- **Does:** asks a neutral service for the exit IP through the tunnel; after
  three failures, switches to the next configured server, at most once an hour,
  with the spacing saved across restarts. MAM work stops while protection is
  lost.
- **Where:** `core/tunnel.rs`, `shell/tunnel.rs`.
- **Tested:** `tests/tunnel_core.rs`, `tests/integrations.rs`, the VM test
  (a real switch).

### 6. Keep the forwarded port and qBittorrent's listen port

- **Trigger:** a granted NAT-PMP lease; renewals before it expires.
- **Does:** keeps a lease with Proton's gateway and sets qBittorrent's listen
  port to it; renews after a server switch.
- **Where:** `core/port_forwarding.rs`, `core/qbittorrent.rs`, `shell/natpmp.rs`.
- **Tested:** `tests/integrations_core.rs`, `tests/integrations.rs`, the VM test.

### 7. Keep the privacy boundary

- **Does:** BitTorrent and MAM traffic leave only through the tunnel and fail
  when it fails; the dashboard is not reachable through the tunnel;
  notifications reach only their relay target, also while the VPN is down.
- **Where:** `nix/module.nix`.
- **Tested:** the VM test only, including that MAM's seedbox registration comes
  from the VPN's exit IP.

## qBittorrent

### 8. Keep qBittorrent matching the registry

- **Trigger:** every qBittorrent poll (10 seconds).
- **Does:** adopts torrents it finds; starts stopped ones; selects every file;
  removes torrents marked removed; adds fetched torrents from the kept file; keeps
  held torrents stopped. Rows are never deleted. qBittorrent versions MAM does not
  allow are reported and block downloads.
- **Where:** `core/registry.rs`, `core/qbittorrent.rs`, `shell/qbittorrent.rs`.
- **Tested:** `tests/registry_core.rs`, `tests/integrations_core.rs`,
  `tests/integrations.rs`, `tests/mam.rs` (add).

### 8a. Keep torrents within the limits; never queue an unsatisfied seed

- **Trigger:** every qBittorrent poll; a saved change to the limits.
- **Does:** keeps qBittorrent's queue matched to the owner's limits (default 5
  downloading, 10 active): queueing on, the same downloading and active limits,
  as many seeding as active, and idle torrents not counted, so a seed nobody
  downloads from keeps seeding. Changes are read back to confirm them; a queue
  qBittorrent does not report is left alone. A complete torrent short of 96
  hours of seeding (MAM's 72, plus a day, since qBittorrent's count is not
  MAM's) is force-started so the queue never holds it, and goes back to the
  queue once it has seeded 96 hours. Nothing here adds an unsatisfied torrent:
  force start only touches complete torrents, which MAM counts already, and a
  torrent found stopped that has downloaded nothing is left stopped. Windlass starts no new download
  while its own running downloads reach the downloading limit.
- **Where:** `core/qbittorrent.rs` (`set_preferences`, `QueuePreferences`),
  `core/registry.rs` (`reconcile`), `core/mam/acquisition.rs` (`next`),
  `core/settings.rs` (`TorrentLimits`), `shell/qbittorrent.rs`.
- **Tested:** `tests/integrations_core.rs`, `tests/integrations.rs`,
  `tests/mam_core.rs`, `tests/web.rs`.

## MAM access

### 9. Use the API session and keep the cookie

- **Trigger:** the owner pastes the `mam_id` cookie and resumes MAM requests.
- **Does:** sends requests only while resumed, protected, and within the shared
  budget (10 at once, then one every 3 seconds), one at a time, naming
  `Windlass/<version>`. Saves every replacement cookie MAM issues. A refused
  session stops cookie requests until MAM sees another IP or ASN (for a
  mismatch) or the owner changes the cookie.
- **Where:** `core/mam/mod.rs`, `core/mam/budget.rs`, `shell/mam.rs`.
- **Tested:** `tests/mam_core.rs`, `tests/mam.rs` (both scenarios), shell tests
  against the fake MAM.

### 10. Observe the IP and register the seedbox

- **Trigger:** every 5 minutes; a server switch; a connectability repair.
- **Does:** reads the IP and ASN MAM sees, never twice within a minute, also
  across a restart: the first lookup after a start waits a minute, since the
  previous run may have just made one. Nothing that uses the cookie is sent until
  MAM has shown which IP and ASN it sees, after a start or a server switch, since
  the session is locked to an ASN. Registers the IP as the seedbox when it
  changes, at most once an hour, saving each attempt before sending it, so
  restarts keep the spacing. No download starts until MAM confirms the current IP.
- **Where:** `core/mam/network.rs`.
- **Tested:** `tests/mam_core.rs`, `tests/mam.rs`, the VM test.
- **Note:** the confirmed registration is not saved, so after a restart within
  the hour downloads wait until registration is allowed again (up to an hour).

## Acquisition

### 11. Turn bookmarks into queued and snatched rows

- **Trigger:** the bookmark listing, every 15 minutes, 100 to a page.
- **Does:** records each new bookmark as *Queued*, or as *Snatched* when MAM
  reports it snatched before; a queued bookmark a later listing reports snatched
  becomes *Snatched*. Snatched rows show on the dashboard with "Download again",
  which queues them with origin *Redownload*.
- **Where:** `core/mam/mod.rs` (`listed`), `core/registry.rs`, `shell/web/pages.rs`.
- **Tested:** `tests/mam_core.rs`; the button's route in `tests/web.rs`; E3.

### 12. Admit a download: gates and the wedge decision

- **Trigger:** each decision while a queued bookmark waits.
- **Does:** admits the oldest queued bookmark still bookmarked when MAM is
  available, the seedbox confirmed, downloads not refused, qBittorrent ready and
  listed by MAM, fewer of Windlass's downloads running than the downloading limit, the account read in the last 5 minutes, the ratio at least 2.0,
  and an unsatisfied slot free after a reserve of 2, counting downloads
  qBittorrent added in the last hour. The torrent's flags must be at most 5
  minutes old: from the bookmark listing, or else a lookup by ID. No wedge when it
  is freeleech for everyone, a personal freeleech, or VIP while it and the
  account stay VIP for 20 minutes; otherwise a wedge, unless the bonus history
  shows one already spent. Torrents MAM no longer lists are skipped. Disk space
  is not checked yet (B-0001).
- **Where:** `core/mam/acquisition.rs` (`next`).
- **Tested:** `tests/mam_core.rs` covers every gate and wedge case; E1 and E3 the
  wedged path; E5 a site-wide freeleech bookmark downloaded without a wedge.

### 13. Fetch and add a download

- **Trigger:** an admitted bookmark.
- **Does:** downloads the torrent file (with `fl=1` when wedged), accepts only a
  valid torrent file, keeps it in SQLite, links the row to its hash as *Active*,
  and adds it to qBittorrent whole. A refused download holds every download until
  restart or a new cookie.
- **Where:** `core/mam/mod.rs` (`fetched`), `shell/mam.rs`, `shell/torrent_file.rs`,
  `shell/qbittorrent.rs`.
- **Tested:** `tests/mam_core.rs`, `tests/mam.rs`, shell tests.

### 14. Keep running downloads free

- **Trigger:** each account reading, bookmark listing, lookup, and bonus history
  read; startup.
- **Does:** every unfinished download Windlass added, and every one added in the
  last hour, must be shown free: by a wedge in the bonus history, or by flags from
  the last 20 minutes, which the bookmark listing refreshes for every bookmarked
  download at once. When freeleech or VIP ends, Windlass spends a wedge (with a
  fresh wedge count); with none left, or when MAM no longer lists it, it stops
  the torrent and resumes it once free. A wedge missing from the history, read a
  minute after the download, stops the torrent and is never retried. A download that finished first is reported.
- **Where:** `core/mam/acquisition.rs` (`watch`, `watch_next`),
  `core/mam/mod.rs` (`follow_downloads`).
- **Tested:** `tests/mam_core.rs`; E5 and E5b end to end.
- **Known limit:** MAM gives no end time for site-wide freeleech, so up to about
  15 minutes of a download can fall after it ends.

## Account safety and spending

### 15. Watch the account

- **Trigger:** every account reading (15 minutes; client list every 30).
- **Does:** reports a ratio below 2.0 (urgent below 1.1), unsatisfied torrents at
  80% of MAM's limit, unsatisfied torrents not seeding, partial downloads, tracker
  errors and other red categories, MAM notification counts, points below the
  reserve, VIP ending, and torrents at day 20 without 96 hours seeded.
- **Where:** `core/mam/account.rs`, `core/notifications.rs` (`mam`).
- **Tested:** `tests/mam_core.rs`, `tests/notifications_core.rs`.
- **Missing end to end:** one fake-MAM account with red flags producing the
  notices (part of E8).

### 16. Check MAM's client list (A5)

- **Does:** downloads wait while MAM lists qBittorrent but not the running
  version, marks it not allowed, or lists entries Windlass cannot match; an empty
  list admits the first download.
- **Where:** `core/mam/account.rs` (`client_standing`), `shell/mam.rs` (`clients`).
- **Tested:** `tests/mam_core.rs`, shell parser tests.
- **Known limit:** the entry's shape is a guess until it is captured while
  seeding.
- **Missing end to end:** part of E8, after the capture.

### 17. Keep qBittorrent connectable (A6)

- **Does:** when MAM cannot reach qBittorrent, records the listen port, forwarded
  port, IPs, registration, and interface; renews the lease; observes and
  registers again; reports if a reading 30 minutes later still cannot reach it.
- **Where:** `core/mam/connectability.rs`, `core/mam/mod.rs`.
- **Tested:** `tests/mam_core.rs`; not when MAM's IP is unknown.
- **End to end:** wiring only (the lease renewal reaches the gateway); the
  30-minute judgement stays in the core tests.

### 18. Spend points: VIP and upload credit

- **Does:** renews VIP within 14 days of its end, priced from the last purchase;
  buys 50 GiB of upload credit when the buffer is under 25 GiB or the ratio under
  3.0; never below the points reserve; backs off 6 hours after a refusal.
- **Where:** `core/mam/account.rs` (`wanted_purchase`).
- **Tested:** `tests/mam_core.rs`; E1 covers a VIP renewal and E4 upload credit.

### 19. Settle an uncertain purchase

- **Does:** a purchase whose answer was lost is settled from the next reading,
  which comes early, about a minute later (points not spent, a later VIP end, or
  credit arrived); otherwise the dashboard asks the owner, and nothing is bought
  until they answer.
- **Where:** `core/mam/account.rs` (`settle`, `checked`), `shell/web/mam_pages.rs`.
- **Tested:** `tests/mam_core.rs`; the route in `tests/web.rs`; E4.

### 20. Remind the owner weekly

- **Does:** once a week, a notice linking to MAM's main page, to keep the account
  active and read announcements about allowed clients. The time is saved.
- **Where:** `core/mam/mod.rs` (`remind`).
- **Tested:** `tests/mam_core.rs`.

## Notifications

### 21. Deliver notifications

- **Does:** gives every notice an id, continuing from the last stored one, and
  keeps it on the notifications page; lasting problems only after their grace
  period, and again when they clear. Urgent notices are pushed to the ntfy topic
  through the relay as only a link to their entry; recoveries and progress stay
  on the page. Retries with growing waits; keeps a bounded outbox. Without a
  readable log, urgent notices are still pushed, linking to the page.
- **Where:** `core/notifications.rs` (`record`, `Notice::reach`),
  `shell/notify.rs`, `shell/storage.rs` (`notices`), `shell/web/pages.rs`
  (`notifications`).
- **Tested:** `tests/notifications_core.rs`, `tests/web.rs`, the VM test.

## Restart

### 22. Restart and verify before acting

What Windlass saved is only where it starts. After a restart:

| Saved and trusted | Re-checked with MAM or qBittorrent before acting | Lost, by design |
| --- | --- | --- |
| Settings, pause preference, history | The account (a fresh reading) | An unresolved purchase wait |
| The registry and kept torrent files | Bookmarks and each torrent's flags | The "downloads refused" hold |
| The last registration attempt and VPN switch times | Recent downloads (qBittorrent's added times) | A connectability repair in progress |
| The last reminder | Running downloads' freeleech (flow 14) | |
| | The seedbox registration (registered again when allowed) | |

- **Tested:** core scenarios start from saved rows
  (`downloads_added_in_the_last_hour_count_against_the_limit_after_a_restart`,
  the running-download tests); E5 and E5b restart a whole Windlass mid-download.
  The recent-downloads count after a restart is proven in the core only: end to
  end, new downloads wait for the seedbox registration first.

## Planned end-to-end tests

Each runs in `tests/mam.rs`: Windlass with its dashboard, storage, the fake MAM,
and a fake qBittorrent, driven over HTTP.

| Test | Flows | Status |
| --- | --- | --- |
| E1. A pasted cookie leads to a registered seedbox and a wedged download | 9, 10, 12, 13, 18 | `a_pasted_cookie_leads_to_a_registered_seedbox_and_a_wedged_download` |
| E2. A refused session is reported and stops requests | 9 | `a_refused_session_is_reported_and_stops_requests` |
| E3. A snatched bookmark waits; "Download again" downloads it | 11, 12, 13 | `a_snatched_bookmark_waits_until_the_owner_downloads_it_again` |
| E4. An uncertain purchase is settled from the dashboard; upload credit is bought | 18, 19 | `an_uncertain_purchase_is_settled_on_the_dashboard` |
| E5. A freeleech download needs no wedge; after its freeleech ends and Windlass restarts, the bookmark listing shows it and it is wedged, without registering again within the hour | 10, 12, 14, 22 | `a_download_whose_freeleech_ends_is_wedged_after_a_restart` |
| E5b. A download no longer free and no longer bookmarked is looked up by itself, stopped with no wedge left, and resumed once free | 14, 22 | `a_download_that_cannot_be_wedged_stops_and_resumes_when_free` |
| E6, E7. Restarts and request counts are checked inside E5 and E5b: no lookup for a bookmarked download, one for an un-bookmarked one, no second registration, no broken rule | 10, 14, 22 | (in E5, E5b) |
| E8. Account flags and the client list produce notices and holds | 15, 16 | Planned, after the client list capture |

Each test starts Windlass, which waits a minute before its first IP lookup, so
the end-to-end tests take about three minutes in all.
