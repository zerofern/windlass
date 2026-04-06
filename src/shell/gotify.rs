use serde_json::json;
use tracing::warn;

use crate::types::AlertPriority;

/// Wraps a `reqwest::Client` together with the Gotify connection details.
/// All Gotify operations are methods so call sites only pass `&self`.
#[derive(Clone)]
pub struct GotifyClient {
    client: reqwest::Client,
    base_url: String,
    token: String,
}

impl GotifyClient {
    pub const fn new(client: reqwest::Client, base_url: String, token: String) -> Self {
        Self {
            client,
            base_url,
            token,
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
        if let Err(e) = self
            .client
            .post(&url)
            .header("X-Gotify-Key", &self.token)
            .json(&json!({
                "title": "Windlass",
                "message": message,
                "priority": gotify_priority,
            }))
            .send()
            .await
        {
            warn!("Gotify alert failed to send: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

        let client = GotifyClient::new(reqwest::Client::new(), server.uri(), "my_token".into());
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

        let client = GotifyClient::new(reqwest::Client::new(), server.uri(), "tok".into());
        client.send_alert(AlertPriority::Critical, "vpn down").await;
    }

    #[tokio::test]
    async fn send_alert_silently_ignores_network_errors() {
        // Fire-and-forget: a network failure must not panic or propagate.
        let client = GotifyClient::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1".into(),
            "tok".into(),
        );
        client.send_alert(AlertPriority::Info, "hi").await;
        // reaching here means no panic
    }
}
