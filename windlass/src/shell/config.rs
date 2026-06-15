use anyhow::{Context, Result};
use ipnet::IpNet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use windlass_observability::ObservabilityConfig;
use windlass_observability::ring::parse_byte_budget;
use windlass_types::{MamSessionId, QbitPassword};

pub struct Config {
    pub qbit_url: String,
    pub qbit_user: String,
    pub qbit_pass: QbitPassword,
    pub mam_session: MamSessionId,
    /// Full URL for the MAM dynamic-seedbox endpoint.
    /// Override via `MAM_SEEDBOX_URL` to point at a mock in integration tests.
    pub mam_seedbox_url: String,
    /// Full URL for the MAM jsonLoad endpoint.
    /// Override via `MAM_LOAD_URL` to point at a mock in integration tests.
    pub mam_load_url: String,
    /// Full URL for the MAM `checkCookie.php` endpoint.
    /// Override via `MAM_CHECK_SESSION_URL` to point at a mock in
    /// integration tests; the default goes to real MAM and **must not**
    /// be reached from any test stack.
    pub mam_check_session_url: String,
    /// Full URL for the MAM `jsonIp.php` endpoint.
    /// Override via `MAM_JSON_IP_URL` to point at a mock in integration
    /// tests; the default goes to real MAM.
    pub mam_json_ip_url: String,
    /// Base URL (no trailing slash) for the MAM torrent download
    /// endpoint (`/tor/download.php/{hash}`).  Override via
    /// `MAM_TORRENT_BASE_URL` to point at a mock in integration tests;
    /// the default goes to real MAM.
    pub mam_torrent_base_url: String,
    pub mam_user_agent: String,
    /// Mount path to check for available disk space.
    pub data_path: PathBuf,
    pub disk_poll_interval: Duration,
    pub disk_hard_floor_bytes: u64,
    pub dump_dir: String,
    pub database_url: String,
    /// Path to a `ProtonVPN`-generated `wg.conf` file.  Required:
    /// Windlass owns the `WireGuard` tunnel in-process via
    /// `windlass-tunnel-core` + `windlass-net` and needs `NET_ADMIN`
    /// plus a network namespace it can manage.
    pub wg_config_path: String,
    /// Interface name for the in-process tunnel (when
    /// `wg_config_path` is set).  Defaults to `wg0`.
    pub wg_interface_name: String,
    /// `ProtonVPN` NAT-PMP gateway, `host:port`.  Defaults to
    /// `10.2.0.1:5351` — `ProtonVPN`'s documented address.  Override
    /// for other WireGuard-with-NAT-PMP providers.
    pub natpmp_gateway: String,
    /// Comma-separated TCP endpoints allowed outside the tunnel by the
    /// nftables kill switch. Used for local control-plane dependencies
    /// such as the shipped Postgres service.
    pub tunnel_firewall_allow_tcp: Vec<SocketAddr>,
    /// Private networks whose replies must bypass the tunnel and use
    /// Docker's original gateway. Empty means host-local access only.
    pub tunnel_local_routes: Vec<IpNet>,
    /// Comma-separated URLs the exit-IP query GETs through the tunnel
    /// (`EXIT_IP_URLS`).  Each must return the connection's source IP
    /// as plain text on the first line.  Defaults to two independent
    /// public reflectors; the integration stack points this at its
    /// in-tunnel fixture.
    pub exit_ip_urls: Vec<String>,
    /// Exit-IP query cadence in seconds (`EXIT_IP_QUERY_INTERVAL_SECS`,
    /// default 6 h).  The integration stack shortens it so IP-change
    /// contracts are testable.
    pub exit_ip_query_interval_secs: u64,
    /// Machine-side spacing between MAM dynamic-seedbox updates in
    /// seconds (`SEEDBOX_UPDATE_MIN_INTERVAL_SECS`, default 61 min —
    /// one minute over MAM's documented 1-hour limit).  The
    /// integration stack shortens it so IP-change contracts are
    /// testable.
    pub seedbox_update_min_interval_secs: u64,
    /// Name of the Docker container whose network namespace
    /// dependents (like qBittorrent) share via
    /// `network_mode: container:<name>` — Windlass itself.  The
    /// default is `windlass` to match the shipped
    /// `docker-compose.tunnel.yml`; operators with a different
    /// container name override with `WINDLASS_ANCHOR_CONTAINER`.
    pub anchor_container: String,
    /// Interval between compliance polls in seconds (default: 60).
    pub compliance_poll_interval_secs: u64,
    /// Maximum unsatisfied torrents before alerting (default: 50).
    pub unsatisfied_quota_limit: u32,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        use std::env::var;
        Ok(Self {
            qbit_url: var("QBITTORRENT_URL").context("QBITTORRENT_URL missing")?,
            qbit_user: var("QBITTORRENT_USER").context("QBITTORRENT_USER missing")?,
            qbit_pass: QbitPassword::new(
                var("QBITTORRENT_PASS").context("QBITTORRENT_PASS missing")?,
            ),
            mam_session: MamSessionId::new(var("MAM_SESSION").context("MAM_SESSION missing")?),
            mam_seedbox_url: var("MAM_SEEDBOX_URL").unwrap_or_else(|_| {
                "https://t.myanonamouse.net/json/dynamicSeedbox.php".to_string()
            }),
            mam_load_url: var("MAM_LOAD_URL").unwrap_or_else(|_| {
                "https://www.myanonamouse.net/jsonLoad.php?snatch_summary=true&clientStats"
                    .to_string()
            }),
            mam_check_session_url: var("MAM_CHECK_SESSION_URL").unwrap_or_else(|_| {
                "https://www.myanonamouse.net/json/checkCookie.php".to_string()
            }),
            mam_json_ip_url: var("MAM_JSON_IP_URL")
                .unwrap_or_else(|_| "https://t.myanonamouse.net/json/jsonIp.php".to_string()),
            mam_torrent_base_url: var("MAM_TORRENT_BASE_URL")
                .unwrap_or_else(|_| "https://www.myanonamouse.net".to_string()),
            mam_user_agent: var("MAM_USER_AGENT").unwrap_or_else(|_| "windlass".to_string()),
            data_path: PathBuf::from(var("DATA_PATH").unwrap_or_else(|_| "/mnt/Data".to_string())),
            disk_poll_interval: Duration::from_secs(parse_nonzero_u64(
                "DISK_POLL_INTERVAL_SECS",
                var("DISK_POLL_INTERVAL_SECS").ok().as_deref(),
                60,
            )?),
            disk_hard_floor_bytes: parse_nonzero_u64(
                "DISK_HARD_FLOOR_BYTES",
                var("DISK_HARD_FLOOR_BYTES").ok().as_deref(),
                50 * 1_073_741_824,
            )?,
            dump_dir: var("DUMP_DIR").unwrap_or_else(|_| "/mnt/Data/windlass_dumps".to_string()),
            database_url: var("DATABASE_URL")
                .context("DATABASE_URL missing; expected postgres:// URL")?,
            wg_config_path: var("WG_CONFIG_PATH").context(
                "WG_CONFIG_PATH missing; Windlass owns the WireGuard tunnel and \
                 requires a ProtonVPN-generated wg.conf",
            )?,
            wg_interface_name: var("WG_INTERFACE_NAME").unwrap_or_else(|_| "wg0".to_string()),
            natpmp_gateway: var("NATPMP_GATEWAY").unwrap_or_else(|_| "10.2.0.1:5351".to_string()),
            tunnel_firewall_allow_tcp: parse_socket_addr_list(
                var("TUNNEL_FIREWALL_ALLOW_TCP")
                    .unwrap_or_default()
                    .as_str(),
            )
            .context("TUNNEL_FIREWALL_ALLOW_TCP")?,
            tunnel_local_routes: parse_ip_net_list(
                var("TUNNEL_LOCAL_ROUTES").unwrap_or_default().as_str(),
            )
            .context("TUNNEL_LOCAL_ROUTES")?,
            exit_ip_urls: var("EXIT_IP_URLS").map_or_else(
                |_| {
                    vec![
                        "https://api.ipify.org".to_string(),
                        "https://ipv4.icanhazip.com".to_string(),
                    ]
                },
                |raw| {
                    raw.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(ToString::to_string)
                        .collect()
                },
            ),
            exit_ip_query_interval_secs: var("EXIT_IP_QUERY_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(6 * 60 * 60),
            seedbox_update_min_interval_secs: var("SEEDBOX_UPDATE_MIN_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(61 * 60),
            anchor_container: var("WINDLASS_ANCHOR_CONTAINER")
                .unwrap_or_else(|_| "windlass".to_string()),
            compliance_poll_interval_secs: var("COMPLIANCE_POLL_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60),
            unsatisfied_quota_limit: var("MAM_UNSATISFIED_QUOTA_LIMIT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(50),
        })
    }
}

fn parse_socket_addr_list(raw: &str) -> Result<Vec<SocketAddr>> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<SocketAddr>()
                .with_context(|| format!("expected socket address `ip:port`, got `{s}`"))
        })
        .collect()
}

