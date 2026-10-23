# External references

## qBittorrent integration

- Link: [WebUI API for 5.0+](https://github.com/qbittorrent/qBittorrent/wiki/WebUI-API-%28qBittorrent-5.0%29), [current torrent controller](https://github.com/qbittorrent/qBittorrent/blob/master/src/webui/api/torrentscontroller.cpp), and [NixOS service module](https://github.com/NixOS/nixpkgs/blob/master/nixos/modules/services/torrent/qbittorrent.nix).
- Relevance: Primary sources for HTTP integration and native service deployment. The API supports authentication, uploading torrent metadata, reading torrent state, and deletion.
- Unverified: The deployment version and MAM approval remain to be established. The API wiki uses `paused` for submission, whereas current controller source uses `stopped`; pin the contract to the chosen release and test it against that binary.
- Related: [Deployment](direction.md#deployment), [qBittorrent Feature Brief](features/B-0007/brief.md).

## ProtonVPN

- Link: [Manual port forwarding setup](https://protonvpn.com/support/port-forwarding-manual-setup)
- Relevance: Owner-selected VPN provider. Port forwarding needs a P2P server and a
  WireGuard configuration generated with NAT-PMP enabled. Mappings are requested
  from the gateway at `10.2.0.1` with a 60-second lifetime, for UDP and TCP, and
  renewed about every 45 seconds. `natpmpc` versions before 20230423 may misread
  the gateway's response.
- Owner-reported: ProtonVPN servers belong to several ASNs, so changing servers
  can change the ASN MAM sees.
- Unverified: How often the forwarded port changes across reconnects.
- Related: [ADR-0003](decisions/adr/0003-vpn-namespace-confinement.md), [VPN Feature Brief](features/B-0010/brief.md).

## VPN-Confinement

- Link: [NixOS Discourse discussion](https://discourse.nixos.org/t/setting-up-wireguard-in-a-network-namespace-for-selectively-routing-traffic-through-vpn/10252), [example configuration](https://lemmy.mariusdavid.fr/post/420598)
- Relevance: A community NixOS module that creates a WireGuard network namespace,
  with DNS leak protection, and attaches systemd services to it. A candidate for
  the namespace in ADR-0003.
- Unverified: Its options for exposing ports to the host and LAN, and whether it
  can switch WireGuard configurations without restarting attached services.

## MLM — Myanonamouse Library Manager

- Link: [MLM](https://github.com/CampingAvocado/MLM)
- Relevance: Owner-selected inspiration for Windlass's MAM interactions. Its README describes automated downloads from searches and Goodreads lists, library organisation through hardlinks, preferred formats, duplicate avoidance, and awareness of unsatisfied torrent slots.
- Unverified: Only the README was inspected. The owner reports that MLM was banned for API-rule violations; that report has not been independently verified. MLM is workflow inspiration, not evidence of permitted API behaviour.

## Mousehole — MAM seedbox IP updater

- Link: [Mousehole](https://github.com/t-mart/mousehole)
- Relevance: The owner identifies Mousehole as a good, working reference for Windlass's MAM interactions. Its README describes scheduled seedbox IP updates, cookie setup, a management website, manual updates, and troubleshooting guidance.
- Implementation: [MAM HTTP calls](https://github.com/t-mart/mousehole/blob/master/src/backend/external-api/mam.ts) and [contact scheduling](https://github.com/t-mart/mousehole/blob/master/src/backend/contact.ts). The implementation calls dynamicSeedbox with an API cookie, reads IP/ASN and registration outcomes, and preserves rotated cookies. Without a cookie, it observes the IP instead.
- Unverified: Source inspection is not live verification. Windlass must follow the supplied API limits rather than inherit Mousehole's scheduling unchanged. Windlass's VPN ownership is defined in [Direction](direction.md#deployment).

## ntfy

- Link: [ntfy](https://ntfy.sh/), [Publishing documentation](https://docs.ntfy.sh/publish/)
- Relevance: Owner-selected initial notification channel, using hosted ntfy.sh. Supports publishing messages to topics over HTTP, with titles and priorities.
- Related: [Notifications](direction.md#notifications)

## MAM rules and API documentation

- Link: Owner-supplied [rules](../docs/MAM/rules.txt), [search](../docs/MAM/loadSearchJSONbasic.txt), [torrent download](../docs/MAM/download.txt), [user data](../docs/MAM/jsonLoad.txt), [dynamic seedbox](../docs/MAM/dynamicSeedbox.txt), [IP lookup](../docs/MAM/jsonIp.txt), [bonus purchases](../docs/MAM/bonusBuy.txt), [bonus history](../docs/MAM/userBonusHistory.txt), and [form data](../docs/MAM/JSON+fast+fillout.txt).
- Relevance: All nine supplied files were reviewed. Rule 1.7 limits automation to documented API functions or staff approval. Dynamic seedbox calls have a rolling hourly limit; IP lookup has a once-per-minute limit. Rule 1.2 requires a VPN IP that changes less than once an hour and notifying staff about VPN or seedbox use. Registration requires an IP- or ASN-locked API session with update permission.
- Owner-reported: MAM staff approved the owner's VPN use. A session can hold several
  ASNs, but each new one must be added manually.
- Unverified: These local copies do not establish live conformance, Windlass's approval status, or complete rate limits for every endpoint. They do not specify retry timing, server-delay headers, or cookie-rotation headers.
- Related: [API compliance](direction.md#mam-api-compliance)
