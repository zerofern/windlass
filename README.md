# Windlass

Windlass is being redesigned around one pure Rust core, an asynchronous shell,
SQLite persistence, and native systemd services on NixOS.

- [Direction](.workflow/direction.md) describes the intended product.
- [Backlog](.workflow/backlog.md) retains future work.
- [Application foundation](.workflow/features/B-0008/brief.md) is ready for Design.
- [Web dashboard and MAM pause control](.workflow/features/B-0003/brief.md) is ready for Design.
- [Feature Briefs](.workflow/features/) contain the agreed and proposed flows.
- [Flows](docs/flows.md) maps what Windlass does, flow by flow, with how each is
  tested and the end-to-end tests still planned.
- [Project instructions](AGENTS.md) define coding and testing conventions.

## Proof of concept

The previous implementation and its documentation are archived in [POC](POC/README.md).
That directory contains the old Rust workspace, frontend, Docker setup, database
metadata, tests, research material, and local build artifacts. It is reference
material, not the specification for the new implementation.

The root Rust package contains a small executable core/shell slice. Run
`just check` for compilation, formatting, strict Clippy, tests, and documentation
checks. Run `just coverage` for LLVM coverage and `just integration` for real
SQLite integration tests. The foundation feature is still incomplete.

## Run the dashboard

```sh
just run serve initialise /tmp/windlass.sqlite --listen 127.0.0.1:8740
```

Open `http://127.0.0.1:8740/`. Add `--qbittorrent http://127.0.0.1:8080/` to watch a
qBittorrent web UI, and `--nat-pmp-gateway 10.2.0.1` to keep a ProtonVPN forwarded
port and make qBittorrent listen on it. Add `--exit-ip-service <url>` to check that
the VPN carries traffic, with `--vpn-server <name>` for each server Windlass may
fail over to and `--vpn-current-file <path>` naming the one connected now. Use
`--ntfy-server <url>`, `--ntfy-relay <address>`, and `--dashboard-url <url>` set
where notifications go, the relay that reaches it, and the dashboard link they carry.
Add `--mam-site https://www.myanonamouse.net/ --mam-tracker https://t.myanonamouse.net/`
to let Windlass work with MAM; without them it never contacts MAM.
Use `resume` instead of `initialise` to restart an
existing instance. Repeat `--listen` for each LAN or Tailscale address, and add
`--allowed-host <name>` for host names such as a Tailscale name. Requests naming
any other host are refused, as are form submissions from other websites. Windlass
has no login; reach it only through the LAN or Tailscale. Ctrl-C or SIGTERM
finishes outstanding storage work and history before exiting.

The dashboard is server-rendered HTML with a small script for live status and local
times; it works without the script, without live updates. It shows:

- **Status:** whether Windlass's own MAM requests are running, paused, or waiting
  for a pause or resume to be saved, plus API cookie and history state. A fresh
  instance starts paused and asks for the API cookie.
- **History:** every event the core handled, with the actions it returned, when the
  event happened, and when it was processed. Filter by kind and time; page through
  older entries. Routine entries, such as polls and lease renewals, are hidden unless
  asked for. Secrets are recorded as `[redacted]`.
- **VPN tunnel:** whether the latest exit-IP check succeeded, the exit IP and
  server, failed checks, and when Windlass may next switch servers.
- **qBittorrent and port forwarding:** the client version and whether MAM allows it,
  the network interface, the listen port against the forwarded port, and the lease.
- **Torrents:** every torrent in the registry with what Windlass wants for it, its
  state, progress, size, upload, and completion, marking torrents missing from
  qBittorrent and content missing on disk. Bookmarks snatched before have a
  "Download again" button. Windlass adopts torrents it finds,
  starts stopped ones, selects every file, and removes those marked removed.
  It keeps qBittorrent's queue to the torrent limits set on the settings page (by
  default 5 downloading and 10 active, where idle seeds do not count) and
  force-starts any complete torrent short of 96 hours of seeding (MAM requires
  72; Windlass adds a day), so a limit never stops one. A torrent you add to
  qBittorrent stopped, that has downloaded nothing, stays stopped: starting it
  would add an unsatisfied torrent on MAM without Windlass checking the limit.
- **MAM:** whether the API session works, the IP and ASN MAM sees, the seedbox
  registration and when it may next be renewed; the account as MAM last reported
  it (ratio, upload buffer, points, wedges, VIP, unsatisfied torrents, and flagged
  categories as counts); the last purchase; and the bookmark list, with why the
  next queued torrent is held.
- **Notifications:** whether the last delivery succeeded, notices waiting, and
  problems reported but not yet resolved. The notifications page lists every
  notice, newest first.
- **Settings:** the MAM API cookie, the ntfy topic with a test button, the bonus
  points reserve, the torrent limits, and the history size limit, each confirmed only once saved, plus the read-only
  deployment settings.

