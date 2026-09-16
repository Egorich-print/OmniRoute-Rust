//! Parity test: `openai-responses → openai` streaming conversion.
//!
//! The fixture (`fixtures/responses_to_openai.json`) was produced by running
//! the JavaScript translator (`openaiResponsesToOpenAIResponse`) over a
//! representative grok-cli event sequence. This test asserts the Rust port
//! emits byte-equivalent chunks (compared as JSON values) in the same order,
//! ignoring nothing it should emit and emitting nothing it should ignore.

use omniroute_translate::responses::{Converted, ResponsesState, convert_event};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct FixtureState {
    chat_id: String,
    created: i64,
    model: String,
}

#[derive(Debug, Deserialize)]
struct CaseInput {
    #[serde(rename = "type")]
    event_type: String,
    data: Value,
}

#[derive(Debug, Deserialize)]
struct FixtureCase {
    input: CaseInput,
    expected: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    state: FixtureState,
    cases: Vec<FixtureCase>,
}

#[test]
fn responses_to_openai_matches_js_ground_truth() {
    let fixture: Fixture = serde_json::from_str(include_str!("fixtures/responses_to_openai.json"))
        .expect("fixture parses");
    let mut state = ResponsesState::new(
        fixture.state.chat_id,
        fixture.state.created,
        fixture.state.model,
    );

    for (index, case) in fixture.cases.iter().enumerate() {
        let outcome = convert_event(&mut state, &case.input.event_type, &case.input.data);
        match (&outcome, &case.expected) {
            (Converted::Emit(got), Some(want)) => {
                assert_eq!(got, want, "case {index} chunk mismatch");
            }
            (Converted::Ignore, None) => {}
            _ => panic!(
                "case {index} ({}): got {outcome:?}, want {:?}",
                case.input.event_type, case.expected
            ),
        }
    }
}
