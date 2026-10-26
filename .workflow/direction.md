# Direction

## Goal

Make MAM downloading, seeding, and library organisation a cohesive experience.
Establish a working, private system as the foundation for future capabilities.

## Intended end state

### Cohesive MAM experience

Windlass brings together the MAM workflows represented by
[MLM and Mousehole](references.md). Acquisition, library organisation, and seedbox
IP registration belong to one understandable experience. The user can understand
what the system is doing and where intervention is needed.

The core journey is to select a book on MAM, download it, organise it into the
library, and keep seeding it. Seedbox IP registration happens in the background.

The MAM initialization loop establishes API access and maintains seedbox IP
registration. A separate search and torrent queueing loop handles acquisition.
These are workflows within the single core and share its MAM request controls.

The user selects specific torrents by bookmarking them on MAM. Bookmarks are
the acquisition input; Windlass does not need to infer a matching torrent from
a book-title search for this workflow.

Windlass leaves MAM bookmarks untouched and keeps durable local acquisition
history. A torrent already downloaded must not be downloaded automatically again,
even after its files or torrent-client entry are removed to free disk space.
The user can explicitly request another download. Current presence and historical
acquisition are separate facts; removing files must not erase history.
MAM's previously-snatched flag also suppresses automatic acquisition, including
downloads made before Windlass was installed. It does not prove completion and
can be overridden by an explicit user request.

New downloads require confirmed MAM access and seedbox registration, an available
torrent client, account eligibility, sufficient disk space, and the duplicate
and history checks above. The dashboard explains why acquisition is blocked.

The precise capabilities to carry forward from each project remain to be agreed.
Neither project defines Windlass's requirements by itself.

### Audiobookshelf integration

Audiobookshelf integration is retained as future work connecting Windlass's
acquisition and library experience. Its responsibilities and synchronisation
behaviour remain to be defined.

### MAM API compliance

