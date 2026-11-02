# What must never happen on MAM

Each line is something Windlass must never do to the owner's MAM account, the
MAM rule or API limit it protects, and the tests that prove it. Rule numbers
refer to MAM's rules page; API limits to MAM's API documentation.

`tests/mam_rules.rs` checks this page: every line names at least one test, and
every test named exists. A rule without a test here is a rule Windlass does not
yet keep.

Two rules stay with the owner, since Windlass cannot see them: one account per
person and per IP (1.2, 1.3), and turning off qBittorrent's automatic updates
(2.3).

## Logging in and automation (1.7)

- **Never log in with a password, or call anything outside MAM's API.**
  Windlass only knows the `mam_id` cookie and calls seven documented endpoints.
  Tests: `src/shell/mam.rs::every_request_reaches_mam_and_is_understood`,
  `tests/mam.rs::a_pasted_cookie_leads_to_a_registered_seedbox_and_a_wedged_download`
- **Never gift points or send a wedge.** Neither can be expressed as a purchase.
  Tests: `src/shell/mam.rs::every_request_reaches_mam_and_is_understood`,
  `tests/mam.rs::an_uncertain_purchase_is_settled_on_the_dashboard`
- **Never keep using a cookie MAM refused, or use it from an IP or ASN MAM has
  not shown.**
  Tests: `tests/mam_core.rs::a_refused_session_stops_cookie_requests_until_the_cookie_changes`,
  `tests/mam_core.rs::an_asn_mismatch_lifts_when_mam_sees_another_asn`,
  `tests/mam_core.rs::an_ip_mismatch_lifts_when_mam_sees_another_ip`,
  `tests/mam_core.rs::without_a_cookie_only_the_ip_is_observed`,
  `tests/mam.rs::a_refused_session_is_reported_and_stops_requests`

## IP addresses and the VPN (1.2)

- **Never send MAM a request while paused or without VPN protection.**
  Tests: `tests/mam_core.rs::nothing_is_sent_until_mam_is_enabled_unpaused_and_protected`
- **Never switch VPN server more than once an hour, even across restarts.**
  Tests: `tests/tunnel_core.rs::switches_wait_an_hour_after_the_last_one`,
  `tests/tunnel_core.rs::switches_need_a_known_last_switch_and_another_server`,
  `src/shell/storage.rs::the_last_vpn_switch_round_trips`
- **Never register the seedbox IP more than once an hour, even across
  restarts or pauses.**
  Tests: `tests/mam_core.rs::registration_is_saved_before_it_is_sent_and_spaced_an_hour_apart`,
  `tests/mam_core.rs::a_saved_attempt_from_before_a_restart_still_spaces_the_next`,
  `tests/mam_core.rs::a_pause_while_saving_an_attempt_keeps_it_unsent_but_counted`,
  `tests/mam_core.rs::unreadable_state_blocks_registration`,
  `tests/mam.rs::a_download_whose_freeleech_ends_is_wedged_after_a_restart`
- **Never download before MAM confirmed the seedbox for the IP it sees now.**
  Tests: `tests/mam_core.rs::downloads_wait_for_the_seedbox_the_client_and_the_bookmark`,
  `tests/mam_core.rs::a_held_download_resumes_after_a_fresh_free_check_during_registration_cooldown`,
  `tests/registry_core.rs::restart_waits_for_mam_before_starting_a_saved_download`

## API limits

- **Never look the IP up more than once a minute, even across a pause or a
  restart.**
  Tests: `tests/mam_core.rs::nothing_is_sent_until_mam_is_enabled_unpaused_and_protected`,
  `tests/mam_core.rs::nothing_is_sent_to_mam_in_the_first_minute_after_a_start`
- **Never send more than ten requests at once, or more than one every three
  seconds after.**
  Tests: `tests/mam_core.rs::the_bucket_allows_ten_requests_then_one_every_three_seconds`
- **Never let restarts send faster than a running Windlass.** Nothing is sent
  in the first minute after a start.
  Tests: `tests/mam_core.rs::nothing_is_sent_to_mam_in_the_first_minute_after_a_start`
