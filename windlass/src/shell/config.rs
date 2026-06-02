use anyhow::{Context, Result};

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
