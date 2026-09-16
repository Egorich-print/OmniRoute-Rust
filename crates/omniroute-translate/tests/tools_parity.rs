//! Parity test: `openai-responses → openai` incremental tool calls.
//!
//! The fixture (`fixtures/responses_tools.json`) was produced by running
//! the JavaScript translator over an `.added` → deltas → `.done` →
//! `.completed` sequence. It asserts the buffered-arguments path: deltas
//! emit nothing, `.done` emits the accumulated arguments, and the terminal
//! reason is `tool_calls`.

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
struct FixtureCase {
    input: Value,
    expected: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    state: FixtureState,
    cases: Vec<FixtureCase>,
}

fn event_type(input: &Value) -> &str {
    input["type"].as_str().expect("input has type")
}

#[test]
fn responses_tools_match_js_ground_truth() {
    let fixture: Fixture = serde_json::from_str(include_str!("fixtures/responses_tools.json"))
        .expect("fixture parses");
    assert_eq!(fixture.cases.len(), 5);
    let mut state = ResponsesState::new(
        fixture.state.chat_id,
        fixture.state.created,
        fixture.state.model,
    );

    for (index, case) in fixture.cases.iter().enumerate() {
        let outcome = convert_event(&mut state, event_type(&case.input), &case.input);
        match (&outcome, &case.expected) {
            (Converted::Emit(got), Some(want)) => {
                assert_eq!(got, want, "case {index} chunk mismatch");
            }
            (Converted::Ignore, None) => {}
            _ => panic!("case {index}: got {outcome:?}, want {:?}", case.expected),
        }
    }
}
