use anyhow::{anyhow, bail, Context as _, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Maximum number of retry attempts for transient network failures.
/// Total attempts = MAX_RETRIES + 1 (initial attempt + retries).
const MAX_RETRIES: usize = 2;

/// Base delay for exponential backoff between retries.
const RETRY_BASE_DELAY: Duration = Duration::from_millis(200);

/// Timeout for establishing a TCP connection to OpenRouter.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Request timeout covering the full request + response body read.
/// Bulk batches of 256 inputs can be slow, so use a generous timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Build a `reqwest::Client` with bounded connect and request timeouts.
/// Falls back to `Client::new()` if the builder fails (should never happen
/// with valid timeout values, but we keep construction infallible).
fn build_client() -> Client {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .unwrap_or_else(|_| Client::new())
}

#[derive(Debug, Clone)]
pub struct OpenRouterEmbedder {
    client: Client,
    api_key: String,
    endpoint: String,
}

impl OpenRouterEmbedder {
    pub const BATCH_SIZE: usize = 256;
    pub const MODEL: &'static str = "baai/bge-base-en-v1.5";
    pub const MAX_INPUT_CHARS: usize = 384;
    pub const MAX_INPUT_TOKENS: usize = 384;

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
            client: build_client(),
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
            .map(|t: &&str| Self::request_input_text(t))
            .collect();
        let truncated_refs: Vec<&str> = truncated.iter().map(|s: &String| s.as_str()).collect();

        let mut embeddings = Vec::with_capacity(texts.len());
        for chunk in truncated_refs.chunks(Self::BATCH_SIZE) {
            let chunk_embeddings = self.embed_chunk_with_retry(chunk).await?;
            embeddings.extend(chunk_embeddings);
        }
        Ok(embeddings)
    }

    /// Embed a single chunk with bounded retry-with-backoff for transient
    /// network failures (connection drop, body read error, 429 / 5xx status).
    /// Non-transient errors (4xx except 429, 200-with-error-body, JSON decode
    /// errors, embedding count mismatch) are returned immediately without retry.
    async fn embed_chunk_with_retry(&self, chunk: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut last_error: Option<anyhow::Error> = None;

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                // Exponential backoff: 200ms, 400ms, …
                let delay = RETRY_BASE_DELAY * (1u32 << (attempt - 1));
                tracing::debug!(
                    attempt,
                    delay_ms = delay.as_millis(),
                    "Retrying OpenRouter embeddings request after transient error"
                );
                tokio::time::sleep(delay).await;
            }

            let request = EmbeddingRequest {
                model: Self::MODEL,
                input: chunk,
                encoding_format: "float",
            };

            // Transient: send failure (connection drop, timeout, etc.)
            let response = match self
                .client
                .post(&self.endpoint)
                .bearer_auth(&self.api_key)
                .json(&request)
                .send()
                .await
                .context("failed to send OpenRouter embeddings request")
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        attempt,
                        error = %e,
                        "Transient send failure for OpenRouter embeddings request"
                    );
                    last_error = Some(e);
                    continue;
                }
            };

            let status_code = response.status();
            tracing::debug!(
                status = status_code.as_u16(),
                "OpenRouter embeddings response received"
            );

            // Retryable: 429 or 5xx status codes.
            if status_code == reqwest::StatusCode::TOO_MANY_REQUESTS
                || status_code.is_server_error()
            {
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "<failed to read error body>".to_owned());
                let err = anyhow::anyhow!(
                    "OpenRouter embeddings request failed with {status_code}: {body}"
                );
                tracing::warn!(
                    attempt,
                    status = status_code.as_u16(),
                    "Retryable HTTP error for OpenRouter embeddings request"
                );
                last_error = Some(err);
                continue;
            }

            // Non-retryable: 4xx (except 429 handled above) — fail fast.
            if !status_code.is_success() {
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "<failed to read error body>".to_owned());
                bail!("OpenRouter embeddings request failed with {status_code}: {body}");
            }

            let headers = response.headers().clone();

            // Transient: body read failure (mid-stream connection drop).
            let body = match response
                .text()
                .await
                .context("failed to read OpenRouter embeddings response body")
            {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(
                        attempt,
                        error = %e,
                        "Transient body read failure for OpenRouter embeddings response"
                    );
                    last_error = Some(e);
                    continue;
                }
            };

            // Non-retryable: 200 with an embedded API error object.
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

            // Non-retryable: JSON decode error.
            let embedding_response = match serde_json::from_str::<EmbeddingResponse>(&body) {
                Ok(r) => r,
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

            // Non-retryable: embedding count mismatch.
            let mut chunk_embeddings = ordered_embeddings_from_response(embedding_response)?;
            if chunk_embeddings.len() != chunk.len() {
                bail!(
                    "OpenRouter returned {} embeddings for {} inputs",
                    chunk_embeddings.len(),
                    chunk.len()
                );
            }

            let mut result = Vec::with_capacity(chunk_embeddings.len());
            result.append(&mut chunk_embeddings);
            return Ok(result);
        }

        // All attempts exhausted — propagate the last transient error.
        Err(last_error.unwrap_or_else(|| anyhow!("OpenRouter embeddings request failed")))
    }

    fn request_input_text(text: &str) -> String {
        let (token_limited, token_truncated) =
            truncate_to_whitespace_token_budget(text, Self::MAX_INPUT_TOKENS);
        let (char_limited, char_truncated) =
            truncate_to_char_budget(token_limited, Self::MAX_INPUT_CHARS);
        if token_truncated || char_truncated {
            let mut truncated = char_limited.trim_end().to_owned();
            truncated.push('…');
            truncated
        } else {
            text.to_owned()
        }
    }
}