Windlass sends notifications to an ntfy topic set on the settings page: when it
starts, when VPN protection is lost or restored, when it switches servers or cannot,
when qBittorrent stops responding or runs a version MAM does not allow, when
torrents have errors, missing files, or are missing from qBittorrent, and when
storage fails. For MAM it reports a refused session, an unregistered seedbox, a low
ratio, unsatisfied torrents near the limit or not seeding, flagged categories and
MAM notifications as counts, held or refused downloads, no wedges left, points
below the reserve, VIP ending, purchases, and a wedge that was not spent. Once a
week it reminds you to visit MAM, linking to MAM's main page, to keep the account
active and read announcements about allowed clients. Lasting problems notify only after a grace period, from immediately
to 10 minutes, and again when they clear.

Every notice is kept on the dashboard's notifications page, which keeps the last
1000. Urgent ones are also pushed, but a push says only "Windlass has a new notice"
and links to that notice's entry, so nothing about your account or torrents goes
to ntfy. Notices that something went back to normal or went as planned, such as
a start, a recovery, or a settled purchase, are only on the page.

History lives in the same SQLite file. The core counts entries and asks the shell to
remove the oldest when the limit (10000 by default) is exceeded. Entries that could
not be saved are reported as a gap once saving works again. The API cookie is stored
unencrypted in the SQLite file, so keep that file private.

## Working with MAM

Windlass contacts MAM only while MAM requests are resumed, the VPN tunnel is
proven to work, and, for everything but the IP lookup, an API cookie is saved.
Requests share a small token bucket, wait for one another, and name
`Windlass/<version>` as their User-Agent. When MAM sends a replacement cookie,
Windlass saves it and uses it from then on.

- It looks up the IP and ASN MAM sees every five minutes, never twice within a
  minute (so the first lookup after a start waits a minute), and uses the cookie
  only once MAM has shown which IP and ASN it sees. It registers that IP as the
  seedbox when it changes, at most once an hour, counting every attempt across
  restarts.
- It reads the account every 15 minutes, and its client list every 30.
- It keeps VIP, renewing it within 14 days of expiry, and buys upload credit in
  50 GiB steps when the upload buffer falls below 25 GiB or the ratio below 3.0.
  Purchases never take points below the reserve set on the settings page, 20,000
  by default, and an uncertain purchase stops further buying until MAM is read again.
- It lists your bookmarks every 15 minutes and queues them. Bookmarks MAM reports
  as snatched before are listed under Torrents but wait for you to press
  "Download again". It admits
  the oldest queued torrent once the seedbox is confirmed, qBittorrent is ready,
  the account was read in the last five minutes, the ratio is at least 2.0, and an
  unsatisfied slot is free after a reserve of two, counting downloads qBittorrent
  added in the last hour, and MAM's client list shows the running qBittorrent (or
  lists nothing yet). Its flags must be at most 5 minutes old: from the bookmark
  listing, or else from looking the torrent up on MAM. It spends a wedge only when the torrent is not already free for you
  (freeleech for everyone, a personal freeleech, or VIP while you are VIP) and
  MAM's bonus history shows none spent on it; afterwards the history must list
  the wedge, or the torrent is stopped and you are told.
- Every running download must stay free for you. Each bookmark listing, every 15
  minutes, re-checks all of them at once. When a download's freeleech or VIP ends,
  Windlass spends a wedge on it; with none left, it stops the torrent and tells
  you, and resumes it once it is free again. After a restart, Windlass checks all
  of this with MAM and qBittorrent before downloading anything.
- When MAM cannot reach qBittorrent, Windlass checks the listen port, the forwarded
  port, the IP MAM sees, and the seedbox registration, renews the lease, and
  registers again when allowed. If MAM still cannot reach it 30 minutes later, you
  are told what it found.
- A purchase whose answer was lost is settled from the next account reading; when
  that cannot tell, the dashboard asks you to check MAM and say whether it went
  through, and nothing is bought until you do.

MAM's rules come first: Windlass calls only documented API functions, never logs
in with a password, and never gifts points or wedges. The feature briefs for
[MAM initialization](.workflow/features/B-0004/brief.md),
[acquisition](.workflow/features/B-0005/brief.md), and
[account safety](.workflow/features/B-0011/brief.md) record the rules and what
remains open.

## Deploy on NixOS

The flake provides the `windlass` package and a NixOS module that runs Windlass
and qBittorrent inside a network namespace whose only internet route is a
ProtonVPN WireGuard tunnel ([ADR-0003](.workflow/decisions/adr/0003-vpn-namespace-confinement.md)):

