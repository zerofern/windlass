use anyhow::{Context, Result};
use std::net::SocketAddr;

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
    pub data_path: String,
    pub dump_dir: String,
    pub database_url: String,
    /// Legacy Gluetun IP file. Used only when `WG_CONFIG_PATH` is
    /// absent and Windlass is running in the Gluetun-compatible mode.
    pub vpn_ip_file: String,
    /// Legacy Gluetun forwarded-port file. Used only when
    /// `WG_CONFIG_PATH` is absent.
    pub vpn_port_file: String,
    /// Path to a `ProtonVPN`-generated `wg.conf` file. When present,
    /// Windlass owns the `WireGuard` tunnel in-process via
    /// `windlass-tunnel-core` + `windlass-net`. Requires `NET_ADMIN`
    /// and a network namespace Windlass can manage. When absent, the
    /// legacy Gluetun-compatible shell remains active.
    pub wg_config_path: Option<String>,
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
    /// Name of the Docker container whose network namespace
    /// dependents (like qBittorrent) share via
    /// `network_mode: container:<name>`.  In tunnel mode this is
    /// Windlass itself; the default is `windlass` to match the
    /// shipped `docker-compose.tunnel.yml`.  Operators with a
    /// different container name (or legacy Gluetun mode during
    /// migration) can override with `WINDLASS_ANCHOR_CONTAINER`.
    pub anchor_container: String,
    /// Interval between compliance polls in seconds (default: 60).
    pub compliance_poll_interval_secs: u64,
    /// Maximum unsatisfied torrents before alerting (default: 50).
    pub unsatisfied_quota_limit: u32,
    /// Executes service actions produced by the sans-I/O service cores. Enabled
    /// by default; disabling is diagnostic only because legacy service
    /// orchestration has been retired from `windlass-core`.
    pub execute_service_actions: bool,
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
            data_path: var("DATA_PATH").unwrap_or_else(|_| "/mnt/Data".to_string()),
            dump_dir: var("DUMP_DIR").unwrap_or_else(|_| "/mnt/Data/windlass_dumps".to_string()),
            database_url: var("DATABASE_URL")
                .context("DATABASE_URL missing; expected postgres:// URL")?,
            vpn_ip_file: var("VPN_IP_FILE").unwrap_or_else(|_| "/tmp/gluetun/ip".to_string()),
            vpn_port_file: var("VPN_PORT_FILE")
                .unwrap_or_else(|_| "/tmp/gluetun/forwarded_port".to_string()),
            wg_config_path: var("WG_CONFIG_PATH").ok(),
            wg_interface_name: var("WG_INTERFACE_NAME").unwrap_or_else(|_| "wg0".to_string()),
            natpmp_gateway: var("NATPMP_GATEWAY").unwrap_or_else(|_| "10.2.0.1:5351".to_string()),
            tunnel_firewall_allow_tcp: parse_socket_addr_list(
                var("TUNNEL_FIREWALL_ALLOW_TCP")
                    .unwrap_or_default()
                    .as_str(),
            )
            .context("TUNNEL_FIREWALL_ALLOW_TCP")?,
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
            execute_service_actions: execute_service_actions_setting(
                var("WINDLASS_EXECUTE_SERVICE_ACTIONS").ok().as_deref(),
                var("WINDLASS_EXECUTE_SHADOW_ACTIONS").ok().as_deref(),
            ),
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

fn execute_service_actions_setting(current: Option<&str>, legacy: Option<&str>) -> bool {
    current.or(legacy).is_none_or(parse_execute_service_actions)
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

fn parse_execute_service_actions(value: &str) -> bool {
    !matches!(value, "0" | "false" | "FALSE" | "no" | "NO")
}

#[cfg(test)]
mod tests {
    use super::{
        execute_service_actions_setting, parse_execute_service_actions, parse_socket_addr_list,
    };

    #[test]
    fn parse_execute_service_actions_accepts_enabled_values() {
        assert!(parse_execute_service_actions("1"));
        assert!(parse_execute_service_actions("true"));
        assert!(parse_execute_service_actions("yes"));
        assert!(parse_execute_service_actions("anything-else"));
    }

    #[test]
    fn parse_execute_service_actions_accepts_disabled_values() {
        assert!(!parse_execute_service_actions("0"));
        assert!(!parse_execute_service_actions("false"));
        assert!(!parse_execute_service_actions("FALSE"));
        assert!(!parse_execute_service_actions("no"));
        assert!(!parse_execute_service_actions("NO"));
    }

    #[test]
    fn execute_service_actions_prefers_current_env_name() {
        assert!(execute_service_actions_setting(Some("true"), Some("false")));
        assert!(!execute_service_actions_setting(
            Some("false"),
            Some("true")
        ));
    }

    #[test]
    fn execute_service_actions_accepts_legacy_env_name() {
        assert!(!execute_service_actions_setting(None, Some("false")));
        assert!(execute_service_actions_setting(None, None));
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
