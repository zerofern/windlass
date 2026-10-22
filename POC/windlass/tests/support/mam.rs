//! Fake-MAM control-plane client for integration tests.
//!
//! Thin wrapper over the `/control/...` endpoints exposed by the
//! testkit's `TESTKIT_MODE=mam` server.  See
//! `windlass-testkit/src/mam.rs` for the server-side definitions.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub struct FakeMam {
    base: String,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
pub struct JournalEntry {
    pub method: String,
    pub path: String,
    pub query: String,
    pub body: String,
    pub cookie: Option<String>,
}

impl FakeMam {
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("build reqwest client"),
        }
    }

    /// Override the canned `/json/dynamicSeedbox.php` response.  Fields
    /// follow the testkit's `SetSeedbox` patch shape (any missing
    /// field stays at its current value):
    ///
    /// ```json
    /// { "status": 200, "success": true, "msg": "Completed",
    ///   "ip": "10.8.0.1", "asn": 212238, "as_org": "Datacamp Limited" }
    /// ```
    pub async fn set_seedbox(&self, patch: serde_json::Value) -> Result<()> {
        self.post_control("/control/seedbox", patch).await
    }

    /// Override `/jsonLoad.php`.  Shape:
    /// `{ status, connectable, ratio, seedbonus, username, unsat }`.
    pub async fn set_json_load(&self, patch: serde_json::Value) -> Result<()> {
        self.post_control("/control/json_load", patch).await
    }

    /// Override `/json/jsonIp.php`.  Shape:
    /// `{ status, ip, asn, as_org, time }`.
    pub async fn set_json_ip(&self, patch: serde_json::Value) -> Result<()> {
        self.post_control("/control/json_ip", patch).await
    }

    /// Override the status returned by `/json/checkCookie.php`.
    pub async fn set_check_cookie(&self, status: u16) -> Result<()> {
        self.post_control(
            "/control/check_cookie",
            serde_json::json!({ "status": status }),
        )
        .await
    }

    /// Return every request the fake has recorded since the last
    /// `reset()` (or process start).
    pub async fn journal(&self) -> Result<Vec<JournalEntry>> {
        let resp = self
            .client
            .get(format!("{}/control/journal", self.base))
            .send()
            .await
            .context("fake-mam journal")?;
        if !resp.status().is_success() {
            bail!("fake-mam journal HTTP {}", resp.status());
        }
        resp.json().await.context("journal json")
    }

    /// Clear the journal and restore every endpoint to its
    /// `docs/mam-api.md` happy-path default.
    pub async fn reset(&self) -> Result<()> {
        let resp = self
            .client
            .post(format!("{}/control/reset", self.base))
            .send()
            .await
            .context("fake-mam reset")?;
        if !resp.status().is_success() {
            bail!("fake-mam reset HTTP {}", resp.status());
        }
        Ok(())
    }

    async fn post_control(&self, path: &str, body: serde_json::Value) -> Result<()> {
        let resp = self
            .client
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("fake-mam POST {path}"))?;
        if !resp.status().is_success() {
            bail!("fake-mam POST {path} HTTP {}", resp.status());
        }
        Ok(())
    }
}
