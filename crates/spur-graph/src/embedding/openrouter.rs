use anyhow::{anyhow, bail, Context as _, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct OpenRouterEmbedder {
    client: Client,
    api_key: String,
    endpoint: String,
}

impl OpenRouterEmbedder {
    pub const BATCH_SIZE: usize = 256;
    pub const MODEL: &'static str = "baai/bge-base-en-v1.5";

    const DEFAULT_ENDPOINT: &'static str = "https://openrouter.ai/api/v1/embeddings";

    pub fn new() -> Result<Self> {
        let api_key = std::env::var("OPENROUTER_API_KEY")
            .context("OPENROUTER_API_KEY is not set for OpenRouter embeddings")?;
        if api_key.trim().is_empty() {
            bail!("OPENROUTER_API_KEY is empty for OpenRouter embeddings");
        }
        Ok(Self::from_api_key(api_key))
    }

    pub fn from_api_key(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            endpoint: Self::DEFAULT_ENDPOINT.to_owned(),
        }
    }

    pub async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut embeddings = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(Self::BATCH_SIZE) {
            let request = EmbeddingRequest {
                model: Self::MODEL,
                input: chunk,
                encoding_format: "float",
            };
            let response = self
                .client
                .post(&self.endpoint)
                .bearer_auth(&self.api_key)
                .json(&request)
                .send()
                .await
                .context("failed to send OpenRouter embeddings request")?;
            let status = response.status();
            if !status.is_success() {
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "<failed to read error body>".to_owned());
                bail!("OpenRouter embeddings request failed with {status}: {body}");
            }

            let response = response
                .json::<EmbeddingResponse>()
                .await
                .context("failed to decode OpenRouter embeddings response")?;
            let mut chunk_embeddings = ordered_embeddings_from_response(response)?;
            if chunk_embeddings.len() != chunk.len() {
                bail!(
                    "OpenRouter returned {} embeddings for {} inputs",
                    chunk_embeddings.len(),
                    chunk.len()
                );
            }
            embeddings.append(&mut chunk_embeddings);
        }
        Ok(embeddings)
    }
}

#[derive(Debug, Serialize)]
struct EmbeddingRequest<'a> {
    model: &'static str,
    input: &'a [&'a str],
    encoding_format: &'static str,
}

#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingData {
    index: usize,
    embedding: Vec<f32>,
}

fn ordered_embeddings_from_response(response: EmbeddingResponse) -> Result<Vec<Vec<f32>>> {
    let mut data = response.data;
    data.sort_by_key(|item| item.index);
    for (expected, item) in data.iter().enumerate() {
        if item.index != expected {
            return Err(anyhow!(
                "OpenRouter embeddings response missing index {expected}; found {}",
                item.index
            ));
        }
    }
    Ok(data.into_iter().map(|item| item.embedding).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_embeddings_sorts_by_response_index() {
        let embeddings = ordered_embeddings_from_response(EmbeddingResponse {
            data: vec![
                EmbeddingData {
                    index: 1,
                    embedding: vec![1.0, 1.1],
                },
                EmbeddingData {
                    index: 0,
                    embedding: vec![0.0, 0.1],
                },
            ],
        })
        .expect("ordered embeddings");

        assert_eq!(embeddings, vec![vec![0.0, 0.1], vec![1.0, 1.1]]);
    }

    #[test]
    fn ordered_embeddings_rejects_missing_indexes() {
        let error = ordered_embeddings_from_response(EmbeddingResponse {
            data: vec![EmbeddingData {
                index: 1,
                embedding: vec![1.0],
            }],
        })
        .expect_err("missing index should fail");

        assert!(error.to_string().contains("missing index 0"));
    }
}
