//! LicenseSeat admin API client.
//!
//! Uses `sk_*` secret keys to perform administrative operations
//! such as creating, revoking, and listing licenses.

use serde_json::json;

/// Admin client for LicenseSeat REST API.
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
            secret_key: secret_key.to_string(),
            product_slug: product_slug.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.secret_key)
    }

    /// Create a new license.
    pub async fn create_license(
        &self,
        plan_key: &str,
        email: Option<&str>,
        seats: Option<u32>,
    ) -> anyhow::Result<()> {
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
            .map_err(|e| anyhow::anyhow!("HTTP error: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("LicenseSeat API error {status}: {text}");
        }

        Ok(())
    }

    /// List all licenses for the product.
    pub async fn list_licenses(&self) -> anyhow::Result<()> {
        let url = format!("{}/products/{}/licenses", self.base_url, self.product_slug);

        let response = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("HTTP error: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("LicenseSeat API error {status}: {text}");
        }

        Ok(())
    }

    /// Revoke (delete) a license by key.
    pub async fn revoke_license(&self, license_key: &str) -> anyhow::Result<()> {
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
            .map_err(|e| anyhow::anyhow!("HTTP error: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("LicenseSeat API error {status}: {text}");
        }

        Ok(())
    }

    /// List activations (seats) for a given license key.
    pub async fn list_activations(&self, license_key: &str) -> anyhow::Result<()> {
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
            .map_err(|e| anyhow::anyhow!("HTTP error: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("LicenseSeat API error {status}: {text}");
        }

        Ok(())
    }
}
