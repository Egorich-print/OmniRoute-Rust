//! Explicit HTTP client.

use serde_json::Value;
use thiserror::Error;

/// HTTP failures.
#[derive(Debug, Error)]
pub enum HttpError {
    /// Transport / protocol failure.
    #[error("http transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// Upstream answered with a non-success status.
    #[error("upstream status {status}: {body}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// Truncated response body for diagnostics.
        body: String,
    },
}

/// Convenience result alias.
pub type Result<T> = std::result::Result<T, HttpError>;

/// Shared async HTTP client. Cheap to clone; holds connection pools.
#[derive(Debug, Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
}

impl HttpClient {
    /// Build a client with a total request timeout.
    pub fn new(timeout_secs: u64) -> Result<Self> {
        let inner = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build()?;
        Ok(Self { inner })
    }

    /// POST a JSON body, returning the response for the caller to stream or
    /// buffer. Bearer auth is attached when `bearer` is `Some`.
    pub async fn post_json(
        &self,
        url: &str,
        bearer: Option<&str>,
        body: &Value,
    ) -> Result<reqwest::Response> {
        self.post_json_with_headers(url, bearer, &[], body).await
    }

    /// POST a JSON body with extra headers (provider session headers, etc.).
    pub async fn post_json_with_headers(
        &self,
        url: &str,
        bearer: Option<&str>,
        headers: &[(&str, &str)],
        body: &Value,
    ) -> Result<reqwest::Response> {
        let mut request = self.inner.post(url);
        if let Some(token) = bearer {
            request = request.bearer_auth(token);
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.json(body).send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(2000)
                .collect();
            return Err(HttpError::Status {
                status: status.as_u16(),
                body,
            });
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, header, method, path},
    };

    #[tokio::test]
    async fn post_json_sends_bearer_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer tok"))
            .and(body_json(serde_json::json!({"model": "m"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .expect(1)
            .mount(&server)
            .await;

        let client = HttpClient::new(10).unwrap();
        let response = client
            .post_json(
                &format!("{}/v1/responses", server.uri()),
                Some("tok"),
                &serde_json::json!({"model": "m"}),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn non_success_status_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
            .mount(&server)
            .await;

        let client = HttpClient::new(10).unwrap();
        let error = client
            .post_json(&format!("{}/x", server.uri()), None, &serde_json::json!({}))
            .await
            .unwrap_err();
        match error {
            HttpError::Status { status, body } => {
                assert_eq!(status, 429);
                assert!(body.contains("slow down"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
