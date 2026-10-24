# Boot Windlass with two fake ProtonVPN WireGuard servers and check the privacy
# boundary of ADR-0003: traffic leaves only through the tunnel and fails when it
# fails, the dashboard is reachable from the LAN but not through the tunnel, and
# notifications still reach their single destination while the VPN is down.
# Windlass proves the tunnel works and fails over to the other server by itself,
# and its notifications reach ntfy through the relay with a link to the dashboard.
#
# The keys below are throwaway test keys; never reuse them.
{ windlassModule }:

let
  keys = {
    machine = {
      private = "gPjssMk1J+a6tjbfIXvzTi9OwEpNMW1nG8c1DMcxyG0=";
      public = "5PTsVM7RWouAMmCp2HWUvwxu2nhusToA3ts5kBpADig=";
    };
    vpn1 = {
      private = "WPk/rfa3RHraFFjQmrogFTjOqDfTt/9LpxKOTQex5WY=";
      public = "Buy5qxK1dxZxFnZaTTIG4A/qz1X63WJT1YDICBM1JG8=";
    };
    vpn2 = {
      private = "GEaHv5XXGIcNFN42fOjCYao7UExXcvv6cu3ZyZEHWFI=";
      public = "UtPnc2fgiCU3KRV4esyxbuyFW0I3CGnFVYHUb4cJljs=";
    };
  };

  # The port each fake server's NAT-PMP gateway forwards.
  forwardedPorts = {
    vpn1 = 41000;
    vpn2 = 42000;
  };

  # A WireGuard server standing in for a ProtonVPN P2P server: it forwards the
  # client's traffic to the "internet" with its own address as the exit IP, and
  # grants a fixed forwarded port over NAT-PMP like Proton's gateway.
  vpnServer =
    name:
    { pkgs, ... }:
    {
      systemd.services.fake-nat-pmp = {
        wantedBy = [ "multi-user.target" ];
        after = [ "wireguard-wg0.service" ];
        requires = [ "wireguard-wg0.service" ];
        serviceConfig.Restart = "on-failure";
        script = "exec ${
          pkgs.writers.writePython3 "fake-nat-pmp" { } ''
            import socket
            import struct

            gateway = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            gateway.bind(("10.2.0.1", 5351))
            while True:
                request, client = gateway.recvfrom(64)
                if len(request) != 12 or request[0] != 0 or request[1] not in (1, 2):
                    continue
                lifetime = struct.unpack("!I", request[8:12])[0]
                reply = struct.pack(
                    "!BBHIHHI", 0, request[1] + 128, 0, 1, 0, ${toString forwardedPorts.${name}}, lifetime
                )
                gateway.sendto(reply, client)
          ''
        }";
      };
      networking.firewall.interfaces.wg0.allowedUDPPorts = [ 5351 ];
      networking.wireguard.interfaces.wg0 = {
        ips = [ "10.2.0.1/24" ];
        listenPort = 51820;
        privateKeyFile = toString (pkgs.writeText "${name}.key" keys.${name}.private);
        peers = [
          {
            publicKey = keys.machine.public;
            allowedIPs = [ "10.2.0.2/32" ];
          }
        ];
      };
      networking.nat = {
        enable = true;
        externalInterface = "eth1";
        internalInterfaces = [ "wg0" ];
      };
      networking.firewall.allowedUDPPorts = [ 51820 ];
      environment.systemPackages = [ pkgs.curl ];
    };

  # A wg-quick configuration in the format ProtonVPN generates.
  protonConfig =
    pkgs: nodes: name:
    pkgs.writeText "proton-${name}.conf" ''
      [Interface]
      # Key for NAT-PMP is enabled in ProtonVPN's generator.
      PrivateKey = ${keys.machine.private}
      Address = 10.2.0.2/32
      DNS = 10.2.0.1

      [Peer]
      PublicKey = ${keys.${name}.public}
      AllowedIPs = 0.0.0.0/0
      Endpoint = ${nodes.${name}.networking.primaryIPAddress}:51820
      PersistentKeepalive = 25
    '';
