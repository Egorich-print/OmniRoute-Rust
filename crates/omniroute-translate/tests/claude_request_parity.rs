//! Parity test: `openai → claude` request conversion (text path).
//!
//! The fixture (`fixtures/openai_to_claude.json`) was produced by running
//! the JavaScript `openaiToClaudeRequest` over three text-only requests.
//! This test asserts the Rust port builds byte-equivalent Claude bodies
//! (compared as JSON values).

use omniroute_translate::claude_request::openai_to_claude;
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
fn openai_to_claude_matches_js_ground_truth() {
    let cases: Vec<FixtureCase> =
        serde_json::from_str(include_str!("fixtures/openai_to_claude.json"))
            .expect("fixture parses");
    assert_eq!(cases.len(), 3);
    for case in &cases {
        let got = openai_to_claude(&case.input.model, &case.input.body, true);
        assert_eq!(got, case.expected, "case {} mismatch", case.name);
    }
}
