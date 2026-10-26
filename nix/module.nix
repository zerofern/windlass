# Run Windlass and qBittorrent inside a network namespace whose only internet
# route is a ProtonVPN WireGuard tunnel (ADR-0003).
#
# The namespace has one veth link to the host. The host forwards the dashboard
# to the configured LAN and Tailscale interfaces, and relays connections from
# the namespace to ntfy.sh only, so notifications work while the VPN is down.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.windlass;
  ns = cfg.namespace.name;
  hostVeth = "${ns}-host";
  nsVeth = "${ns}-ns";
  ip = "${pkgs.iproute2}/bin/ip";
  # nsenter, unlike `ip netns exec`, does not bind-mount /etc/netns over /etc.
  nsExec = "${pkgs.util-linux}/bin/nsenter --net=/run/netns/${ns}";
  resolvConf = "netns/${ns}/resolv.conf";

  # Only the configured server names map to a configuration file; anything else
  # is refused, so a template instance name cannot select an arbitrary file.
  connect = pkgs.writeShellApplication {
    name = "windlass-vpn-connect";
    runtimeInputs = with pkgs; [
      iproute2
      wireguard-tools
      gawk
      coreutils
    ];
    text = ''
      server="$1"
      case "$server" in
      ${lib.concatStrings (
        lib.mapAttrsToList (name: file: ''
          ${lib.escapeShellArg name}) conf=${lib.escapeShellArg (toString file)} ;;
        '') cfg.vpn.servers
      )}
        *) echo "unknown VPN server: $server" >&2; exit 2 ;;
      esac

      # wg-quick only strips files named after an interface. Write the result to
      # a file so a failure stops this unit instead of configuring nothing.
      umask 077
      work="$(mktemp -d)"
      trap 'rm -rf "$work"' EXIT
      cp "$conf" "$work/wg0.conf"
      wg-quick strip "$work/wg0.conf" > "$work/setconf"

      # Replace any previous tunnel; traffic fails until the new one is up.
      ip -n ${ns} link del wg0 2>/dev/null || true
      # Created in the host namespace so its UDP socket uses the host's uplink,
      # then moved so the namespace's only internet route is the tunnel.
      ip link add wg0 type wireguard
      ip link set wg0 netns ${ns}
      ${nsExec} wg setconf wg0 "$work/setconf"
      awk -F '=' '
        tolower($1) ~ /^[[:space:]]*address[[:space:]]*$/ {
          n = split($2, addresses, ",")
          for (i = 1; i <= n; i++) { gsub(/[[:space:]]/, "", addresses[i]); print addresses[i] }
        }' "$conf" | while read -r address; do
        case "$address" in
          *:*) ;; # IPv6 is disabled inside the namespace.
          *) ip -n ${ns} address add "$address" dev wg0 ;;
        esac
      done
      ip -n ${ns} link set wg0 up
      ip -n ${ns} route replace default dev wg0
      # Windlass reads which server is connected when it starts.
      install -d -m 0755 /run/windlass-vpn
      printf '%s\n' "$server" > "$work/current"
      install -m 0644 "$work/current" /run/windlass-vpn/current
    '';
  };

  dashboard = "${cfg.namespace.address}:${toString cfg.dashboard.port}";

  start = pkgs.writeShellScript "windlass-start" ''
    database="$STATE_DIRECTORY/windlass.sqlite"
    if [ -e "$database" ]; then mode=resume; else mode=initialise; fi
    exec ${lib.getExe cfg.package} serve "$mode" "$database" \
      --listen ${dashboard} \
      --qbittorrent http://127.0.0.1:${toString cfg.qbittorrent.webuiPort}/ \
      --nat-pmp-gateway ${cfg.vpn.natPmpGateway} \
      --exit-ip-service ${lib.escapeShellArg cfg.vpn.exitIpService} \
      --vpn-current-file /run/windlass-vpn/current \
      --ntfy-server ${lib.escapeShellArg cfg.notifications.server} \
      --ntfy-relay ${cfg.namespace.hostAddress}:${toString cfg.notifications.relayPort} \
      ${
        lib.optionalString (
          cfg.dashboard.url != null
        ) "--dashboard-url ${lib.escapeShellArg cfg.dashboard.url}"
      } \
      ${lib.optionalString cfg.mam.enable "--mam-site ${lib.escapeShellArg cfg.mam.site} --mam-tracker ${lib.escapeShellArg cfg.mam.tracker}"} \
      ${
        lib.concatMapStringsSep " " (server: "--vpn-server ${lib.escapeShellArg server}") (
          lib.attrNames cfg.vpn.servers
        )
      } \
      ${lib.concatMapStringsSep " " (
        host: "--allowed-host ${lib.escapeShellArg host}"
      ) cfg.dashboard.allowedHosts}
  '';

  firewall = pkgs.writeText "windlass-namespace.nft" ''
    table inet windlass {
      chain input {
        type filter hook input priority filter; policy drop;
        iif lo accept
        ct state established,related accept
        iifname "${nsVeth}" tcp dport ${toString cfg.dashboard.port} accept
        # Peers reach qBittorrent's forwarded port; nothing else listens on wg0.
        iifname "wg0" tcp dport != { ${toString cfg.dashboard.port}, ${toString cfg.qbittorrent.webuiPort} } accept
        iifname "wg0" udp dport != { ${toString cfg.dashboard.port}, ${toString cfg.qbittorrent.webuiPort} } accept
      }
      chain output {
        type filter hook output priority filter; policy drop;
        oif lo accept
        ct state established,related accept
        oifname "wg0" accept
        oifname "${nsVeth}" ip daddr ${cfg.namespace.hostAddress} tcp dport ${toString cfg.notifications.relayPort} accept
      }
    }
  '';

  confined = {
    NetworkNamespacePath = "/run/netns/${ns}";
    # Name resolution must use the tunnel: glibc would otherwise ask the host's
    # nscd, which resolves outside the namespace.
    BindReadOnlyPaths = [ "/etc/${resolvConf}:/etc/resolv.conf" ];
    InaccessiblePaths = [ "-/run/nscd" ];
  };
