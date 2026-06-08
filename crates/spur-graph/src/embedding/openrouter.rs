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
    pub const MAX_INPUT_CHARS: usize = 2000;

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

        let truncated: Vec<String> = texts
            .iter()
            .map(|t: &&str| {
                if t.len() > Self::MAX_INPUT_CHARS {
                    let mut s = t[..Self::MAX_INPUT_CHARS].to_owned();
                    s.push('…');
                    s
                } else {
                    (*t).to_owned()
                }
            })
            .collect();
        let truncated_refs: Vec<&str> = truncated.iter().map(|s: &String| s.as_str()).collect();

        let mut embeddings = Vec::with_capacity(texts.len());
        for chunk in truncated_refs.chunks(Self::BATCH_SIZE) {
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
            let status_code = response.status();
            tracing::debug!(
                status = status_code.as_u16(),
                "OpenRouter embeddings response received"
            );
            if !status_code.is_success() {
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "<failed to read error body>".to_owned());
                bail!("OpenRouter embeddings request failed with {status_code}: {body}");
            }

            let headers = response.headers().clone();
            let body = response
                .text()
                .await
                .context("failed to read OpenRouter embeddings response body")?;
            if let Some(api_error) = openrouter_error_from_body(&body) {
                bail!(
                    "OpenRouter API error (model={}, batch_size={}, status={}): {}. \
                     Response body (first 500 chars): {}",
                    Self::MODEL,
                    chunk.len(),
                    status_code.as_u16(),
                    api_error,
                    response_body_preview(&body, 500)
                );
            }
            let response = match serde_json::from_str::<EmbeddingResponse>(&body) {
                Ok(response) => response,
                Err(error) => {
                    let content_type = headers
                        .get("content-type")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("unknown");
                    let content_length = headers
                        .get("content-length")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("unknown");
                    tracing::warn!(
                        status = status_code.as_u16(),
                        content_type,
                        content_length,
                        headers = ?headers,
                        "failed to decode OpenRouter embeddings response"
                    );
                    bail!(
                        "failed to decode OpenRouter embeddings response \
                         (model={}, batch_size={}, status={}, content_type={}): {}. \
                         Response body (first 500 chars): {}",
                        Self::MODEL,
                        chunk.len(),
                        status_code.as_u16(),
                        content_type,
                        error,
                        response_body_preview(&body, 500)
                    );
                }
            };
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

fn response_body_preview(body: &str, max_chars: usize) -> String {
    let mut chars = body.chars();
    let preview: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

fn openrouter_error_from_body(body: &str) -> Option<String> {
    serde_json::from_str::<OpenRouterApiError>(body)
        .ok()
        .map(|e| e.error.message)
}

#[derive(Debug, Deserialize)]
struct OpenRouterApiError {
    error: OpenRouterErrorDetail,
}

#[derive(Debug, Deserialize)]
struct OpenRouterErrorDetail {
    message: String,
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
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    async fn malformed_openrouter_server(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
        let addr = listener.local_addr().expect("server addr");
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).await.expect("read request");
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        });
        format!("http://{addr}/embeddings")
    }

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

    #[tokio::test]
    async fn embed_batch_decode_error_includes_response_diagnostics() {
        let endpoint = malformed_openrouter_server("not-json").await;
        let embedder = OpenRouterEmbedder {
            client: Client::new(),
            api_key: "test-key".to_owned(),
            endpoint,
        };

        let error = embedder
            .embed_batch(&["first", "second"])
            .await
            .expect_err("malformed JSON should fail");
        let message = error.to_string();

        assert!(message.contains("failed to decode OpenRouter embeddings response"));
        assert!(message.contains("model=baai/bge-base-en-v1.5"));
        assert!(message.contains("batch_size=2"));
        assert!(message.contains("status=200"));
        assert!(message.contains("content_type=text/html"));
        assert!(message.contains("Response body (first 500 chars): not-json"));
    }

    #[tokio::test]
    async fn embed_batch_detects_openrouter_api_error_in_200_body() {
        let error_body = r#"{"error":{"message":"HTTP 400: input too long","code":400}}"#;
        let endpoint = malformed_openrouter_server(error_body).await;
        let embedder = OpenRouterEmbedder {
            client: Client::new(),
            api_key: "test-key".to_owned(),
            endpoint,
        };

        let error = embedder
            .embed_batch(&["hello"])
            .await
            .expect_err("API error in 200 body should fail");
        let message = error.to_string();

        assert!(message.contains("OpenRouter API error"));
        assert!(message.contains("HTTP 400: input too long"));
    }

    #[test]
    fn long_input_text_is_truncated() {
        let long_text = "x".repeat(3000);
        let texts: Vec<&str> = vec![&long_text];
        let truncated: Vec<String> = texts
            .iter()
            .map(|t| {
                if t.len() > OpenRouterEmbedder::MAX_INPUT_CHARS {
                    let mut s = t[..OpenRouterEmbedder::MAX_INPUT_CHARS].to_owned();
                    s.push('…');
                    s
                } else {
                    t.to_owned()
                }
            })
            .collect();
        assert_eq!(truncated[0].len(), OpenRouterEmbedder::MAX_INPUT_CHARS + 3);
        assert!(truncated[0].ends_with('…'));
    }
}