```nix
{
  inputs.windlass.url = "github:zerofern/windlass";
  outputs = { nixpkgs, windlass, ... }: {
    nixosConfigurations.nuc = nixpkgs.lib.nixosSystem {
      modules = [
        windlass.nixosModules.default
        {
          services.windlass = {
            enable = true;
            vpn.servers = {
              nl-1 = "/run/secrets/proton-nl-1.conf";
              ch-1 = "/run/secrets/proton-ch-1.conf";
            };
            vpn.defaultServer = "nl-1";
            dashboard.interfaces = [ "enp3s0" "tailscale0" ];
            dashboard.allowedHosts = [ "192.168.1.20" "nuc.example.ts.net" ];
            dashboard.url = "http://nuc.example.ts.net:8740/";
          };
        }
      ];
    };
  };
}
```

Generate each server's WireGuard configuration in ProtonVPN with NAT-PMP enabled,
and keep the files out of the Nix store. The module creates the namespace, its
firewall, one WireGuard tunnel at a time, the dashboard forwarding on the chosen
interfaces, the ntfy.sh relay, and qBittorrent bound to the tunnel. Windlass checks
the tunnel through `vpn.exitIpService` (by default `https://ipv4.icanhazip.com/`)
and, after three failed checks, starts `windlass-wireguard@<server>.service` for
the next server in name order, at most once an hour; polkit allows nothing else.
Windlass works with MAM's real hosts unless `mam.enable` is false; `mam.site` and
`mam.tracker` point it elsewhere.

`nix build` builds and tests the package. `just vm-test` boots the module with fake
WireGuard servers and checks the privacy boundary, automatic failover, and that MAM sees the VPN's
exit IP when Windlass registers the seedbox. It needs KVM for reasonable
speed; without it, the test driver falls back to software emulation.

## Inspect the core and shell

```sh
just run initialise /tmp/windlass.sqlite
```

The console accepts `pause`, `enable`, `forget`, `status`, and `quit`. Startup
and every storage operation are driven by core actions. `forget` deletes the
saved MAM request preference and applies the paused default. It does not delete
books or download history. No network requests are implemented.

To restore the same instance:

```sh
just run resume /tmp/windlass.sqlite
```

Initialisation refuses to overwrite an existing file; resume refuses to create
missing storage. The console shows the actual core state, including startup
phase, saved, requested, and effective preferences, and the exact pending update.
`Core::state()` returns a read-only reference to that state. Enabling waits for
a successful save, while pause applies immediately. Outstanding writes finish
before normal console shutdown. Failed startup or a failed final preference save
produces a nonzero exit status.

Slow console output keeps the latest status and storage error while the core
continues processing commands and storage results. Intermediate status updates
can be replaced. Shutdown waits for the remaining console output to drain.

The [runner](src/runner.rs) owns the [core](src/core/machine.rs), consumes one
timestamped event stream, and dispatches actions through the [shell](src/shell/mod.rs).
The shell executes effects and returns results through that same stream. The
[console](src/shell/console.rs) supplies requests and observes core state. The
[SQLite boundary](src/shell/storage.rs) executes typed storage operations on
blocking workers, outside the event loop. There are no operation identifiers,
generic machine traits, or database connection pools.

`Runner::new` returns the runner, an `EventSender`, and an observation subscription.
Clone the sender for another input source; the dashboard does this. Producers
record occurrence time before enqueueing; `send_timed` preserves a timestamp
already recorded by its producer. The queue holds 64 messages and applies
backpressure. Events follow queue order even when their timestamps differ.
Observers receive the actual core state, the last event, and the latest storage
error. Slow observers retain the newest observation without blocking the runner.

`EventSender::shutdown` rejects new submissions and drains accepted inputs and
all resulting storage work. Sender clones can remain alive during shutdown.
The runner records each handled event and its actions in history before
dispatching the actions; a single writer keeps entries in processing order.
The core schedules named timers with deadlines from supplied time. The shell
sleeps until each deadline and reports the expiry; a replaced timer's expiry is
recognised by its identity and ignored.

Task cancellation remains to be built. The
console is for reviewing the operating model; it does not implement the full
service.

## Development

Clippy enables `all`, `pedantic`, `nursery`, and selected restriction lints.
Warnings fail the build; unsafe code is forbidden. No lint suppressions are
configured. Lint settings live in `Cargo.toml`.

Development requires Rust with rustfmt, Clippy, and llvm-tools, plus just,
cargo-llvm-cov, Python, ripgrep, and prose-check. The workflow checker defaults to
the installed workflow-document-check skill under `~/.agents/skills/`; set
`WORKFLOW_DOC_CHECKER` to use another installation.

The archived build and test recipes remain in `POC/justfile`; run those from
`POC/`. They still depend on the old Docker/PostgreSQL setup.

Official MAM documentation remains in `docs/MAM/` as locally supplied references.
Windlass-authored API flows live in Feature Briefs under `.workflow/features/`.
