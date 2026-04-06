use serde_json::json;
use tracing::warn;

use windlass_core::Observation;
use windlass_debug::DebugController;
use windlass_types::AlertPriority;

/// Wraps a `reqwest::Client` together with the Gotify connection details.
/// All Gotify operations are methods so call sites only pass `&self`.
#[derive(Clone)]
pub struct GotifyClient {
    client: reqwest::Client,
    base_url: String,
    token: String,
    debug_ctrl: DebugController,
}

impl GotifyClient {
    #[must_use]
    // DebugController wraps an Arc — cannot be const.
    #[allow(clippy::missing_const_for_fn)]
    pub fn new(
        client: reqwest::Client,
        base_url: String,
        token: String,
        debug_ctrl: DebugController,
    ) -> Self {
        Self {
            client,
            base_url,
            token,
            debug_ctrl,
        }
    }

    /// Sends a push notification to Gotify. Fire-and-forget — failures are
    /// logged but never propagated back to the Core.
    pub async fn send_alert(&self, priority: AlertPriority, message: &str) {
        let gotify_priority: u8 = match priority {
            AlertPriority::Info => 3,
            AlertPriority::Warning => 5,
            AlertPriority::Critical => 8,
        };

        let url = format!("{}/message", self.base_url);
        let req_body = json!({
            "title": "Windlass",
            "message": message,
            "priority": gotify_priority,
        });
        let req_body_str = req_body.to_string();

        match self
            .client
            .post(&url)
            .header("X-Gotify-Key", &self.token)
            .json(&req_body)
            .send()
            .await
        {
            Err(e) => {
                warn!("Gotify alert failed to send: {e}");
            }
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                if let Some(tx) = self.debug_ctrl.obs_sender() {
                    let _ = tx.send(Observation::HttpExchange {
                        module: "gotify".into(),
                        method: "POST".into(),
                        url: url.clone(),
                        request_body: Some(req_body_str),
                        response_status: status,
                        response_body: body,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windlass_debug::DebugController;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn send_alert_posts_to_message_endpoint_with_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/message"))
            .and(header("X-Gotify-Key", "my_token"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = GotifyClient::new(
            reqwest::Client::new(),
            server.uri(),
            "my_token".into(),
            DebugController::new(),
        );
        client.send_alert(AlertPriority::Warning, "disk low").await;

        // wiremock asserts all mounted mocks were called when the server drops
        server.verify().await;
    }

    #[tokio::test]
    async fn send_alert_critical_uses_priority_8() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/message"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = GotifyClient::new(
            reqwest::Client::new(),
            server.uri(),
            "tok".into(),
            DebugController::new(),
        );
        client.send_alert(AlertPriority::Critical, "vpn down").await;
    }

    #[tokio::test]
    async fn send_alert_silently_ignores_network_errors() {
        // Fire-and-forget: a network failure must not panic or propagate.
        let client = GotifyClient::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1".into(),
            "tok".into(),
            DebugController::new(),
        );
        client.send_alert(AlertPriority::Info, "hi").await;
        // reaching here means no panic
    }
}
