//! OpenAI chat → Responses API request conversion.
//!
//! Mirrors `openaiToOpenAIResponsesRequest` in
//! `open-sse/translator/request/openai-responses/toResponses.ts` for the
//! core text/tool/param path.
//!
//! Explicitly deferred (documented, not silently dropped): assistant
//! reasoning-value extraction, `verbosity`, `reasoning`/`reasoning_effort`
//! normalisation, the credential-gated `store:true` path, and multimodal
//! edge shapes beyond the common `image_url`/`file` parts. Unknown content
//! parts are preserved verbatim, exactly as the JS adapter does.

use serde_json::{Map, Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic fallback for omitted tool-call ids. The JS adapter mints
/// `call_<timestamp>_<random>`; only the uniqueness (not the shape) matters
/// downstream, and fixtures always carry explicit ids.
static CALL_ID_SEQ: AtomicU64 = AtomicU64::new(0);

/// Responses API rejects `call_id` longer than 64 characters.
fn clamp_call_id(id: &str) -> String {
    const MAX_LEN: usize = 64;
    if id.len() > MAX_LEN {
        id[..MAX_LEN].to_string()
    } else {
        id.to_string()
    }
}

fn generate_tool_call_id() -> String {
    let seq = CALL_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("call_fallback_{seq}")
}

/// `toString(value, fallback)` — string or fallback, never coerced.
fn s(value: Option<&Value>) -> &str {
    value.and_then(Value::as_str).unwrap_or("")
}

/// Flatten chat content into the string `instructions` takes: text parts
/// joined, bare strings kept, anything non-textual dropped.
fn instructions_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(text) => Some(text.clone()),
                Value::Object(_) => {
                    let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                    if part.get("type").and_then(Value::as_str) == Some("text")
                        || part.get("text").and_then(Value::as_str).is_some()
                    {
                        Some(text.to_string())
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// Chat content block → Responses `input_text` part array.
fn text_parts(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(text)) => vec![json!({"type": "input_text", "text": text})],
        Some(Value::Array(parts)) => {
            let mut out = Vec::new();
            for part in parts {
                if let Value::String(text) = part {
                    out.push(json!({"type": "input_text", "text": text}));
                    continue;
                }
                let is_text = part.get("type").and_then(Value::as_str) == Some("text")
                    || part.get("text").and_then(Value::as_str).is_some();
                if is_text {
                    out.push(json!({
                        "type": "input_text",
                        "text": part.get("text").and_then(Value::as_str).unwrap_or(""),
                    }));
                }
            }
            if out.is_empty() {
                out.push(json!({"type": "input_text", "text": ""}));
            }
            out
        }
        _ => vec![json!({"type": "input_text", "text": ""})],
    }
}

/// User content block → Responses message content (text, images, files;
/// unknown parts preserved verbatim).
fn user_content(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(text)) => vec![json!({"type": "input_text", "text": text})],
        Some(Value::Array(parts)) => parts.iter().map(user_part).collect(),
        _ => vec![json!({"type": "input_text", "text": ""})],
    }
}

fn user_part(part: &Value) -> Value {
    let part_type = part.get("type").and_then(Value::as_str).unwrap_or("");
    match part_type {
        "text" => json!({
            "type": "input_text",
            "text": part.get("text").and_then(Value::as_str).unwrap_or(""),
        }),
        "image_url" => {
            let mut item = Map::new();
            item.insert("type".to_string(), Value::from("input_image"));
            match part.get("image_url") {
                Some(Value::String(url)) => {
                    item.insert("image_url".to_string(), Value::from(url.clone()));
                }
                Some(image) => {
                    item.insert(
                        "image_url".to_string(),
                        Value::from(
                            image
                                .get("url")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        ),
                    );
                    if let Some(detail) = image.get("detail") {
                        item.insert("detail".to_string(), detail.clone());
                    }
                }
                None => {
                    item.insert("image_url".to_string(), Value::from(""));
                }
            }
            Value::Object(item)
        }
        "image" => {
            // AI-SDK data-URL form. The JS adapter matches
            // `^data:([^;]+);base64,(.+)$`; a prefix check is equivalent here.
            if let Some(image) = part.get("image").and_then(Value::as_str)
                && image.starts_with("data:")
                && image.contains(";base64,")
            {
                let mut item = Map::new();
                item.insert("type".to_string(), Value::from("input_image"));
                item.insert("image_url".to_string(), Value::from(image));
                item.insert(
                    "detail".to_string(),
                    part.get("detail")
                        .cloned()
                        .unwrap_or_else(|| Value::from("auto")),
                );
                return Value::Object(item);
            }
            part.clone()
        }
        "file" | "document" => {
            let file = if part_type == "document" {
                part.get("document")
            } else {
                part.get("file")
            };
            let mut item = Map::new();
            item.insert("type".to_string(), Value::from("input_file"));
            if let Some(value) = file
                .and_then(|f| f.get("file_data"))
                .or_else(|| file.and_then(|f| f.get("data")))
            {
                item.insert("file_data".to_string(), value.clone());
            }
            if let Some(value) = file.and_then(|f| f.get("file_id")) {
                item.insert("file_id".to_string(), value.clone());
            }
            if let Some(value) = file
                .and_then(|f| f.get("file_url"))
                .or_else(|| file.and_then(|f| f.get("url")))
            {
                item.insert("file_url".to_string(), value.clone());
            }
            if let Some(value) = file
                .and_then(|f| f.get("filename"))
                .or_else(|| file.and_then(|f| f.get("name")))
            {
                item.insert("filename".to_string(), value.clone());
            }
            Value::Object(item)
        }
        _ => part.clone(),
    }
}