Windlass must follow the supplied [MAM rules and API documentation](references.md#mam-rules-and-api-documentation).
Automation is limited to documented API functions or explicit staff approval.
Windlass must not scrape the site or automate email/password login. API access
uses the prescribed API cookie and respects endpoint restrictions and limits.
Prior tools and the proof of concept are not evidence that a behaviour is allowed.
The documented dynamicSeedbox limit means at most one request per rolling hour
for Windlass, including requests that might return `No Change`.

All Windlass requests to MAM share an additional token bucket with capacity ten.
Each request consumes one token; one token is restored every three seconds, up to
capacity. The bucket starts full. Endpoint-specific restrictions still apply.
Requests blocked only by token availability or endpoint cooldowns wait in a
queue. Requests with the same operation and parameters are identical; only one
copy may wait in the queue. Different parameters represent distinct requests.
Restart token balances remain to be agreed.

### Web dashboard

Windlass provides a web dashboard showing current system status and allowing the
user to configure Windlass. It also shows a structured history of what Windlass
received and did, including when each thing happened and when it was processed.
The dashboard is served as server-rendered HTML and reached through the LAN and
Tailscale. The dashboard also lets the user pause all of
Windlass's own requests to MAM. This control does not pause torrent-client
tracker communication. The pause persists across restarts until the user
explicitly resumes requests.

Pausing discards queued Windlass requests to MAM and ignores new requests until
unpaused. Outstanding requests are cancelled where possible. Requests already
sent cannot be recalled. Dropped work is not replayed on resume.
While paused, authentication is unverified and IP reporting is unconfirmed.
These states do not mean that MAM rejected the cookie.
The dashboard confirms that Windlass is paused only after the pause is durably
saved. A failed save must not produce that confirmation.

### Disk space

Windlass monitors disk space and holds new downloads when space is insufficient.
Existing torrents continue seeding subject to the VPN privacy boundary.

A disk cleanup page lets the user mark books as allowed for deletion. This
permission is separate from acquisition history: deleting content must not
cause a bookmarked torrent to be downloaded again. Marking a book grants
permission; it does not delete it immediately.

Windlass automatically deletes eligible marked books only when space is needed
for the next download, and only after their MAM seeding obligations are satisfied.
It deletes oldest first, using download completion date, and stops when enough
space is available. Remaining books are retained rather than clearing all marked
content. The files and torrent-client entries removed remain to be agreed.

### Notifications

Windlass sends the user notifications about important events so they can stay
informed without continually checking the system. The initial notification
channel is hosted [ntfy.sh](references.md#ntfy). Notifications still reach ntfy.sh
when the VPN is down, through a relay that can reach only ntfy.sh. Notifications
describe operational conditions, such as low disk space or failed MAM registration. They exclude book
titles and account details, and link to the dashboard page that explains them.
The [notifications brief](features/B-0002/brief.md) lists the agreed events.

### VPN privacy

Torrent traffic and MAM requests associated with the seedbox must use the VPN.
When VPN protection is unavailable, those operations stop until protection returns.
Local management access can remain available for diagnosis. The
[privacy boundary decision](decisions/pdr/0001-vpn-privacy-boundary.md) records
the rationale and scope.

For Windlass's MAM requests, unavailable VPN protection has the same effect as
pause: discard queued work, ignore new requests, and cancel outstanding requests
where possible. Authentication is unverified and IP reporting is unconfirmed. VPN
recovery must not clear an explicit user pause.

The VPN provider is ProtonVPN over WireGuard. All BitTorrent traffic goes through
the tunnel and fails when it fails. Windlass checks that the tunnel actually works
with an external service, regularly and when problems are suspected, and fails
over to another configured ProtonVPN server when it does not. MAM staff have
approved the owner's VPN use. MAM access uses an ASN-locked API session. ProtonVPN
servers use several ASNs and each must be added to the session manually, so
Windlass notifies the user when the exit ASN is not yet added. The dashboard is
reachable over the LAN and Tailscale but not through the VPN. [ADR-0003](decisions/adr/0003-vpn-namespace-confinement.md) records the
enforcement mechanism.

### Deployment

NixOS owns VPN setup, operation, and recovery. Windlass coordinates its MAM
workflows within the protection supplied by NixOS. Windlass does not implement
WireGuard or manage the tunnel lifecycle. This keeps host networking under the
operating system's control and avoids duplicating its responsibilities.

The intended deployment uses native NixOS systemd services, including headless
qBittorrent. Windlass integrates through qBittorrent's HTTP API rather than
container lifecycle control. Select and pin a MAM-approved client version.
Docker is dropped from the intended deployment: Windlass and its supporting
services run under systemd on NixOS. A NixOS network namespace whose only internet
route is the WireGuard tunnel enforces the privacy boundary; qBittorrent and
Windlass run inside it. Windlass decides on port-forwarding renewals and server
failover; NixOS performs the privileged network changes.

### Local persistence

Windlass uses SQLite for durable application data instead of PostgreSQL. This
removes the need to operate a separate database service. The
[persistence decision](decisions/adr/0002-sqlite-persistence.md) records the scope.

### Future AI librarian

An AI librarian remains a desirable future capability. A working MAM system takes
precedence. The [earlier specification](../POC/docs/windlass-specification.md) preserves
ideas for later discussion; its detailed proposals are not adopted requirements.

## Boundaries and tensions

The existing implementation is a proof of concept, not authority for product
behaviour or deployment. Existing code and documents may supply evidence without
constraining the intended system.

Deployment convenience must preserve the required VPN privacy guarantees.

## Open questions

- What should Windlass exchange with Audiobookshelf, and which system owns each
  part of library state?
- Which MLM and Mousehole workflows belong in the cohesive MAM experience?
- How often should bookmarks be checked?
- What additional API limits or tool-approval requirements must be confirmed
  beyond the supplied MAM documentation?
- How should the shared MAM token bucket recover across restarts?
- How should deletion handle shared files, library copies, and torrent-client
  entries, and books whose download completion date is unknown?
- Which events warrant notifications?
