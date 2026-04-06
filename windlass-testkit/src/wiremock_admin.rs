use serde_json::{Value, json};

#[derive(Clone)]
pub struct WireMockAdmin {
    client: reqwest::Client,
    base: String,
}

impl WireMockAdmin {
    pub fn new(base: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base: base.to_owned(),
        }
    }

    /// Replace ALL stub mappings with the provided list.
    pub async fn set_mappings(&self, mappings: Vec<Value>) -> anyhow::Result<()> {
        // First reset all existing stubs, then bulk-import via the /import endpoint.
        self.client
            .delete(format!("{}/mappings", self.base))
            .send()
            .await?
            .error_for_status()?;

        self.client
            .post(format!("{}/mappings/import", self.base))
            .json(&json!({ "mappings": mappings }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// Reset request journal.
    pub async fn reset_requests(&self) -> anyhow::Result<()> {
        self.client
            .delete(format!("{}/requests", self.base))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// Count requests matching a URL fragment.
    #[allow(dead_code)]
    pub async fn count_requests(&self, url_fragment: &str) -> anyhow::Result<usize> {
        let resp: Value = self
            .client
            .get(format!("{}/requests", self.base))
            .send()
            .await?
            .json()
            .await?;
        let count = resp["requests"].as_array().map_or(0, |arr| {
            arr.iter()
                .filter(|r| {
                    r["request"]["url"]
                        .as_str()
                        .is_some_and(|u| u.contains(url_fragment))
                })
                .count()
        });
        Ok(count)
    }

    /// Count requests matching URL fragment AND body fragment.
    #[allow(dead_code)]
    pub async fn count_requests_with_body(
        &self,
        url_fragment: &str,
        body_fragment: &str,
    ) -> anyhow::Result<usize> {
        let resp: Value = self
            .client
            .get(format!("{}/requests", self.base))
            .send()
            .await?
            .json()
            .await?;
        let count = resp["requests"].as_array().map_or(0, |arr| {
            arr.iter()
                .filter(|r| {
                    let url_ok = r["request"]["url"]
                        .as_str()
                        .is_some_and(|u| u.contains(url_fragment));
                    let body_ok = r["request"]["body"]
                        .as_str()
                        .is_some_and(|b| b.contains(body_fragment));
                    url_ok && body_ok
                })
                .count()
        });
        Ok(count)
    }
}
