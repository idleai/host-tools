//! Task status is accepted only from a verified importer slot and exact turn scope.

use std::collections::BTreeSet;

use editchain_core::SourceId;
use editchain_index::OrderedSet;
use idle_history::provider::CodexDerivationEvidence;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::super::model::Snapshot;
use super::{Item, validate};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) enum Status {
    Active,
    Completed,
    Failed,
    Interrupted,
    Unknown,
}

pub(super) fn apply(
    snapshot: &mut Snapshot,
    source: SourceId,
    meta: &CodexDerivationEvidence,
    remove: bool,
    items: &mut BTreeSet<Item>,
) -> BTreeSet<SourceId> {
    let mut hidden = BTreeSet::new();
    for output in &meta.outputs {
        let Some((turn, status)) = observation(snapshot, source, meta, *output) else {
            continue;
        };
        let turn = ((source.node.0, source.boot), meta.thread.0.clone(), turn);
        let records = snapshot.logical.statuses.entry(turn.clone()).or_default();
        if remove {
            let _removed = records.remove(&source);
        } else {
            let _old = records.insert(source, status);
        }
        items.extend(
            snapshot
                .logical
                .turns
                .get(&turn)
                .into_iter()
                .flat_map(OrderedSet::iter)
                .cloned(),
        );
        if !matches!(status, Status::Failed | Status::Interrupted) {
            let _inserted = hidden.insert(*output);
        }
    }
    hidden
}

fn observation(
    snapshot: &Snapshot,
    source: SourceId,
    meta: &CodexDerivationEvidence,
    output: SourceId,
) -> Option<(String, Status)> {
    let fact = validate::record(snapshot, output)?;
    let scope = fact.note_turn?;
    let (turn, summary) = fact.row.preview.split_once(": ")?;
    if turn.is_empty()
        || scope != hash64(&format!("editchain:turn:{}:{turn}", meta.thread.0))
        || output.boot != source.boot
        || output.seq != source.seq.checked_add(1)?
    {
        return None;
    }
    let valid = (0..meta.outputs.len()).any(|index| {
        let slot = serde_json::json!([meta.contract, source.node, {"Turn": [turn, index]}]);
        output.node.0 == hash64(&format!("editchain:node:{slot}"))
    });
    if !valid {
        return None;
    }
    let status = match summary.split_whitespace().next()? {
        "inProgress" => Status::Active,
        "completed" => Status::Completed,
        "failed" => Status::Failed,
        "interrupted" => Status::Interrupted,
        _ => Status::Unknown,
    };
    Some((turn.into(), status))
}

fn hash64(text: &str) -> u64 {
    let mut bytes = [0; 8];
    for (out, byte) in bytes.iter_mut().zip(Sha256::digest(text.as_bytes())) {
        *out = byte;
    }
    u64::from_le_bytes(bytes)
}

pub(in super::super) fn caption(
    snapshot: &Snapshot,
    task: &str,
    native_live: Option<bool>,
) -> String {
    let status = snapshot
        .logical
        .task_keys
        .get(task)
        .and_then(|turn| snapshot.logical.statuses.get(turn))
        .and_then(editchain_index::OrderedMap::last_key_value)
        .map_or(
            if let Some(live) = native_live {
                if live {
                    Status::Active
                } else {
                    Status::Completed
                }
            } else {
                Status::Unknown
            },
            |(_, status)| *status,
        );
    let title = snapshot
        .logical
        .titles
        .get(task)
        .and_then(editchain_index::OrderedMap::first_key_value)
        .map_or(
            if native_live.is_some() {
                "Task"
            } else {
                "Codex task"
            },
            |(_, title)| title.as_str(),
        );
    if task.starts_with("human:") && status == Status::Unknown {
        return title.into();
    }
    let label = match status {
        Status::Active => "In progress",
        Status::Completed => "Completed",
        Status::Failed => "Failed",
        Status::Interrupted => "Interrupted",
        Status::Unknown => "Status unknown",
    };
    format!("{title} · {label}")
}
