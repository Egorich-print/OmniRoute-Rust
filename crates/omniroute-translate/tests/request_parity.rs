//! Parity test: `openai → openai-responses` request conversion.
//!
//! The fixture (`fixtures/openai_to_responses.json`) was produced by running
//! the JavaScript `openaiToOpenAIResponsesRequest` over four representative
//! chat requests. This test asserts the Rust port builds byte-equivalent
//! Responses bodies (compared as JSON values).

use omniroute_translate::requests::openai_to_responses;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct FixtureCase {
    name: String,
    input: FixtureInput,
    expected: Value,
}

#[derive(Debug, Deserialize)]
struct FixtureInput {
    model: String,
    body: Value,
}

#[test]
fn openai_to_responses_matches_js_ground_truth() {
    let cases: Vec<FixtureCase> =
        serde_json::from_str(include_str!("fixtures/openai_to_responses.json"))
            .expect("fixture parses");
    assert_eq!(cases.len(), 4);
    for case in &cases {
        let got = openai_to_responses(&case.input.model, &case.input.body);
        assert_eq!(got, case.expected, "case {} mismatch", case.name);
    }
}
