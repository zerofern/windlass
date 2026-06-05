use anyhow::{Context, Result};

use windlass_observability::ObservabilityConfig;
use windlass_observability::ring::parse_byte_budget;
use windlass_types::{MamSessionId, QbitPassword};

pub struct Config {
    pub qbit_url: String,
    pub qbit_user: String,
    pub qbit_pass: QbitPassword,
    pub mam_session: MamSessionId,
    /// Gluetun's built-in HTTP proxy, used to route MAM traffic through the VPN.
    /// When `None` (env var absent), the VPN client makes direct connections — useful
    /// in integration tests and local dev where no VPN tunnel is running.
    pub gluetun_proxy_url: Option<String>,
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
    pub vpn_ip_file: String,
    pub vpn_port_file: String,
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
            gluetun_proxy_url: var("GLUETUN_PROXY_URL").ok(),
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
    use super::{execute_service_actions_setting, parse_execute_service_actions};

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
