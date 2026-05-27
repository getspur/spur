//! `LicenseSeat` admin API client.
//!
//! Uses `sk_*` secret keys to perform administrative operations
//! such as creating, revoking, and listing licenses.

use anyhow::Context as _;
use serde_json::{json, Value};

/// Admin client for `LicenseSeat` REST API.
pub struct AdminClient {
    client: reqwest::Client,
    secret_key: String,
    product_slug: String,
    base_url: String,
}

impl AdminClient {
    /// Create a new admin client.
    pub fn new(secret_key: &str, product_slug: &str, base_url: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            secret_key: secret_key.to_owned(),
            product_slug: product_slug.to_owned(),
            base_url: base_url.trim_end_matches('/').to_owned(),
        }
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.secret_key)
    }

    async fn parse_response(response: reqwest::Response) -> anyhow::Result<Value> {
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        response
            .json::<Value>()
            .await
            .context("failed to parse response")
    }

    /// Create a new license. Returns the parsed JSON response body
    /// (typically the new license record, including its key).
    pub async fn create_license(
        &self,
        plan_key: &str,
        email: Option<&str>,
        seats: Option<u32>,
    ) -> anyhow::Result<Value> {
        let url = format!("{}/products/{}/licenses", self.base_url, self.product_slug);

        let mut body = json!({"plan_key": plan_key});
        if let Some(e) = email {
            body["email"] = json!(e);
        }
        if let Some(s) = seats {
            body["seats"] = json!(s);
        }

        let response = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .await
            .context("HTTP error")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("LicenseSeat API error {status}: {text}");
        }

        Self::parse_response(response).await
    }

    /// List all licenses for the product.
    pub async fn list_licenses(&self) -> anyhow::Result<Value> {
        let url = format!("{}/products/{}/licenses", self.base_url, self.product_slug);

        let response = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .context("HTTP error")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("LicenseSeat API error {status}: {text}");
        }

        Self::parse_response(response).await
    }

    /// Revoke (delete) a license by key.
    pub async fn revoke_license(&self, license_key: &str) -> anyhow::Result<Value> {
        let url = format!(
            "{}/products/{}/licenses/{}",
            self.base_url, self.product_slug, license_key
        );

        let response = self
            .client
            .delete(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .context("HTTP error")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("LicenseSeat API error {status}: {text}");
        }

        Self::parse_response(response).await
    }

    /// List activations (seats) for a given license key.
    pub async fn list_activations(&self, license_key: &str) -> anyhow::Result<Value> {
        let url = format!(
            "{}/products/{}/licenses/{}/activations",
            self.base_url, self.product_slug, license_key
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .context("HTTP error")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("LicenseSeat API error {status}: {text}");
        }

        Self::parse_response(response).await
    }
}