fn parse_ip_net_list(raw: &str) -> Result<Vec<IpNet>> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            anyhow::ensure!(
                s.contains('/'),
                "expected IP network in CIDR notation, got `{s}`"
            );
            s.parse::<IpNet>()
                .with_context(|| format!("expected IP network in CIDR notation, got `{s}`"))
        })
        .collect()
}

fn parse_nonzero_u64(name: &str, raw: Option<&str>, default: u64) -> Result<u64> {
    let value = raw.map_or(Ok(default), |value| {
        value
            .parse::<u64>()
            .with_context(|| format!("{name} must be a positive integer"))
    })?;
    anyhow::ensure!(value > 0, "{name} must be greater than zero");
    Ok(value)
}

/// Read the `WINDLASS_OBS_*` env vars into an [`ObservabilityConfig`].
/// Each var is optional and defaults to the §37pre B7 locked constants
/// (decision 19 — config-driven from day one with sensible defaults).
///
/// Byte budgets accept IEC binary suffixes (`KiB`, `MiB`); count
/// budgets are plain `usize`.  Returns an error if a value is set but
/// fails to parse — silently falling back would hide misconfiguration.
///
/// Recognized vars (all optional):
/// - `WINDLASS_OBS_STEP_RECORDS_PER_CORE`
/// - `WINDLASS_OBS_STEP_RECORD_BYTES_PER_CORE` (e.g. `4MiB`)
/// - `WINDLASS_OBS_HTTP_EXCHANGES`
/// - `WINDLASS_OBS_HTTP_EXCHANGE_BYTES_TOTAL` (e.g. `8MiB`)
/// - `WINDLASS_OBS_MAX_REQUEST_BODY_BYTES` (e.g. `64KiB`)
/// - `WINDLASS_OBS_MAX_RESPONSE_BODY_BYTES` (e.g. `256KiB`)
pub fn load_observability_config() -> Result<ObservabilityConfig> {
    use std::env::var;
    let mut cfg = ObservabilityConfig::default();
    if let Ok(v) = var("WINDLASS_OBS_STEP_RECORDS_PER_CORE") {
        cfg.step_records_per_core = v
            .parse()
            .with_context(|| format!("WINDLASS_OBS_STEP_RECORDS_PER_CORE={v}"))?;
    }
    if let Ok(v) = var("WINDLASS_OBS_STEP_RECORD_BYTES_PER_CORE") {
        cfg.step_record_bytes_per_core = parse_byte_budget(&v).map_err(|e| anyhow::anyhow!(e))?;
    }
    if let Ok(v) = var("WINDLASS_OBS_HTTP_EXCHANGES") {
        cfg.http_exchanges_total = v
            .parse()
            .with_context(|| format!("WINDLASS_OBS_HTTP_EXCHANGES={v}"))?;
    }
    if let Ok(v) = var("WINDLASS_OBS_HTTP_EXCHANGE_BYTES_TOTAL") {
        cfg.http_exchange_bytes_total = parse_byte_budget(&v).map_err(|e| anyhow::anyhow!(e))?;
    }
    if let Ok(v) = var("WINDLASS_OBS_MAX_REQUEST_BODY_BYTES") {
        cfg.max_request_body_bytes = parse_byte_budget(&v).map_err(|e| anyhow::anyhow!(e))?;
    }
    if let Ok(v) = var("WINDLASS_OBS_MAX_RESPONSE_BODY_BYTES") {
        cfg.max_response_body_bytes = parse_byte_budget(&v).map_err(|e| anyhow::anyhow!(e))?;
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::{parse_ip_net_list, parse_nonzero_u64, parse_socket_addr_list};

    #[test]
    fn parse_nonzero_u64_accepts_default_and_explicit_value() {
        assert_eq!(parse_nonzero_u64("TEST", None, 60).unwrap(), 60);
        assert_eq!(parse_nonzero_u64("TEST", Some("15"), 60).unwrap(), 15);
    }

    #[test]
    fn parse_nonzero_u64_rejects_zero_and_invalid_values() {
        assert!(parse_nonzero_u64("TEST", Some("0"), 60).is_err());
        assert!(parse_nonzero_u64("TEST", Some("nope"), 60).is_err());
    }

    #[test]
    fn parse_socket_addr_list_accepts_comma_separated_values() {
        let values =
            parse_socket_addr_list("172.30.0.10:5432, [fd00::10]:5432").expect("list parses");
        assert_eq!(values.len(), 2);
        assert_eq!(values[0], "172.30.0.10:5432".parse().unwrap());
        assert_eq!(values[1], "[fd00::10]:5432".parse().unwrap());
    }

    #[test]
    fn parse_socket_addr_list_rejects_hostnames() {
        let err = parse_socket_addr_list("postgres:5432").expect_err("hostnames are not allowed");
        assert!(err.to_string().contains("expected socket address"));
    }

    #[test]
    fn parse_ip_net_list_accepts_comma_separated_networks() {
        let values = parse_ip_net_list("100.64.0.0/10, 192.168.2.0/24").expect("list parses");
        assert_eq!(values.len(), 2);
        assert_eq!(values[0], "100.64.0.0/10".parse().unwrap());
        assert_eq!(values[1], "192.168.2.0/24".parse().unwrap());
    }

    #[test]
    fn parse_ip_net_list_defaults_to_empty() {
        assert!(parse_ip_net_list("").expect("empty list parses").is_empty());
    }

    #[test]
    fn parse_ip_net_list_rejects_invalid_networks() {
        let err = parse_ip_net_list("192.168.2.1").expect_err("prefix is required");
        assert!(err.to_string().contains("expected IP network"));
    }

    #[test]
    fn observability_config_defaults_when_no_env_vars() {
        // Pre-emptively scrub the env vars so other tests can't leak
        // into this one.  `unsafe` is required because env mutation
        // is process-global in Rust 2024.
        unsafe {
            for k in [
                "WINDLASS_OBS_STEP_RECORDS_PER_CORE",
                "WINDLASS_OBS_STEP_RECORD_BYTES_PER_CORE",
                "WINDLASS_OBS_HTTP_EXCHANGES",
                "WINDLASS_OBS_HTTP_EXCHANGE_BYTES_TOTAL",
                "WINDLASS_OBS_MAX_REQUEST_BODY_BYTES",
                "WINDLASS_OBS_MAX_RESPONSE_BODY_BYTES",
            ] {
                std::env::remove_var(k);
            }
        }
        let cfg = super::load_observability_config().expect("defaults parse");
        let default = windlass_observability::ObservabilityConfig::default();
        assert_eq!(cfg.step_records_per_core, default.step_records_per_core);
        assert_eq!(cfg.max_request_body_bytes, default.max_request_body_bytes);
    }

    #[test]
    fn config_secrets_debug_does_not_leak_cleartext() {
        use windlass_types::{MamSessionId, QbitPassword};
        let pass = QbitPassword::new("super-secret-pw".to_string());
        let session = MamSessionId::new("super-secret-session".to_string());
        let dbg = format!("{pass:?} {session:?}");
        assert!(
            !dbg.contains("super-secret-pw"),
            "QbitPassword leaked: {dbg}"
        );
        assert!(
            !dbg.contains("super-secret-session"),
            "MamSessionId leaked: {dbg}"
        );
    }
}
