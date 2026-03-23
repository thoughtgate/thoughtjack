//! Shared retry logic for LLM API providers.
//!
//! Handles HTTP status-based retries (429 rate limit), auth error detection
//! (401/403), and malformed JSON retries with exponential backoff.
//!
//! Both `OpenAiCompatibleProvider` and `AnthropicProvider` delegate their
//! retry loops to [`send_with_retry`].

use std::time::Duration;

use serde_json::Value;

use crate::transport::context::ProviderError;

/// Maximum number of retries for rate limiting and parse errors.
const MAX_RETRIES: u32 = 3;

/// Initial retry backoff in seconds (doubles on each retry).
const INITIAL_RETRY_BACKOFF_SECS: u64 = 1;

/// Sends an HTTP request with retry handling for rate limits and parse errors.
///
/// The caller provides a closure that builds and sends a single HTTP request.
/// This function handles:
/// - **401/403**: Immediate auth error (no retry)
/// - **429**: Retry with exponential backoff
/// - **Other non-success**: Immediate HTTP error
/// - **JSON parse failure**: Retry once with 1s delay
///
/// Returns the parsed JSON body on success.
///
/// # Errors
///
/// Returns `ProviderError::Http` for auth and HTTP errors,
/// `ProviderError::RateLimited` when retries are exhausted,
/// `ProviderError::Parse` for persistent JSON parse failures.
///
/// Implements: TJ-SPEC-022 F-001
pub async fn send_with_retry<F, Fut>(mut make_request: F) -> Result<Value, ProviderError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<reqwest::Response, reqwest::Error>>,
{
    let mut retries = 0;
    loop {
        let resp = make_request().await?;
        let status = resp.status().as_u16();

        // Fail immediately on auth errors
        if status == 401 || status == 403 {
            let resp_body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Http {
                status,
                body: resp_body,
            });
        }

        // Retry on rate limit
        if status == 429 {
            retries += 1;
            if retries > MAX_RETRIES {
                return Err(ProviderError::RateLimited { retries });
            }
            let backoff = INITIAL_RETRY_BACKOFF_SECS * (1 << (retries - 1));
            tracing::warn!(
                retry = retries,
                backoff_secs = backoff,
                "rate limited, retrying"
            );
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            continue;
        }

        if !resp.status().is_success() {
            let resp_body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Http {
                status,
                body: resp_body,
            });
        }

        let resp_body: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                // EC-CTX-008: retry once on malformed JSON
                retries += 1;
                if retries > MAX_RETRIES {
                    return Err(ProviderError::Parse(format!("JSON parse error: {e}")));
                }
                tracing::warn!(error = %e, "malformed provider JSON, retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        return Ok(resp_body);
    }
}
