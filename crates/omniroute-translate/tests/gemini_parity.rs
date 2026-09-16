//! Parity test: `gemini → openai` streaming conversion (text path).
//!
//! The fixture (`fixtures/gemini_to_openai.json`) was produced by running
//! the JavaScript `geminiToOpenAIResponse` over a three-payload text stream
//! (`created` normalised). One input routinely yields two chunks, so each
//! case asserts the whole emitted list in order.

use omniroute_translate::gemini::{GeminiState, convert_event};
use serde::Deserialize;
use serde_json::Value;

const NOW: i64 = 1700000000;

#[derive(Debug, Deserialize)]
struct FixtureCase {
    input: Value,
    expected: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    cases: Vec<FixtureCase>,
}

#[test]
fn gemini_to_openai_matches_js_ground_truth() {
    let fixture: Fixture = serde_json::from_str(include_str!("fixtures/gemini_to_openai.json"))
        .expect("fixture parses");
    assert_eq!(fixture.cases.len(), 3);
    let mut state = GeminiState::default();

    for (index, case) in fixture.cases.iter().enumerate() {
        let got = convert_event(&mut state, &case.input, NOW);
        assert_eq!(got, case.expected, "case {index} mismatch");
    }
    assert!(state.upstream_error.is_none());
}
