//! Unwrap recorded text and command fields without showing transport envelopes.

use serde_json::Value;

pub(super) fn exec_outcome(value: &Value) -> Option<editchain_core::activity::Status> {
    if value.get("type").and_then(Value::as_str) != Some("response_item") {
        return None;
    }
    let payload = value.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("custom_tool_call_output") {
        return None;
    }
    let first = payload.get("output")?.as_array()?.first()?;
    if first.get("type").and_then(Value::as_str) != Some("input_text") {
        return None;
    }
    let mut lines = first.get("text")?.as_str()?.lines();
    let status = lines.next()?;
    let elapsed = lines.next()?;
    if !elapsed.starts_with("Wall time ")
        || !elapsed.ends_with(" seconds")
        || lines.next() != Some("Output:")
    {
        return None;
    }
    match status {
        "Script completed" => Some(editchain_core::activity::Status::Success),
        "Script failed" => Some(editchain_core::activity::Status::Failure),
        _ => None,
    }
}

pub(super) fn description(text: &str) -> String {
    serde_json::from_str(text)
        .ok()
        .as_ref()
        .and_then(value)
        .unwrap_or_else(|| text.into())
}

pub(super) fn value(input: &Value) -> Option<String> {
    match input {
        Value::String(text) => Some(description(text)),
        Value::Array(values) => {
            let parts: Vec<_> = values.iter().filter_map(value).collect();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(fields) => {
            for key in [
                "cmd",
                "command",
                "text",
                "message",
                "content",
                "summary",
                "arguments",
                "input",
                "output",
                "last_agent_message",
                "description",
            ] {
                if let Some(text) = fields
                    .get(key)
                    .and_then(value)
                    .filter(|text| !text.is_empty())
                {
                    return Some(text);
                }
            }
            fields
                .get("path")
                .or_else(|| fields.get("file_path"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

pub(in crate::timeline) fn commit(message: &str) -> (String, Option<String>) {
    let message = message.trim();
    let Some((prefix, rest)) = message.split_once(": ") else {
        return (message.into(), None);
    };
    if prefix.len() <= 48
        && !prefix.contains('\n')
        && prefix.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '(' | ')' | '!' | '-' | '_' | '/')
        })
    {
        (rest.into(), Some(prefix.into()))
    } else {
        (message.into(), None)
    }
}
