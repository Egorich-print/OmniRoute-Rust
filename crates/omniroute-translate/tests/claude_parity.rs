//! Parity test: `claude → openai` streaming conversion (text path).
//!
//! The fixture (`fixtures/claude_to_openai.json`) was produced by running
//! the JavaScript `claudeToOpenAIResponse` over a representative text stream
//! (`created` normalised). This test asserts the Rust port emits
//! byte-equivalent chunks in the same order.

use omniroute_translate::claude::{ClaudeState, Converted, convert_event};
use serde::Deserialize;
use serde_json::Value;

const NOW: i64 = 1700000000;

#[derive(Debug, Deserialize)]
struct FixtureCase {
    input: Value,
    expected: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    cases: Vec<FixtureCase>,
}

#[test]
fn claude_to_openai_matches_js_ground_truth() {
    let fixture: Fixture = serde_json::from_str(include_str!("fixtures/claude_to_openai.json"))
        .expect("fixture parses");
    assert_eq!(fixture.cases.len(), 9);
    let mut state = ClaudeState::default();

    for (index, case) in fixture.cases.iter().enumerate() {
        let event_type = case.input["type"].as_str().expect("input has type");
        let outcome = convert_event(&mut state, event_type, &case.input, NOW);
        match (&outcome, &case.expected) {
            (Converted::Emit(got), Some(want)) => {
                assert_eq!(got, want, "case {index} chunk mismatch");
            }
            (Converted::Ignore, None) => {}
            _ => panic!(
                "case {index} ({event_type}): got {outcome:?}, want {:?}",
                case.expected
            ),
        }
    }
}