- **Never ask for the client list more than once in MAM's 30-minute cache.**
  Tests: `tests/mam_core.rs::the_client_list_is_read_at_most_every_thirty_minutes`
- **Never ask again before MAM said to.**
  Tests: `tests/mam_core.rs::mam_asking_to_wait_delays_the_retry`,
  `tests/mam_core.rs::a_queued_bookmark_does_not_bypass_the_accounts_retry_after`

## Ratio (1.4, 2.4): Windlass never uses it

- **Never download a torrent MAM would count against the ratio.** Only
  freeleech, VIP torrents while the account is VIP, and wedged torrents.
  Tests: `tests/mam_core.rs::a_bookmark_is_queued_wedged_fetched_and_added`,
  `tests/mam_core.rs::vip_torrents_need_no_wedge_while_the_account_is_vip`,
  `tests/mam_core.rs::each_download_is_judged_by_its_listing_on_mam_now`,
  `tests/mam_core.rs::every_admission_rule_can_hold_a_download`
- **Never keep downloading once a torrent stops being free.** It is wedged, or
  stopped until it is free again.
  Tests: `tests/mam_core.rs::a_download_whose_freeleech_ends_is_wedged_while_it_runs`,
  `tests/mam_core.rs::a_download_that_is_not_free_and_cannot_be_wedged_stops_until_it_is_free`,
  `tests/mam_core.rs::a_download_mam_no_longer_lists_stops_and_a_finished_one_is_reported`,
  `tests/mam_core.rs::a_saved_download_is_readded_only_after_mam_confirms_it_is_free`,
  `tests/mam.rs::a_download_that_cannot_be_wedged_stops_and_resumes_when_free`
- **Never go on if ratio was used anyway.** A rise in MAM's downloaded figure,
  even while Windlass was stopped, stops every download until the owner
  checks; it is read every 5 minutes while a download runs.
  Tests: `tests/mam_core.rs::ratio_use_stops_every_download_until_the_owner_checks`,
  `tests/mam_core.rs::ratio_use_holds_every_queued_download`,
  `tests/mam_core.rs::ratio_used_before_a_restart_or_while_windlass_was_stopped_is_caught`,
  `tests/mam_core.rs::the_first_reading_sets_the_figure_watched_and_a_fall_replaces_it`,
  `tests/mam_core.rs::without_the_saved_guard_no_download_starts`,
  `tests/mam_core.rs::the_account_is_read_every_fifteen_minutes_without_a_running_download`,
  `src/shell/storage.rs::the_ratio_guard_round_trips_and_partial_rows_fail`,
  `src/shell/web/mam_pages.rs::every_ratio_guard_state_has_words`,
  `tests/mam.rs::ratio_use_stops_downloads_until_the_owner_checks`
- **Never download while the ratio is below 2.0**, twice MAM's minimum.
  Tests: `tests/mam_core.rs::every_admission_rule_can_hold_a_download`,
  `tests/mam_core.rs::the_account_watches_raise_notices`
- **Never buy upload credit.**
  Tests: `tests/mam_core.rs::upload_credit_is_never_bought`,
  `src/shell/mam.rs::every_request_reaches_mam_and_is_understood`

## Seeding (2.5)

- **Never leave a completed torrent short of 72 hours of seeding.** Windlass
  aims for 96 and runs it regardless of qBittorrent's queue.
  Tests: `tests/registry_core.rs::starting_and_force_starting_never_add_an_unsatisfied_torrent`,
  `tests/integrations_core.rs::qbittorrents_queue_follows_the_saved_limits`,
  `tests/mam_core.rs::torrents_behind_on_seeding_are_reported`
- **Never stop a completed torrent.** One that completed while held seeds.
  Tests: `tests/registry_core.rs::a_held_torrent_that_completed_seeds_and_is_never_stopped`,
  `tests/registry_core.rs::a_complete_saved_torrent_can_resume_seeding_without_mam`
- **Never remove a torrent before it has seeded 96 hours**, unless it never
  downloaded anything.
  Tests: `tests/registry_core.rs::removed_torrents_are_deleted_only_once_nothing_is_left_unsatisfied`,
  `tests/integrations.rs::qbittorrent_is_brought_to_match_the_registry`