in
{
  name = "windlass-vpn";

  # Generous limits let the test also run, slowly, without KVM.
  defaults =
    { lib, ... }:
    {
      systemd.settings.Manager.DefaultDeviceTimeoutSec = lib.mkForce "1800s";
      virtualisation.cores = 2;
    };

  nodes = {
    # Reports the caller's address, standing in for an exit-IP service on port
    # 80 and for ntfy.sh on port 443. It also acts as a LAN client.
    web =
      { pkgs, ... }:
      {
        environment.systemPackages = [ pkgs.curl ];
        services.nginx = {
          enable = true;
          commonHttpConfig = ''
            log_format ntfy '$request_method $uri "$http_title" "$http_click"';
          '';
          virtualHosts.default = {
            default = true;
            listen = [
              {
                addr = "0.0.0.0";
                port = 80;
              }
              {
                addr = "0.0.0.0";
                port = 443;
              }
            ];
            locations."/ip".extraConfig = ''
              default_type text/plain;
              return 200 "$remote_addr\n";
            '';
            # Stands in for an ntfy topic, logging each notification's title and link.
            locations."= /windlass-vm".extraConfig = ''
              access_log /var/log/nginx/ntfy.log ntfy;
              return 200 "ok\n";
            '';
          };
        };
        networking.firewall.allowedTCPPorts = [
          80
          443
        ];
      };

    vpn1 = vpnServer "vpn1";
    vpn2 = vpnServer "vpn2";

    machine =
      { pkgs, nodes, ... }:
      {
        imports = [ windlassModule ];
        virtualisation.memorySize = 2048;
        services.windlass = {
          enable = true;
          vpn = {
            servers = {
              one = protonConfig pkgs nodes "vpn1";
              two = protonConfig pkgs nodes "vpn2";
            };
            defaultServer = "one";
            exitIpService = "http://web/ip";
          };
          dashboard = {
            interfaces = [ "eth1" ];
            allowedHosts = [ "machine" ];
            url = "http://machine:8740/";
          };
          notifications = {
            server = "http://web/";
            relayTarget = "web:443";
          };
        };
        environment.systemPackages = [
          pkgs.curl
          pkgs.wireguard-tools
        ];
      };

  };

  testScript =
    { nodes, ... }:
    let
      ipOf = name: nodes.${name}.networking.primaryIPAddress;
    in
    ''
      start_all()
      web.wait_for_unit("nginx.service")
      for server in [vpn1, vpn2]:
          server.wait_for_unit("wireguard-wg0.service")
      machine.wait_for_unit("windlass-wireguard-default.service")
      machine.wait_for_unit("windlass.service")
      machine.wait_for_unit("qbittorrent.service")
      machine.wait_for_unit("windlass-dashboard-proxy.service")
      machine.wait_for_unit("windlass-notification-relay.service")

      inside = "ip netns exec windlass "
      exit_ip = inside + "curl -sf --max-time 60 http://web/ip"

      with subtest("Windlass and qBittorrent run inside the namespace"):
          for unit in ["windlass", "qbittorrent"]:
              pid = machine.succeed(f"systemctl show -p MainPID --value {unit}").strip()
              assert machine.succeed(f"ip netns identify {pid}").strip() == "windlass", unit

      with subtest("Traffic from the namespace leaves through the VPN"):
          machine.wait_until_succeeds(exit_ip, timeout=900)
          assert machine.succeed(exit_ip).strip() == "${ipOf "vpn1"}"
          machine.succeed(inside + "sysctl -n net.ipv6.conf.all.disable_ipv6 | grep -qx 1")

      with subtest("Windlass proves the tunnel works"):
          web.wait_until_succeeds(
              "curl -sf http://machine:8740/ | grep -q 'tag ok\">Protected'", timeout=900
          )
          web.succeed("curl -sf http://machine:8740/ | grep -q '${ipOf "vpn1"}'")

      with subtest("Notifications reach ntfy through the relay with a dashboard link"):
          ntfy_log = "/var/log/nginx/ntfy.log"
          web.succeed("curl -sf -X POST -d topic=windlass-vm http://machine:8740/settings/ntfy-topic")
          web.wait_until_succeeds(
              "curl -sf http://machine:8740/settings | grep -q 'Send test notification'", timeout=900
          )
          web.succeed("curl -sf -X POST http://machine:8740/notifications/test")
          web.wait_until_succeeds(
              f"grep -q 'POST /windlass-vm \"Test notification\" \"http://machine:8740/settings\"' {ntfy_log}",
              timeout=900,
          )

      with subtest("The dashboard is reachable from the LAN, not through the tunnel"):
          web.wait_until_succeeds("curl -sf http://machine:8740/ | grep -q Windlass", timeout=900)
          vpn1.fail("curl -sf --max-time 5 http://10.2.0.2:8740/")
          # The namespace cannot reach the host's other services through its link.
          machine.fail(inside + "curl -sf --max-time 5 http://10.200.0.1:8740/")

      with subtest("qBittorrent listens on the port Proton forwards"):
          machine.wait_until_succeeds(
              inside + "curl -sf http://127.0.0.1:8080/api/v2/app/preferences"
              + " | grep -q '\"listen_port\":${toString forwardedPorts.vpn1}'",
              timeout=900,
          )
          web.succeed("curl -sf http://machine:8740/ | grep -q 'TCP ${toString forwardedPorts.vpn1}'")

      with subtest("qBittorrent is bound to the tunnel with UPnP off"):
          machine.wait_until_succeeds(
              inside + "curl -sf http://127.0.0.1:8080/api/v2/app/version", timeout=900
          )
          preferences = machine.succeed(
              inside + "curl -sf http://127.0.0.1:8080/api/v2/app/preferences"
          )
          assert '"current_network_interface":"wg0"' in preferences, preferences
          assert '"upnp":false' in preferences, preferences

      with subtest("Windlass may fail over between configured servers only"):
          machine.succeed("runuser -u windlass -- systemctl start windlass-wireguard@two.service")
          machine.wait_until_succeeds(exit_ip, timeout=900)
          assert machine.succeed(exit_ip).strip() == "${ipOf "vpn2"}"
          # The next lease renewal reaches the new server's gateway.
          machine.wait_until_succeeds(
              inside + "curl -sf http://127.0.0.1:8080/api/v2/app/preferences"
              + " | grep -q '\"listen_port\":${toString forwardedPorts.vpn2}'",
              timeout=900,
          )
          machine.fail("systemctl start windlass-wireguard@unknown.service")
          machine.fail("runuser -u windlass -- systemctl stop windlass.service")
          machine.fail("runuser -u windlass -- systemctl start sshd.service")
          # Back to the server Windlass connected to, before it fails over itself.
          machine.succeed("runuser -u windlass -- systemctl start windlass-wireguard@one.service")
          machine.wait_until_succeeds(
              inside + "curl -sf http://127.0.0.1:8080/api/v2/app/preferences"
              + " | grep -q '\"listen_port\":${toString forwardedPorts.vpn1}'",
              timeout=900,
          )

      with subtest("Traffic fails when the VPN fails"):
          vpn1.succeed("systemctl stop wireguard-wg0.service")
          machine.fail(exit_ip)
          machine.fail(inside + "curl -sf --max-time 5 http://${ipOf "web"}/ip")

      with subtest("Notifications and the dashboard work while the VPN is down"):
          relayed = machine.succeed(inside + "curl -sf --max-time 60 http://10.200.0.1:8443/ip").strip()
          assert relayed == "${ipOf "machine"}", relayed
          web.succeed("curl -sf http://machine:8740/ | grep -q Windlass")

      with subtest("Windlass fails over to the other server by itself"):
          # Three failed checks, a minute apart, start the switch.
          machine.wait_until_succeeds(
              "grep -qx two /run/windlass-vpn/current", timeout=900
          )
          machine.wait_until_succeeds(exit_ip, timeout=900)
          assert machine.succeed(exit_ip).strip() == "${ipOf "vpn2"}"
          machine.wait_until_succeeds(
              inside + "curl -sf http://127.0.0.1:8080/api/v2/app/preferences"
              + " | grep -q '\"listen_port\":${toString forwardedPorts.vpn2}'",
              timeout=900,
          )
          web.wait_until_succeeds(
              "curl -sf http://machine:8740/ | grep -q '${ipOf "vpn2"}'", timeout=900
          )
          # MAM allows one IP change an hour, so the next switch must wait.
          web.succeed("curl -sf http://machine:8740/ | grep -q 'Not before'")
          # Each step reached ntfy, even while the tunnel was down.
          for title in ["VPN not working", "Switching VPN server", "VPN server switched"]:
              web.wait_until_succeeds(f"grep -q '\"{title}\" \"http://machine:8740/#tunnel\"' {ntfy_log}", timeout=900)
    '';
}