/// Coerce a tool-result payload the way `String(content ?? "")` does.
fn tool_output_string(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        _ => String::new(),
    }
}

/// Convert an OpenAI chat request body to a Responses API request body.
///
/// `model` is stamped through. `store` is always `false` here; the
/// credential-gated `store:true` path needs provider context the translator
/// does not own yet.
pub fn openai_to_responses(model: &str, body: &Value) -> Value {
    let mut input: Vec<Value> = Vec::new();
    let mut instructions: Option<String> = None;

    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for message in messages {
            let role = s(message.get("role"));
            match role {
                "system" | "developer" => {
                    if instructions.is_none() {
                        instructions = Some(instructions_text(message.get("content")));
                    } else {
                        input.push(json!({
                            "type": "message",
                            "role": "developer",
                            "content": text_parts(message.get("content")),
                            "status": "completed",
                        }));
                    }
                }
                "user" => {
                    input.push(json!({
                        "type": "message",
                        "role": "user",
                        "content": user_content(message.get("content")),
                        "status": "completed",
                    }));
                }
                "assistant" => {
                    let mut output_content: Vec<Value> = Vec::new();
                    match message.get("content") {
                        Some(Value::String(text)) if !text.is_empty() => {
                            output_content.push(json!({"type": "output_text", "text": text}));
                        }
                        Some(Value::Array(parts)) => {
                            for part in parts {
                                let part_type =
                                    part.get("type").and_then(Value::as_str).unwrap_or("");
                                match part_type {
                                    "text" => output_content.push(json!({
                                        "type": "output_text",
                                        "text": part.get("text").and_then(Value::as_str).unwrap_or(""),
                                    })),
                                    "image_url" => {
                                        let url = match part.get("image_url") {
                                            Some(Value::String(url)) => url.clone(),
                                            Some(image) => image
                                                .get("url")
                                                .and_then(Value::as_str)
                                                .unwrap_or("")
                                                .to_string(),
                                            None => String::new(),
                                        };
                                        let text = if url.is_empty() {
                                            "[Image]".to_string()
                                        } else {
                                            format!("[Image: {url}]")
                                        };
                                        output_content.push(
                                            json!({"type": "output_text", "text": text}),
                                        );
                                    }
                                    "thinking" | "redacted_thinking" => {}
                                    _ => output_content.push(part.clone()),
                                }
                            }
                        }
                        _ => {}
                    }
                    if !output_content.is_empty() {
                        input.push(json!({
                            "type": "message",
                            "role": "assistant",
                            "content": output_content,
                            "status": "completed",
                        }));
                    }

                    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                        for call in calls {
                            let name = s(call
                                .get("function")
                                .and_then(|function| function.get("name")))
                            .trim();
                            if name.is_empty() {
                                continue;
                            }
                            let raw_id = s(call.get("id")).trim();
                            let call_id = if raw_id.is_empty() {
                                generate_tool_call_id()
                            } else {
                                clamp_call_id(raw_id)
                            };
                            let arguments = call
                                .get("function")
                                .and_then(|function| function.get("arguments"))
                                .and_then(Value::as_str)
                                .unwrap_or("{}");
                            input.push(json!({
                                "type": "function_call",
                                "call_id": call_id,
                                "name": name,
                                "arguments": arguments,
                                "status": "completed",
                            }));
                        }
                    } else if let Some(function_call) = message.get("function_call") {
                        let name = s(function_call.get("name")).trim();
                        if !name.is_empty() {
                            let arguments = function_call
                                .get("arguments")
                                .and_then(Value::as_str)
                                .unwrap_or("{}");
                            input.push(json!({
                                "type": "function_call",
                                "call_id": clamp_call_id(&format!("call_{name}")),
                                "name": name,
                                "arguments": arguments,
                                "status": "completed",
                            }));
                        }
                    }
                }
                "tool" => {
                    let call_id = clamp_call_id(s(message.get("tool_call_id")));
                    let output = match message.get("content") {
                        Some(Value::String(text)) => Value::from(text.clone()),
                        Some(Value::Array(parts)) => Value::Array(
                            parts
                                .iter()
                                .map(|part| {
                                    if part.get("type").and_then(Value::as_str)
                                        == Some("text")
                                    {
                                        json!({
                                            "type": "input_text",
                                            "text": part.get("text").and_then(Value::as_str).unwrap_or(""),
                                        })
                                    } else {
                                        part.clone()
                                    }
                                })
                                .collect(),
                        ),
                        _ => Value::from(tool_output_string(message.get("content"))),
                    };
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": output,
                        "status": "completed",
                    }));
                }
                "function" => {
                    let name = s(message.get("name"));
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": clamp_call_id(&format!("call_{name}")),
                        "output": tool_output_string(message.get("content")),
                        "status": "completed",
                    }));
                }
                _ => {}
            }
        }
    }

    // Drop orphaned outputs (no matching function_call), as the JS adapter does.
    let known: std::collections::HashSet<String> = input
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .filter_map(|item| {
            item.get("call_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    input.retain(|item| {
        if item.get("type").and_then(Value::as_str) == Some("function_call_output")
            && let Some(call_id) = item.get("call_id").and_then(Value::as_str)
        {
            return known.contains(call_id);
        }
        true
    });

    let mut result = Map::new();
    result.insert("model".to_string(), Value::from(model));
    result.insert("input".to_string(), Value::Array(input));
    result.insert("stream".to_string(), Value::from(true));
    result.insert("store".to_string(), Value::from(false));
    result.insert(
        "instructions".to_string(),
        Value::from(instructions.unwrap_or_default()),
    );

    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let mapped: Vec<Value> = tools
            .iter()
            .map(|tool| {
                if tool.get("type").and_then(Value::as_str) == Some("function") {
                    let function = tool.get("function");
                    let mut item = Map::new();
                    item.insert("type".to_string(), Value::from("function"));
                    item.insert(
                        "name".to_string(),
                        Value::from(s(function.and_then(|f| f.get("name")))),
                    );
                    item.insert(
                        "description".to_string(),
                        Value::from(s(function.and_then(|f| f.get("description")))),
                    );
                    if let Some(parameters) = function.and_then(|f| f.get("parameters")) {
                        item.insert("parameters".to_string(), parameters.clone());
                    }
                    if let Some(strict) = function.and_then(|f| f.get("strict")) {
                        item.insert("strict".to_string(), strict.clone());
                    }
                    Value::Object(item)
                } else {
                    tool.clone()
                }
            })
            .collect();
        result.insert("tools".to_string(), Value::Array(mapped));
    }

    if let Some(choice) = body.get("tool_choice") {
        let mapped = match choice {
            Value::String(_) => choice.clone(),
            Value::Object(_) => {
                let is_function_call = choice.get("type").and_then(Value::as_str)
                    == Some("function")
                    && choice.get("function").is_some_and(Value::is_object);
                if is_function_call {
                    json!({
                        "type": "function",
                        "name": choice.get("function").and_then(|f| f.get("name")).cloned().unwrap_or(Value::Null),
                    })
                } else {
                    choice.clone()
                }
            }
            _ => choice.clone(),
        };
        result.insert("tool_choice".to_string(), mapped);
    }

    for key in [
        "previous_response_id",
        "prompt_cache_key",
        "session_id",
        "conversation_id",
        "service_tier",
        "temperature",
        "top_p",
    ] {
        if let Some(value) = body.get(key) {
            result.insert(key.to_string(), value.clone());
        }
    }

    if let Some(value) = body
        .get("max_output_tokens")
        .or_else(|| body.get("max_completion_tokens"))
        .or_else(|| body.get("max_tokens"))
    {
        result.insert("max_output_tokens".to_string(), value.clone());
    }

    if let Some(include) = body.get("include").and_then(Value::as_array)
        && !include.is_empty()
    {
        result.insert("include".to_string(), Value::Array(include.clone()));
    }

    map_response_format(body, &mut result);

    Value::Object(result)
}