- **Never stop or remove a torrent the owner added.**
  Tests: `tests/mam_core.rs::torrents_the_owner_added_are_never_stopped`,
  `tests/registry_core.rs::unknown_torrents_are_adopted_as_active`

## Unsatisfied torrents and partials (2.7, 2.8)

- **Never download within two slots of the unsatisfied limit**, counting
  downloads MAM has not counted yet, even after a restart.
  Tests: `tests/mam_core.rs::every_admission_rule_can_hold_a_download`,
  `tests/mam_core.rs::downloads_added_in_the_last_hour_count_against_the_limit_after_a_restart`
- **Never start or force-start a torrent that would become unsatisfied.**
  Tests: `tests/registry_core.rs::starting_and_force_starting_never_add_an_unsatisfied_torrent`
- **Never leave a torrent partly selected.**
  Tests: `tests/registry_core.rs::partial_torrents_get_every_file_once_metadata_is_known`,
  `tests/registry_core.rs::a_partial_saved_torrent_waits_for_mam_before_selecting_more_files`

## Clients (2.3)

- **Never download with a qBittorrent version MAM does not allow**, alpha,
  beta, and development builds included.
  Tests: `tests/integrations_core.rs::versions_follow_mams_client_list`
- **Never download while MAM's client list does not show the running
  qBittorrent as allowed.**
  Tests: `tests/mam_core.rs::downloads_wait_until_mam_lists_the_running_qbittorrent`

## Wedges and points (3.7, the bonus store)

- **Never spend a wedge twice on one torrent**, even after a lost answer.
  Tests: `tests/mam_core.rs::a_wedge_the_bonus_history_shows_spent_is_not_spent_again`,
  `tests/mam_core.rs::a_lost_wedged_download_that_spent_nothing_is_wedged_again`,
  `tests/mam_core.rs::a_wedge_the_bonus_history_does_not_show_stops_the_torrent`
- **Never spend a wedge on a torrent already free**, or without a fresh count
  of wedges.
  Tests: `tests/mam_core.rs::vip_torrents_need_no_wedge_while_the_account_is_vip`,
  `tests/mam_core.rs::a_wedge_waits_for_a_fresh_count_of_wedges`,
  `tests/mam.rs::a_download_whose_freeleech_ends_is_wedged_after_a_restart`
- **Never spend points below the owner's reserve.**
  Tests: `tests/mam_core.rs::vip_is_kept_within_the_reserve`,
  `tests/mam_core.rs::the_points_reserve_is_a_saved_setting`
- **Never buy again while a purchase may have gone through, or soon after a
  refusal.**
  Tests: `tests/mam_core.rs::a_purchase_the_reading_cannot_settle_waits_for_the_owner`,
  `tests/mam_core.rs::an_uncertain_vip_purchase_is_settled_from_the_next_reading`,
  `tests/mam_core.rs::an_uncertain_vip_purchase_that_did_not_extend_vip_was_not_sold`,
  `tests/mam_core.rs::a_refused_purchase_waits_hours_before_it_is_tried_again`,
  `tests/mam.rs::an_uncertain_purchase_is_settled_on_the_dashboard`

## Other

- **Never ignore MAM refusing a download.** Every download waits until a
  restart or a new cookie.
  Tests: `tests/mam_core.rs::a_refused_download_holds_every_download`
- **Never download a torrent snatched before, or no longer bookmarked or
  listed, without the owner.**
  Tests: `tests/mam_core.rs::a_snatched_bookmark_waits_until_the_owner_asks_for_it`,
  `tests/mam_core.rs::torrents_mam_no_longer_lists_or_reports_snatched_are_not_downloaded`,
  `tests/mam.rs::a_snatched_bookmark_waits_until_the_owner_downloads_it_again`
- **Never share MAM's torrent files (2.2).** They are kept in storage and only
  given to qBittorrent.
  Tests: `src/shell/mod.rs::mam_actions_report_their_outcome`,
  `src/shell/storage.rs::mam_state_reserve_and_torrent_files_round_trip`
- **Never let the account go inactive (1.6).** The owner is reminded weekly to
  visit MAM.
  Tests: `tests/mam_core.rs::the_owner_is_reminded_weekly_to_visit_mam`