fn truncate_to_whitespace_token_budget(input: &str, max_tokens: usize) -> (&str, bool) {
    if max_tokens == 0 {
        return ("", !input.trim().is_empty());
    }

    let mut tokens = 0usize;
    let mut in_token = false;
    for (index, character) in input.char_indices() {
        if character.is_whitespace() {
            in_token = false;
        } else if !in_token {
            if tokens == max_tokens {
                return (&input[..index], true);
            }
            tokens += 1;
            in_token = true;
        }
    }

    (input, false)
}

fn truncate_to_char_budget(input: &str, max_chars: usize) -> (&str, bool) {
    if max_chars == 0 {
        return ("", !input.is_empty());
    }

    match input.char_indices().nth(max_chars) {
        Some((index, _)) => (&input[..index], true),
        None => (input, false),
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
    use serde_json::Value;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

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

    async fn capturing_openrouter_server() -> (String, oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
        let addr = listener.local_addr().expect("server addr");
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request = vec![0; 8192];
            let bytes_read = stream.read(&mut request).await.expect("read request");
            request.truncate(bytes_read);
            let request = String::from_utf8(request).expect("utf8 request");
            let _ = tx.send(request);
            let body = r#"{"data":[{"index":0,"embedding":[1.0]}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        });
        (format!("http://{addr}/embeddings"), rx)
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
            client: build_client(),
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
            client: build_client(),
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

    #[tokio::test]
    async fn embed_batch_truncates_token_dense_input_before_request() {
        let (endpoint, request_rx) = capturing_openrouter_server().await;
        let embedder = OpenRouterEmbedder {
            client: build_client(),
            api_key: "test-key".to_owned(),
            endpoint,
        };
        let token_budget = OpenRouterEmbedder::MAX_INPUT_TOKENS;
        let token_dense_text = std::iter::repeat("x")
            .take(token_budget + 64)
            .collect::<Vec<_>>()
            .join(" ");

        embedder
            .embed_batch(&[&token_dense_text])
            .await
            .expect("embedding request succeeds");

        let request = request_rx.await.expect("captured request");
        let body = request
            .split("\r\n\r\n")
            .nth(1)
            .expect("request body present");
        let json: Value = serde_json::from_str(body).expect("request json");
        let input = json["input"][0].as_str().expect("input text");

        assert!(input.split_whitespace().count() <= token_budget);
        assert!(input.ends_with('…'));
    }

    #[test]
    fn request_input_text_truncates_token_dense_text_without_whitespace() {
        let token_dense_text = "/".repeat(OpenRouterEmbedder::MAX_INPUT_CHARS + 64);
        let truncated = OpenRouterEmbedder::request_input_text(&token_dense_text);

        assert!(truncated.chars().count() <= OpenRouterEmbedder::MAX_INPUT_TOKENS + 1);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn long_input_text_is_truncated() {
        let long_text = "x".repeat(3000);
        let truncated = OpenRouterEmbedder::request_input_text(&long_text);

        assert_eq!(truncated.len(), OpenRouterEmbedder::MAX_INPUT_CHARS + 3);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn long_multibyte_input_text_is_truncated_on_char_boundary() {
        let long_text = "€".repeat(3000);
        let truncated = OpenRouterEmbedder::request_input_text(&long_text);

        assert_eq!(
            truncated.chars().count(),
            OpenRouterEmbedder::MAX_INPUT_CHARS + 1
        );
        assert!(truncated.ends_with('…'));
    }

    /// Spawn a server that accepts `fail_count` connections and immediately
    /// closes them (simulating a mid-stream reset), then on the next accept
    /// serves a valid embedding response.
    async fn transient_failing_openrouter_server(fail_count: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
        let addr = listener.local_addr().expect("server addr");
        tokio::spawn(async move {
            for _ in 0..fail_count {
                let (mut stream, _) = listener.accept().await.expect("accept failing request");
                // Read the request then drop the stream without writing a response
                // — this simulates a mid-stream connection reset.
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                drop(stream);
            }
            // Serve a valid response on the final accept.
            let (mut stream, _) = listener.accept().await.expect("accept good request");
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let body = r#"{"data":[{"index":0,"embedding":[0.5,0.6]}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write good response");
        });
        format!("http://{addr}/embeddings")
    }

    /// Spawn a server that returns HTTP 503 `fail_count` times, then a valid
    /// 200 response.  The `attempt_count` sender fires once per accepted
    /// connection so callers can assert how many requests were made.
    async fn flaky_status_openrouter_server(
        fail_count: usize,
    ) -> (String, tokio::sync::mpsc::Receiver<u32>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
        let addr = listener.local_addr().expect("server addr");
        let (tx, rx) = tokio::sync::mpsc::channel::<u32>(16);
        let mut attempt: u32 = 0;
        tokio::spawn(async move {
            for _ in 0..fail_count {
                attempt += 1;
                let (mut stream, _) = listener.accept().await.expect("accept 503 request");
                let _ = tx.send(attempt).await;
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let body = r#"{"error":"service unavailable"}"#;
                let response = format!(
                    "HTTP/1.1 503 Service Unavailable\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write 503 response");
            }
            // Serve a valid response on the final accept.
            attempt += 1;
            let (mut stream, _) = listener.accept().await.expect("accept good request");
            let _ = tx.send(attempt).await;
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let body = r#"{"data":[{"index":0,"embedding":[1.0,2.0]}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write good response");
        });
        (format!("http://{addr}/embeddings"), rx)
    }

    /// A server that tracks how many requests it received, closes after
    /// serving exactly `max_requests` requests (each with the provided body).
    async fn counting_openrouter_server(
        max_requests: usize,
        body: &'static str,
        status: u16,
    ) -> (String, tokio::sync::mpsc::Receiver<u32>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
        let addr = listener.local_addr().expect("server addr");
        let (tx, rx) = tokio::sync::mpsc::channel::<u32>(16);
        tokio::spawn(async move {
            for i in 0..max_requests {
                let (mut stream, _) = listener.accept().await.expect("accept request");
                let _ = tx.send(i as u32 + 1).await;
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let status_text = if status == 200 { "OK" } else { "Error" };
                let response = format!(
                    "HTTP/1.1 {status} {status_text}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write response");
            }
        });
        (format!("http://{addr}/embeddings"), rx)
    }

    /// Retry recovers from a transient connection drop: server drops the first
    /// connection, then serves a valid response on the second attempt.
    #[tokio::test]
    async fn embed_batch_retries_after_connection_drop() {
        let endpoint = transient_failing_openrouter_server(1).await;
        let embedder = OpenRouterEmbedder {
            client: build_client(),
            api_key: "test-key".to_owned(),
            endpoint,
        };

        let result = embedder
            .embed_batch(&["hello"])
            .await
            .expect("should succeed after retry");
        assert_eq!(result, vec![vec![0.5f32, 0.6f32]]);
    }

    /// Retry recovers from a transient 503: server returns 503 once, then a
    /// valid 200 response.
    #[tokio::test]
    async fn embed_batch_retries_after_503() {
        let (endpoint, mut rx) = flaky_status_openrouter_server(1).await;
        let embedder = OpenRouterEmbedder {
            client: build_client(),
            api_key: "test-key".to_owned(),
            endpoint,
        };

        let result = embedder
            .embed_batch(&["hello"])
            .await
            .expect("should succeed after retry on 503");
        assert_eq!(result, vec![vec![1.0f32, 2.0f32]]);

        // Confirm the server received exactly 2 requests (1 failing + 1 good).
        let mut count = 0u32;
        while let Ok(n) = rx.try_recv() {
            count = n;
        }
        assert_eq!(count, 2, "expected exactly 2 requests (1 retry)");
    }

    /// Non-retryable: a 200 with an OpenRouter error body should fail fast
    /// without retry — the server should only receive exactly one request.
    #[tokio::test]
    async fn embed_batch_does_not_retry_openrouter_api_error_in_200_body() {
        let error_body = r#"{"error":{"message":"HTTP 400: input too long","code":400}}"#;
        // Allow up to MAX_RETRIES+1 requests, but assert only 1 was made.
        let (endpoint, mut rx) = counting_openrouter_server(MAX_RETRIES + 1, error_body, 200).await;
        let embedder = OpenRouterEmbedder {
            client: build_client(),
            api_key: "test-key".to_owned(),
            endpoint,
        };

        let error = embedder
            .embed_batch(&["hello"])
            .await
            .expect_err("API error in 200 body should fail without retry");
        let message = error.to_string();
        assert!(message.contains("OpenRouter API error"));
        assert!(message.contains("HTTP 400: input too long"));

        // Only one request should have reached the server.
        let mut count = 0u32;
        while let Ok(n) = rx.try_recv() {
            count = n;
        }
        assert_eq!(count, 1, "non-retryable error must not trigger retries");
    }

    /// Non-retryable: a 400 Bad Request should fail fast (not retried).
    #[tokio::test]
    async fn embed_batch_does_not_retry_400_error() {
        let error_body = r#"{"error":"bad request"}"#;
        let (endpoint, mut rx) = counting_openrouter_server(MAX_RETRIES + 1, error_body, 400).await;
        let embedder = OpenRouterEmbedder {
            client: build_client(),
            api_key: "test-key".to_owned(),
            endpoint,
        };

        let error = embedder
            .embed_batch(&["hello"])
            .await
            .expect_err("400 should fail without retry");
        let message = error.to_string();
        assert!(message.contains("400"));

        let mut count = 0u32;
        while let Ok(n) = rx.try_recv() {
            count = n;
        }
        assert_eq!(count, 1, "4xx non-429 must not trigger retries");
    }

    /// Exhaustion: if all MAX_RETRIES+1 attempts fail (connection drop), the
    /// error should propagate with the original error context intact.
    #[tokio::test]
    async fn embed_batch_exhausts_retries_and_returns_error() {
        // Server that drops MAX_RETRIES+1 connections, never serves a good response.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let total = MAX_RETRIES + 1;
        tokio::spawn(async move {
            for _ in 0..total {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                drop(stream);
            }
        });
        let endpoint = format!("http://{addr}/embeddings");
        let embedder = OpenRouterEmbedder {
            client: build_client(),
            api_key: "test-key".to_owned(),
            endpoint,
        };

        let error = embedder
            .embed_batch(&["hello"])
            .await
            .expect_err("all retries exhausted should return error");
        // Error context should still reference the send/body-read failure.
        let msg = error.to_string();
        assert!(
            msg.contains("failed to send OpenRouter embeddings request")
                || msg.contains("failed to read OpenRouter embeddings response body"),
            "unexpected error message: {msg}"
        );
    }
}