/// Chat `response_format` → Responses `text.format`.
fn map_response_format(body: &Value, result: &mut Map<String, Value>) {
    let format_type = body
        .get("response_format")
        .and_then(|format| format.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if format_type == "json_object" {
        let mut text = result
            .get("text")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        text.insert("format".to_string(), json!({"type": "json_object"}));
        result.insert("text".to_string(), Value::Object(text));
        return;
    }
    if format_type != "json_schema" {
        return;
    }
    let schema = body
        .get("response_format")
        .and_then(|format| format.get("json_schema"));
    if schema.and_then(|s| s.get("schema")).is_none() {
        return;
    }
    let schema = schema.expect("schema present");
    let mut format = Map::new();
    format.insert("type".to_string(), Value::from("json_schema"));
    format.insert(
        "name".to_string(),
        Value::from(
            schema
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("codex_output_schema"),
        ),
    );
    format.insert(
        "schema".to_string(),
        schema.get("schema").cloned().unwrap_or(Value::Null),
    );
    if let Some(description) = schema.get("description") {
        format.insert("description".to_string(), description.clone());
    }
    if let Some(strict) = schema.get("strict") {
        format.insert("strict".to_string(), strict.clone());
    }
    let mut text = result
        .get("text")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    text.insert("format".to_string(), Value::Object(format));
    result.insert("text".to_string(), Value::Object(text));
}
