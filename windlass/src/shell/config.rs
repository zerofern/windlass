use anyhow::{Context, Result};
use secrecy::SecretString;

use windlass_types::QbitPassword;

pub struct Config {
    pub qbit_url: String,
    pub qbit_user: String,
    pub qbit_pass: QbitPassword,
    pub mam_session: String,
    pub gotify_url: String,
    pub gotify_token: String,
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
    /// Mount path to check for available disk space.
    pub data_path: String,
    pub dump_dir: String,
    pub vpn_ip_file: String,
    pub vpn_port_file: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        use std::env::var;
        Ok(Self {
            qbit_url: var("QBITTORRENT_URL").context("QBITTORRENT_URL missing")?,
            qbit_user: var("QBITTORRENT_USER").context("QBITTORRENT_USER missing")?,
            qbit_pass: QbitPassword(SecretString::new(
                var("QBITTORRENT_PASS")
                    .context("QBITTORRENT_PASS missing")?
                    .into(),
            )),
            mam_session: var("MAM_SESSION").context("MAM_SESSION missing")?,
            gotify_url: var("GOTIFY_URL").context("GOTIFY_URL missing")?,
            gotify_token: var("GOTIFY_TOKEN").context("GOTIFY_TOKEN missing")?,
            gluetun_proxy_url: var("GLUETUN_PROXY_URL").ok(),
            mam_seedbox_url: var("MAM_SEEDBOX_URL").unwrap_or_else(|_| {
                "https://t.myanonamouse.net/json/dynamicSeedbox.php".to_string()
            }),
            mam_load_url: var("MAM_LOAD_URL").unwrap_or_else(|_| {
                "https://www.myanonamouse.net/jsonLoad.php?clientStats".to_string()
            }),
            data_path: var("DATA_PATH").unwrap_or_else(|_| "/mnt/Data".to_string()),
            dump_dir: var("DUMP_DIR").unwrap_or_else(|_| "/mnt/Data/windlass_dumps".to_string()),
            vpn_ip_file: var("VPN_IP_FILE").unwrap_or_else(|_| "/tmp/gluetun/ip".to_string()),
            vpn_port_file: var("VPN_PORT_FILE")
                .unwrap_or_else(|_| "/tmp/gluetun/forwarded_port".to_string()),
        })
    }
}
