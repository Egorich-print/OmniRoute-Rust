//! OpenAI chat → Gemini `generateContent` request conversion.
//!
//! Ports the text/tool core of `openaiToGeminiBase` in
//! `open-sse/translator/request/openai-to-gemini.ts`: system instruction
//! extraction, `contents` with `user`/`model` roles, `generationConfig`,
//! function declarations and tool results.
//!
//! Explicitly deferred: thinking/reasoning config, safety overrides,
//! multimodal inline data beyond text, and cached content.

use serde_json::{Map, Value, json};

/// Convert an OpenAI chat body into a Gemini `generateContent` body.
pub fn openai_to_gemini(body: &Value) -> Value {
    let mut contents: Vec<Value> = Vec::new();
    let mut system_text: Option<String> = None;
    // call_id → function name, so `tool` results can name the function.
    let mut call_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for message in messages {
            let role = message.get("role").and_then(Value::as_str).unwrap_or("");
            match role {
                "system" | "developer" => {
                    let text = content_text(message.get("content"));
                    if !text.is_empty() {
                        system_text = Some(match system_text {
                            Some(existing) => format!("{existing}\n\n{text}"),
                            None => text,
                        });
                    }
                }
                "assistant" => {
                    let mut parts: Vec<Value> = Vec::new();
                    let text = content_text(message.get("content"));
                    if !text.is_empty() {
                        parts.push(json!({"text": text}));
                    }
                    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                        for call in calls {
                            let name = call
                                .get("function")
                                .and_then(|function| function.get("name"))
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            if name.is_empty() {
                                continue;
                            }
                            if let Some(id) = call.get("id").and_then(Value::as_str) {
                                call_names.insert(id.to_string(), name.to_string());
                            }
                            parts.push(json!({
                                "functionCall": {
                                    "name": name,
                                    "args": arguments_object(
                                        call.get("function").and_then(|f| f.get("arguments"))
                                    ),
                                }
                            }));
                        }
                    }
                    if !parts.is_empty() {
                        push_turn(&mut contents, "model", parts);
                    }
                }
                "tool" | "function" => {
                    let call_id = message
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let name = call_names
                        .get(call_id)
                        .cloned()
                        .or_else(|| {
                            message
                                .get("name")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .unwrap_or_else(|| "tool".to_string());
                    let text = content_text(message.get("content"));
                    let response = match serde_json::from_str::<Value>(&text) {
                        Ok(Value::Object(_)) => serde_json::from_str(&text).unwrap_or(json!(text)),
                        _ => json!({ "result": text }),
                    };
                    push_turn(
                        &mut contents,
                        "user",
                        vec![json!({
                            "functionResponse": { "name": name, "response": response }
                        })],
                    );
                }
                _ => {
                    let text = content_text(message.get("content"));
                    if !text.is_empty() {
                        push_turn(&mut contents, "user", vec![json!({"text": text})]);
                    }
                }
            }
        }
    }

    let mut result = Map::new();
    result.insert("contents".to_string(), Value::Array(contents));

    if let Some(system_text) = system_text {
        result.insert(
            "systemInstruction".to_string(),
            json!({ "parts": [{ "text": system_text }] }),
        );
    }

    let mut config = Map::new();
    for (source, target) in [
        ("temperature", "temperature"),
        ("top_p", "topP"),
        ("top_k", "topK"),
    ] {
        if let Some(value) = body.get(source) {
            config.insert(target.to_string(), value.clone());
        }
    }
    if let Some(stop) = body.get("stop") {
        let sequences = match stop {
            Value::Array(items) => items.clone(),
            other => vec![other.clone()],
        };
        config.insert("stopSequences".to_string(), Value::Array(sequences));
    }
    if let Some(max_tokens) = body
        .get("max_tokens")
        .or_else(|| body.get("max_completion_tokens"))
        .and_then(Value::as_u64)
    {
        config.insert("maxOutputTokens".to_string(), json!(max_tokens));
    }
    if !config.is_empty() {
        result.insert("generationConfig".to_string(), Value::Object(config));
    }

    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let declarations: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                let function = tool.get("function")?;
                let name = function.get("name").and_then(Value::as_str)?;
                if name.is_empty() {
                    return None;
                }
                let mut declaration = Map::new();
                declaration.insert("name".to_string(), Value::from(name));
                if let Some(description) = function.get("description").and_then(Value::as_str) {
                    declaration.insert("description".to_string(), Value::from(description));
                }
                if let Some(parameters) = function.get("parameters") {
                    declaration.insert("parameters".to_string(), parameters.clone());
                }
                Some(Value::Object(declaration))
            })
            .collect();
        if !declarations.is_empty() {
            result.insert(
                "tools".to_string(),
                json!([{ "functionDeclarations": declarations }]),
            );
        }
    }

    if let Some(choice) = body.get("tool_choice") {
        let mode = match choice {
            Value::String(mode) => match mode.as_str() {
                "none" => Some("NONE"),
                "required" | "any" => Some("ANY"),
                "auto" => Some("AUTO"),
                _ => None,
            },
            Value::Object(object) => {
                if object.get("type").and_then(Value::as_str) == Some("function") {
                    Some("ANY")
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(mode) = mode {
            result.insert(
                "toolConfig".to_string(),
                json!({ "functionCallingConfig": { "mode": mode } }),
            );
        }
    }

    Value::Object(result)
}

/// Append parts, merging into the previous turn when the role repeats
/// (Gemini wants alternating turns).
fn push_turn(contents: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if let Some(last) = contents.last_mut()
        && last.get("role").and_then(Value::as_str) == Some(role)
        && let Some(existing) = last.get_mut("parts").and_then(Value::as_array_mut)
    {
        existing.extend(parts);
        return;
    }
    contents.push(json!({ "role": role, "parts": parts }));
}

/// Plain text of a chat content value (string or `text` parts).
fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// OpenAI tool arguments are a JSON string; Gemini wants an object.
fn arguments_object(arguments: Option<&Value>) -> Value {
    match arguments {
        Some(Value::String(text)) => {
            serde_json::from_str::<Value>(text).unwrap_or_else(|_| json!({}))
        }
        Some(value) if value.is_object() => value.clone(),
        _ => json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_system_text_and_generation_config() {
        let body = json!({
            "messages": [
                {"role": "system", "content": "Be brief."},
                {"role": "user", "content": "Hi"}
            ],
            "temperature": 0.2,
            "max_tokens": 100
        });
        let out = openai_to_gemini(&body);
        assert_eq!(
            out["systemInstruction"]["parts"][0]["text"],
            json!("Be brief.")
        );
        assert_eq!(out["contents"][0]["role"], json!("user"));
        assert_eq!(out["contents"][0]["parts"][0]["text"], json!("Hi"));
        assert_eq!(out["generationConfig"]["temperature"], json!(0.2));
        assert_eq!(out["generationConfig"]["maxOutputTokens"], json!(100));
    }

    #[test]
    fn maps_tool_calls_and_results_with_names() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "time?"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function",
                     "function": {"name": "get_time", "arguments": "{\"city\":\"SF\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "12:00"}
            ],
            "tools": [{"type": "function", "function": {
                "name": "get_time", "description": "Get time",
                "parameters": {"type": "object", "properties": {}}
            }}]
        });
        let out = openai_to_gemini(&body);
        let model_turn = &out["contents"][1];
        assert_eq!(model_turn["role"], json!("model"));
        assert_eq!(
            model_turn["parts"][0]["functionCall"]["name"],
            json!("get_time")
        );
        assert_eq!(
            model_turn["parts"][0]["functionCall"]["args"]["city"],
            json!("SF")
        );
        // The tool result carries the function NAME, resolved from the call id.
        assert_eq!(
            out["contents"][2]["parts"][0]["functionResponse"]["name"],
            json!("get_time")
        );
        assert_eq!(
            out["tools"][0]["functionDeclarations"][0]["name"],
            json!("get_time")
        );
    }

    #[test]
    fn merges_consecutive_same_role_turns() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "a"},
                {"role": "user", "content": "b"}
            ]
        });
        let out = openai_to_gemini(&body);
        assert_eq!(out["contents"].as_array().unwrap().len(), 1);
        assert_eq!(out["contents"][0]["parts"].as_array().unwrap().len(), 2);
    }
}
