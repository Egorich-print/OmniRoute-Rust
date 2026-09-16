//! Parity-harness validation.
//!
//! A `wiremock` stand-in for an upstream provider serves a canned OpenAI
//! `/v1/models` payload, fetched with the real HTTP client. Phase 3
//! executors reuse exactly this shape with provider-specific stubs; this test
//! only proves the harness (mock boot, stub matching, request counting).

use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

#[tokio::test]
async fn mock_upstream_models_round_trip() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "object": "list",
            "data": [
                { "id": "grok-4.6", "object": "model", "owned_by": "omniroute" }
            ],
        })))
        .expect(1)
        .mount(&server)
        .await;

    let url = format!("{}/v1/models", server.uri());
    let body: serde_json::Value = reqwest::get(url).await.unwrap().json().await.unwrap();

    assert_eq!(body["data"][0]["id"], "grok-4.6");
}
