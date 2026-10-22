//! Test-support modules for the §34 integration suite.
//!
//! Shared by the integration tests via `mod support;` from
//! `tests/integration_*.rs`.  Builds on the stack defined in
//! `docker-compose.dev.yml` — see `docs/operator-readiness.md` §34.

#![allow(dead_code)] // each helper is consumed only by specific tests

pub mod docker;
pub mod mam;
pub mod qbit;
pub mod reset;

// ── Stack constants ──────────────────────────────────────────────────────────
//
// The dev-compose binds host ports; tests reach the stack through
// `localhost`.  Container names are bollard-side identifiers used by
// `docker restart` and friends.

pub const WINDLASS_BASE: &str = "http://localhost:5010";

pub const QBIT_BASE: &str = "http://localhost:18080";
pub const QBIT_USER: &str = "admin";
pub const QBIT_PASS: &str = "adminadmin";

pub const MAM_BASE: &str = "http://localhost:18082";

/// WireGuard fixture control plane (wg-server service): set the
/// NAT-PMP granted port, override the reflected exit IP, or reset.
pub const WG_CONTROL: &str = "http://localhost:19090";

pub const DATABASE_URL: &str = "postgres://windlass:windlass@localhost:15432/windlass";

pub const WINDLASS_CONTAINER: &str = "windlass-test";
pub const QBIT_CONTAINER: &str = "windlass-qbittorrent-1";
pub const MAM_CONTAINER: &str = "windlass-mock-mam-1";

// ── Waiting helpers ──────────────────────────────────────────────────────────

/// Poll an async predicate until it returns true, or panic on timeout.
/// Used to wait for state changes after stack operations (container
/// restart, torrent appearing in qBit, etc.).
pub async fn wait_for<F, Fut>(label: &str, timeout: std::time::Duration, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if f().await {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Timed out waiting for: {label}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}
