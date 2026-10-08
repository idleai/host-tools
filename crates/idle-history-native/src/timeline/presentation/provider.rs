//! Display fields from explicit provider item records.

use serde_json::Value;

use super::{super::model::Fact, text, tool_kind};

pub(super) fn apply(fact: &mut Fact, value: &Value) -> bool {
    let payload = value.get("payload").unwrap_or(value);
    let Some(item) = payload.get("item") else {
        return false;
    };
    let Some(kind) = item.get("type").and_then(Value::as_str) else {
        return false;
    };
    let status = item
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if matches!(
        status.to_ascii_lowercase().as_str(),
        "failed" | "error" | "cancelled" | "interrupted"
    ) || item
        .get("exit_code")
        .and_then(Value::as_i64)
        .is_some_and(|code| code != 0)
        || item.pointer("/result/isError").and_then(Value::as_bool) == Some(true)
    {
        fact.outcome = editchain_core::activity::Status::Failure;
    }
    fact.protected = matches!(
        status.to_ascii_lowercase().as_str(),
        "inprogress" | "in_progress" | "running"
    );
    let (title, preview) = match kind {
        "AgentMessage" | "UserMessage" => ("message", field(item, &["content", "text"])),
        "Reasoning" => ("plan", field(item, &["summary_text", "raw_content"])),
        "Plan" => ("plan", field(item, &["text", "plan", "explanation"])),
        "CommandExecution" => ("run", command(item, fact.protected)),
        "McpToolCall" | "DynamicToolCall" => {
            let name = item
                .get("tool")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("tool");
            fact.row.tags.push(name.into());
            (
                tool_kind(name),
                if fact.protected {
                    field(item, &["arguments"])
                } else {
                    output(item, &["result", "error", "output"])
                },
            )
        }
        "FileChange" => ("edit", changes(item)),
        "SubAgentActivity" => ("coordinate", field(item, &["kind", "agent_path"])),
        _ => return false,
    };
    fact.row.title = title.into();
    fact.row.preview = preview;
    if kind == "UserMessage" && !fact.is_legacy() {
        fact.task_title = Some((
            fact.source.map_or(0, |source| source.seq),
            super::caption(&fact.row.preview),
        ));
    }
    if fact.failed() {
        fact.row.tags.push("failed".into());
    }
    true
}

fn command(item: &Value, running: bool) -> String {
    if !running {
        return output(item, &["aggregated_output", "stdout", "stderr"]);
    }
    item.get("command").and_then(Value::as_array).map_or_else(
        || field(item, &["command"]),
        |words| {
            words
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        },
    )
}

fn output(item: &Value, fields: &[&str]) -> String {
    let result = field(item, fields);
    if result.is_empty() {
        "No output".into()
    } else {
        result
    }
}

fn field(item: &Value, fields: &[&str]) -> String {
    fields
        .iter()
        .find_map(|name| {
            item.get(name)
                .and_then(text::value)
                .filter(|text| !text.is_empty())
        })
        .unwrap_or_default()
}

fn changes(item: &Value) -> String {
    let Some(changes) = item.get("changes") else {
        return String::new();
    };
    if let Some(changes) = changes.as_object() {
        return changes.keys().cloned().collect::<Vec<_>>().join(", ");
    }
    changes
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|change| change.get("path").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(", ")
}
