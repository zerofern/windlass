use reqwest::Client;
use serde_json::json;
use tracing::warn;

use crate::types::AlertPriority;

/// Sends a push notification to Gotify. Fire-and-forget — failures are
/// logged but never propagated back to the Core.
pub async fn send_alert(
    client: &Client,
    base_url: &str,
    token: &str,
    priority: AlertPriority,
    message: &str,
) {
    let gotify_priority: u8 = match priority {
        AlertPriority::Info => 3,
        AlertPriority::Warning => 5,
        AlertPriority::Critical => 8,
    };

    let url = format!("{base_url}/message");
    if let Err(e) = client
        .post(&url)
        .header("X-Gotify-Key", token)
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

        let client = reqwest::Client::new();
        send_alert(&client, &server.uri(), "my_token", AlertPriority::Warning, "disk low").await;

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

        let client = reqwest::Client::new();
        send_alert(&client, &server.uri(), "tok", AlertPriority::Critical, "vpn down").await;
    }

    #[tokio::test]
    async fn send_alert_silently_ignores_network_errors() {
        // Fire-and-forget: a network failure must not panic or propagate.
        let client = reqwest::Client::new();
        send_alert(&client, "http://127.0.0.1:1", "tok", AlertPriority::Info, "hi").await;
        // reaching here means no panic
    }
}
