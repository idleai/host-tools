//! Readable activity descriptions derived from recorded, versioned fields.

mod provider;
mod text;
pub(super) use text::commit;

use editchain_core::{
    Op, Tags,
    activity::{FileAction, Kind, MessageKind, Operation},
};
use idle_history::human::{HumanWorkKind, HumanWorkRecord};
use serde_json::Value;

use super::model::{Fact, Visibility, key};

pub(super) fn apply(fact: &mut Fact, operation: &Operation, stored: &Op) {
    if stored.tags.matches_any(Tags::ERROR) {
        fact.outcome = editchain_core::activity::Status::Failure;
    }
    match &operation.kind {
        Kind::Original(_) => original(fact),
        Kind::Message(message) => {
            if stored.tags.matches_any(Tags::HUMAN)
                && (!fact.is_legacy()
                    || message
                        .blocks
                        .iter()
                        .all(|block| matches!(block.content, editchain_core::Payload::Inline(_))))
            {
                fact.task_title = Some((
                    fact.source.map_or(0, |source| source.seq),
                    caption(&fact.row.preview),
                ));
            }
            fact.row.title = match message.category {
                MessageKind::Reasoning | MessageKind::Plan => "plan",
                MessageKind::Text => "message",
                MessageKind::Summary => "summary",
            }
            .into();
        }
        Kind::Tool(_) => {
            let name = fact.row.title.clone();
            fact.row.title = tool_kind(&name).into();
            fact.row.tags.push(name);
            fact.row.preview = text::description(&fact.row.preview);
        }
        Kind::File(file) => {
            fact.row.title = match file.action {
                FileAction::Read => "read",
                FileAction::Create
                | FileAction::Change
                | FileAction::Save
                | FileAction::Rename
                | FileAction::Delete => "edit",
                FileAction::Open => "open",
                FileAction::Close => "close",
                FileAction::View | FileAction::Snapshot => "file",
            }
            .into();
        }
        Kind::Commit(_) => {
            fact.row.title = "git".into();
            let (summary, tag) = commit(&fact.row.preview);
            fact.row.preview = summary;
            fact.row.tags.extend(tag);
        }
        Kind::Note(note) => {
            if note.category == editchain_core::activity::NoteKind::Comment
                && note.targets.is_empty()
                && matches!(&note.content, editchain_core::Payload::Inline(_))
                && let editchain_core::ScopeRef::Turn(turn) = operation
                    .legacy
                    .as_ref()
                    .map_or(stored.scope, |legacy| legacy.scope)
            {
                fact.note_turn = Some(turn.0);
            }
            let observation = fact.row.preview == "vscode.editor.observation.v1"
                || matches!(&note.code, editchain_core::Payload::Inline(bytes) if bytes == b"idle.editor.observation");
            fact.visibility = if observation {
                Visibility::Observation
            } else if stored.tags.matches_any(Tags::META | Tags::FILE) {
                Visibility::Supporting
            } else {
                Visibility::Primary
            };
            if !fact.is_primary() {
                fact.supports = note
                    .targets
                    .iter()
                    .filter_map(|id| {
                        fact.row
                            .address
                            .record()
                            .map(|address| key(address.source, id))
                    })
                    .collect();
            }
            fact.row.title = "note".into();
        }
        Kind::Author(_) | Kind::Session(_) | Kind::Turn(_) | Kind::Link(_) => {}
    }
}

fn original(fact: &mut Fact) {
    let Ok(value) = serde_json::from_str::<Value>(&fact.row.preview) else {
        return;
    };
    if human(fact, &value) {
        return;
    }
    let payload = value.get("payload").unwrap_or(&value);
    let category = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let outer = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    fact.visibility = if category == "item_completed"
        || matches!(
            outer,
            "inter_agent_communication_metadata" | "token_usage_record"
        )
        || category == "token_count"
    {
        Visibility::Supporting
    } else {
        Visibility::Primary
    };
    if let Some(outcome) = text::exec_outcome(&value) {
        fact.outcome = outcome;
        if fact.failed() {
            fact.row.tags.push("failed".into());
        }
    }
    if provider::apply(fact, &value) {
        return;
    }
    fact.row.title = match category {
        "message" | "user_message" | "agent_message" => "message",
        "reasoning" | "agent_reasoning" => "plan",
        "function_call" | "custom_tool_call" => "run",
        "function_call_output" | "custom_tool_call_output" => "result",
        "task_started" | "task_complete" | "turn_completed" | "turn_aborted" => "session",
        _ => "system",
    }
    .into();
    if let Some(name) = payload.get("name").and_then(Value::as_str) {
        fact.row.title = tool_kind(name).into();
        fact.row.tags.push(name.into());
    }
    fact.row.preview = text::value(payload).unwrap_or_else(|| {
        if category.is_empty() {
            outer.replace('_', " ")
        } else {
            category.replace('_', " ")
        }
    });
}

fn human(fact: &mut Fact, value: &Value) -> bool {
    if value.get("source").and_then(Value::as_str) != Some("vscode.work")
        || value.get("schema").and_then(Value::as_u64) != Some(1)
    {
        return false;
    }
    let Ok(record) = serde_json::from_value::<HumanWorkRecord>(value.clone()) else {
        return false;
    };
    fact.row.title = match record.kind {
        HumanWorkKind::Edit | HumanWorkKind::ObservedEdit => "edit",
        HumanWorkKind::Read => "read",
        HumanWorkKind::Exposure => "exposure",
        HumanWorkKind::EditorOpened => "open",
        HumanWorkKind::EditorClosed => "close",
        HumanWorkKind::Gap => "gap",
    }
    .into();
    fact.row.preview = record.summary;
    fact.visibility = if record.kind == HumanWorkKind::Exposure {
        Visibility::Supporting
    } else {
        Visibility::Primary
    };
    fact.task = Some(format!("human:{}:{}", record.session, record.turn));
    fact.task_title = Some((
        fact.source.map_or(0, |source| source.seq),
        format!(
            "Human work · {}",
            record.path.as_deref().unwrap_or("Untitled buffer")
        ),
    ));
    fact.path = record.path;
    if matches!(
        record.kind,
        HumanWorkKind::Edit | HumanWorkKind::ObservedEdit
    ) {
        fact.human_edit = record
            .edit_group
            .map(|id| format!("{}:{id}", record.session));
    }
    if let Some(name) = record.user_name {
        fact.row.tags.push(name);
    }
    true
}

pub(super) fn caption(text: &str) -> String {
    text.trim()
        .chars()
        .take(160)
        .map(|ch| if ch.is_whitespace() { ' ' } else { ch })
        .collect()
}

fn tool_kind(name: &str) -> &'static str {
    let leaf = name
        .rsplit(['.', ':'])
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    match leaf.as_str() {
        "read" | "read_file" | "glob" | "grep" | "rg" | "ls" | "search" => "explore",
        "apply_patch" | "write_file" | "edit_file" | "edit" | "write" | "multiedit" => "edit",
        "update_plan" | "plan" | "todowrite" => "plan",
        "spawn_agent" | "send_message" | "followup_task" | "wait_agent" | "task" => "coordinate",
        _ => "run",
    }
}