in
{
  options.services.windlass = {
    enable = lib.mkEnableOption "Windlass with its VPN-confined qBittorrent";

    package = lib.mkOption {
      type = lib.types.package;
      description = "The Windlass package.";
    };

    namespace = {
      name = lib.mkOption {
        type = lib.types.strMatching "[a-z][a-z0-9-]{0,9}";
        default = "windlass";
        description = "Network namespace for Windlass and qBittorrent. Short, as interface names derive from it.";
      };
      hostAddress = lib.mkOption {
        type = lib.types.str;
        default = "10.200.0.1";
        description = "Host end of the link to the namespace.";
      };
      address = lib.mkOption {
        type = lib.types.str;
        default = "10.200.0.2";
        description = "Namespace end of the link to the host; the dashboard listens here.";
      };
    };

    dashboard = {
      port = lib.mkOption {
        type = lib.types.port;
        default = 8740;
        description = "Dashboard port, inside the namespace and on the host.";
      };
      interfaces = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [
          "enp3s0"
          "tailscale0"
        ];
        description = "Host interfaces, such as the LAN and Tailscale, on which the dashboard is reachable.";
      };
      allowedHosts = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [
          "192.168.1.20"
          "nuc.example.ts.net"
        ];
        description = "Host names and addresses browsers use to reach the dashboard.";
      };
      url = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "http://nuc.example.ts.net:8740/";
        description = "The dashboard address you open, which notifications link to.";
      };
    };

    vpn = {
      servers = lib.mkOption {
        type = lib.types.attrsOf lib.types.path;
        example = lib.literalExpression ''{ nl-free-1 = "/run/secrets/proton-nl-1.conf"; }'';
        description = ''
          ProtonVPN WireGuard configurations, one per P2P server, generated with
          NAT-PMP enabled. Keep them out of the Nix store, for example as secrets.
        '';
      };
      defaultServer = lib.mkOption {
        type = lib.types.str;
        description = "The server connected at boot. Windlass may fail over to the others.";
      };
      dns = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ "10.2.0.1" ];
        description = "Resolvers reached through the tunnel.";
      };
      natPmpGateway = lib.mkOption {
        type = lib.types.str;
        default = "10.2.0.1";
        description = "The NAT-PMP gateway that forwards a port, reached through the tunnel.";
      };
      exitIpService = lib.mkOption {
        type = lib.types.str;
        default = "https://ipv4.icanhazip.com/";
        description = ''
          A neutral service, not MAM, that answers with the caller's IP address as
          plain text. Windlass asks it through the tunnel to prove the tunnel works,
          and fails over to the next server, in name order, after repeated failures.
        '';
      };
    };

    notifications = {
      server = lib.mkOption {
        type = lib.types.str;
        default = "https://ntfy.sh/";
        description = ''
          The ntfy server notifications are published to. Windlass reaches it
          through the relay, so `relayTarget` must name the same host. The topic
          is set on the dashboard's settings page.
        '';
      };
      relayPort = lib.mkOption {
        type = lib.types.port;
        default = 8443;
        description = "Port on the host end of the link where the notification relay listens.";
      };
      relayTarget = lib.mkOption {
        type = lib.types.str;
        default = "ntfy.sh:443";
        description = "The only destination the relay connects to.";
      };
    };

    mam = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Whether Windlass talks to MAM: it observes the IP MAM sees, registers
          it as a seedbox, reads the account, and downloads bookmarks. Requests
          leave through the tunnel only, and start once an API cookie is saved
          and MAM requests are resumed on the dashboard.
        '';
      };
      site = lib.mkOption {
        type = lib.types.str;
        default = "https://www.myanonamouse.net/";
        description = "MAM's site, for the account, bookmarks, torrent files, and the store.";
      };
      tracker = lib.mkOption {
        type = lib.types.str;
        default = "https://t.myanonamouse.net/";
        description = "MAM's tracker host, for IP lookups and seedbox registration.";
      };
    };

    qbittorrent.webuiPort = lib.mkOption {
      type = lib.types.port;
      default = 8080;
      description = "qBittorrent WebUI port on the namespace's loopback interface.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.vpn.servers ? ${cfg.vpn.defaultServer};
        message = "services.windlass.vpn.defaultServer must name one of services.windlass.vpn.servers.";
      }
      {
        # Server names become unit instance names and Windlass arguments.
        assertion = lib.all (name: builtins.match "[A-Za-z0-9_-]{1,64}" name != null) (
          lib.attrNames cfg.vpn.servers
        );
        message = "services.windlass.vpn.servers names may use only letters, digits, - and _, up to 64.";
      }
      {
        # MAM's allowed clients: qBittorrent 5.0.1 to the latest 5.2.x, which it recommends.
        assertion =
          let
            version = config.services.qbittorrent.package.version;
          in
          lib.versionAtLeast version "5.0.1" && lib.versionOlder version "5.3";
        message = "MAM allows qBittorrent 5.0.1 to 5.2.x; services.qbittorrent.package is ${config.services.qbittorrent.package.version}.";
      }
    ];

    environment.etc.${resolvConf}.text = lib.concatMapStrings (
      server: "nameserver ${server}\n"
    ) cfg.vpn.dns;

    systemd.services.windlass-netns = {
      description = "Windlass VPN network namespace";
      before = [ "network.target" ];
      wantedBy = [ "multi-user.target" ];
      path = [
        pkgs.iproute2
        pkgs.nftables
        pkgs.procps
      ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      script = ''
        ip netns del ${ns} 2>/dev/null || true
        ip netns add ${ns}
        ${nsExec} sysctl -q -w net.ipv6.conf.all.disable_ipv6=1 net.ipv6.conf.default.disable_ipv6=1
        ${nsExec} nft -f ${firewall}
        ip -n ${ns} link set lo up
        ip link add ${hostVeth} type veth peer name ${nsVeth} netns ${ns}
        ip address add ${cfg.namespace.hostAddress}/30 dev ${hostVeth}
        ip link set ${hostVeth} up
        ip -n ${ns} address add ${cfg.namespace.address}/30 dev ${nsVeth}
        ip -n ${ns} link set ${nsVeth} up
      '';
      preStop = "ip netns del ${ns}";
    };

    # Connect a server at boot; Windlass starts another instance to fail over.
    systemd.services."windlass-wireguard@" = {
      description = "Connect the Windlass VPN namespace to ProtonVPN server %i";
      requires = [ "windlass-netns.service" ];
      after = [
        "windlass-netns.service"
        "network-online.target"
      ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${lib.getExe connect} %i";
      };
    };
    systemd.services.windlass-wireguard-default = {
      description = "Connect the Windlass VPN namespace to the default ProtonVPN server";
      wantedBy = [ "multi-user.target" ];
      requires = [ "windlass-netns.service" ];
      after = [
        "windlass-netns.service"
        "network-online.target"
      ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = "${lib.getExe connect} ${lib.escapeShellArg cfg.vpn.defaultServer}";
      };
    };

    security.polkit.enable = true;
    security.polkit.extraConfig = ''
      // Windlass may only switch between the configured VPN servers.
      polkit.addRule(function (action, subject) {
        if (action.id == "org.freedesktop.systemd1.manage-units" &&
            subject.user == "windlass" &&
            action.lookup("verb") == "start" &&
            /^windlass-wireguard@[^/]+\.service$/.test(action.lookup("unit"))) {
          return polkit.Result.YES;
        }
      });
    '';

    users.users.windlass = {
      isSystemUser = true;
      group = "windlass";
    };
    users.groups.windlass = { };

    systemd.services.windlass = {
      description = "Windlass";
      # systemctl starts another server's unit on failover, as polkit allows.
      path = [ config.systemd.package ];
      wantedBy = [ "multi-user.target" ];
      requires = [ "windlass-netns.service" ];
      after = [
        "windlass-netns.service"
        "windlass-wireguard-default.service"
      ];
      serviceConfig = confined // {
        User = "windlass";
        Group = "windlass";
        StateDirectory = "windlass";
        StateDirectoryMode = "0700";
        UMask = "0077";
        ExecStart = start;
        Restart = "on-failure";
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectControlGroups = true;
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
      };
    };

    services.qbittorrent = {
      enable = true;
      webuiPort = cfg.qbittorrent.webuiPort;
      serverConfig = {
        LegalNotice.Accepted = true;
        Preferences = {
          WebUI = {
            Address = "127.0.0.1";
            # Only processes inside the namespace reach the WebUI.
            LocalHostAuth = false;
          };
          Connection.UPnP = false;
        };
        BitTorrent.Session = {
          Interface = "wg0";
          InterfaceName = "wg0";
        };
      };
    };
    systemd.services.qbittorrent = {
      requires = [ "windlass-netns.service" ];
      after = [
        "windlass-netns.service"
        "windlass-wireguard-default.service"
      ];
      serviceConfig = confined;
    };

    # The dashboard is published on the chosen host interfaces only.
    systemd.services.windlass-dashboard-proxy = {
      description = "Forward the Windlass dashboard into its namespace";
      wantedBy = [ "multi-user.target" ];
      after = [ "windlass-netns.service" ];
      requires = [ "windlass-netns.service" ];
      serviceConfig = {
        ExecStart = "${lib.getExe pkgs.socat} TCP-LISTEN:${toString cfg.dashboard.port},fork,reuseaddr TCP:${dashboard}";
        DynamicUser = true;
        Restart = "on-failure";
      };
    };
    networking.firewall.interfaces =
      lib.genAttrs cfg.dashboard.interfaces (_: {
        allowedTCPPorts = [ cfg.dashboard.port ];
      })
      // {
        ${hostVeth}.allowedTCPPorts = [ cfg.notifications.relayPort ];
      };

    # Notifications leave outside the VPN, to one destination only.
    systemd.services.windlass-notification-relay = {
      description = "Relay Windlass notifications to ${cfg.notifications.relayTarget}";
      wantedBy = [ "multi-user.target" ];
      after = [ "windlass-netns.service" ];
      requires = [ "windlass-netns.service" ];
      serviceConfig = {
        ExecStart = "${lib.getExe pkgs.socat} TCP-LISTEN:${toString cfg.notifications.relayPort},bind=${cfg.namespace.hostAddress},fork,reuseaddr TCP:${cfg.notifications.relayTarget}";
        DynamicUser = true;
        Restart = "on-failure";
      };
    };
  };
}
