//! A folded task uses its first recorded prompt and only verified status notes.

use std::io;

use editchain_core::{
    NodeId, NoteOp, NoteRelationship, Op, OpKind, Payload, ScopeRef, SourceId, Tags, TurnId,
};
use editchain_engine::Engine;
use idle_history::provider::{CodexDerivationContract, CodexLogicalChange};
use sha2::{Digest, Sha256};

use super::{
    binding, latest,
    legacy::{derivation, raw, source, tool},
};

fn hash(text: &str) -> u64 {
    let mut bytes = [0; 8];
    for (output, input) in bytes.iter_mut().zip(Sha256::digest(text.as_bytes())) {
        *output = input;
    }
    u64::from_le_bytes(bytes)
}

fn status(engine: &Engine, value: u64, scope: u64) -> io::Result<()> {
    let original = raw(
        value,
        Some(value.saturating_sub(1)),
        &serde_json::json!({"type":"turn_completed"}),
    )?;
    let source = source(value);
    let slot = serde_json::json!([CodexDerivationContract::OccurrencesV2, source.node, {"Turn":["turn", 0]}]);
    let output = SourceId::new(
        NodeId(hash(&format!("editchain:node:{slot}"))),
        source.boot,
        source.seq.saturating_add(1),
    );
    let note = Op {
        id: output.id(),
        source: Some(output),
        parents: editchain_core::ParentSet::One(original.id),
        actor: original.actor,
        clock: original.clock,
        scope: ScopeRef::Turn(TurnId(scope)),
        tags: Tags::IMPORT,
        kind: OpKind::Note(NoteOp {
            target_ids: Vec::new(),
            relationship: NoteRelationship::Explains,
            content: Payload::Inline(b"turn: completed (4 items)".to_vec()),
        }),
    };
    let _admitted = engine.append(&original)?;
    let _admitted = engine.append(&note)?;
    let _admitted = engine.append(&derivation(
        200_u64.saturating_add(value),
        &original,
        Vec::new(),
        vec![output],
    )?)?;
    Ok(())
}

#[test]
fn task_caption_uses_the_first_prompt_and_rejects_an_unrelated_turn_note() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let binding = binding(directory.path());
    for value in 1_u64..=4 {
        let item = if value == 1 {
            serde_json::json!({"type":"UserMessage","content":"Review the graph\nand its labels"})
        } else {
            serde_json::json!({"type":"CommandExecution","command":"cargo test", "status":"completed", "aggregated_output":"tests passed"})
        };
        let original = raw(
            value,
            value.checked_sub(1).filter(|value| *value != 0),
            &serde_json::json!({"payload":{"item":item}}),
        )?;
        let mut activity = tool(100_u64.saturating_add(value), value);
        if value == 1 {
            activity.tags |= Tags::HUMAN;
            activity.kind = OpKind::Message(editchain_core::MessageOp {
                content: Payload::Inline(b"Review the graph\nand its labels".to_vec()),
                content_type: Payload::Empty,
            });
        }
        let output = activity
            .source
            .ok_or_else(|| io::Error::other("fixture source"))?;
        let _admitted = engine.append(&original)?;
        let _admitted = engine.append(&activity)?;
        let _admitted = engine.append(&derivation(
            200_u64.saturating_add(value),
            &original,
            vec![CodexLogicalChange::Upsert {
                turn: "turn".into(),
                item: value.to_string(),
                incarnation: source(value),
                outputs: vec![output],
            }],
            vec![output],
        )?)?;
    }
    status(&engine, 5, 999)?;
    let window = latest(&binding)?;
    let group = window
        .rows
        .iter()
        .find(|row| row.group.is_some())
        .ok_or_else(|| io::Error::other("folded task"))?;
    equal!(
        group.preview,
        "Review the graph and its labels · Status unknown",
        "unrelated note cannot complete a task"
    );
    status(&engine, 6, hash("editchain:turn:thread:turn"))?;
    let window = latest(&binding)?;
    let group = window
        .rows
        .iter()
        .find(|row| row.group.is_some())
        .ok_or_else(|| io::Error::other("folded task"))?;
    equal!(
        group.preview,
        "Review the graph and its labels · Completed",
        "verified status updates the recorded prompt"
    );
    check!(
        !group.group.as_ref().is_some_and(|group| group.live),
        "completed task is not live"
    );
    Ok(())
}
