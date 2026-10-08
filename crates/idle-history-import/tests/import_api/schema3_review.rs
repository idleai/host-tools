//! Regression cases for conflict isolation and preserving recorded provider facts.

use super::schema3_regressions::{loaded, saved};
use super::*;
use editchain_core::activity::{Entity, Kind, MessageKind, Operation, Status};
use editchain_core::{
    ActorId, Clock, ImportOp, MessageOp, NoteOp, OpId, ParentSet, ScopeRef, Tags, ToolOp, ToolStage,
};
use idle_history_import::{ImportReport, MemoryBlobSink};

fn legacy(sequence: u8, kind: OpKind) -> Op {
    Op {
        id: OpId::from_bytes([sequence; 32]),
        source: None,
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::None,
        scope: ScopeRef::None,
        tags: Tags::NONE,
        kind,
    }
}

fn converted(ops: &[Op]) -> Result<Vec<Vec<Op>>> {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("old");
    let destination = temp.path().join("new");
    saved(&source, ops)?;
    let _report = idle_history_import::activity::migrate(&source, &destination, || false)?;
    let batch = ImportBatch::capture(&MemoryCursorStore::new(), |sink, _cursors| {
        for op in ops {
            let _admission = sink.accept_op(op)?;
        }
        Ok(ImportReport::default())
    })?
    .into_schema3(&mut MemoryBlobSink::default())?;
    Ok(vec![
        loaded(&destination)?,
        batch.operations().to_vec(),
        idle_history_import::activity::convert(ops, &mut MemoryBlobSink::default())?,
    ])
}

fn activity(ops: &[Op], old: OpId) -> Result<Operation> {
    ops.iter()
        .filter_map(Operation::view)
        .find(|record| {
            record
                .legacy
                .as_ref()
                .is_some_and(|legacy| legacy.operation == old)
        })
        .ok_or_else(|| format!("missing converted record for {old}").into())
}

#[test]
fn conflicting_tool_metadata_never_enters_an_accepted_finished_record() -> Result {
    let original = legacy(
        1,
        OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(b"{}".to_vec()),
            raw_hash: None,
        }),
    );
    let mut first = legacy(
        2,
        OpKind::Tool(ToolOp {
            tool_call_id: Payload::Inline(b"call-1".to_vec()),
            tool_name: Payload::Inline(b"Read".to_vec()),
            stage: ToolStage::Start,
            content: Payload::Inline(b"arguments-a".to_vec()),
        }),
    );
    first.parents = ParentSet::One(original.id);
    let mut second = first.clone();
    if let OpKind::Tool(tool) = &mut second.kind {
        tool.content = Payload::Inline(b"arguments-b".to_vec());
    }
    let mut finish = first.clone();
    finish.id = OpId::from_bytes([3; 32]);
    if let OpKind::Tool(tool) = &mut finish.kind {
        tool.stage = ToolStage::Finish;
        tool.tool_name = Payload::Empty;
        tool.content = Payload::Inline(b"result".to_vec());
    }
    let forward = converted(&[
        original.clone(),
        first.clone(),
        second.clone(),
        finish.clone(),
    ])?;
    let reverse = converted(&[original, second.clone(), first.clone(), finish.clone()])?;
    for (left, right) in forward.iter().zip(&reverse) {
        verify!(
            left.contains(&first) && left.contains(&second),
            "both conflict variants survive"
        );
        verify!(
            right.contains(&first) && right.contains(&second),
            "reverse order retains both variants"
        );
        let record = activity(left, finish.id)?;
        verify_eq!(
            record,
            activity(right, finish.id)?,
            "accepted completion is independent of conflict order"
        );
        let Kind::Tool(tool) = record.kind else {
            return Err("expected Tool".into());
        };
        verify_eq!(
            tool.arguments,
            Payload::Empty,
            "conflicted arguments are unavailable"
        );
        verify_eq!(
            tool.name,
            Payload::Empty,
            "conflicted names are unavailable"
        );
    }
    Ok(())
}

#[test]
fn explicit_relations_keep_their_targets_content_and_old_addresses() -> Result {
    let parent = legacy(
        1,
        OpKind::Message(MessageOp {
            content: Payload::Empty,
            content_type: Payload::Empty,
        }),
    );
    let target = legacy(2, parent.kind.clone());
    for relationship in [
        NoteRelationship::OccurrenceOf,
        NoteRelationship::Contains,
        NoteRelationship::ToolResultOf,
    ] {
        let mut note = legacy(
            3,
            OpKind::Note(NoteOp {
                target_ids: vec![target.id],
                relationship,
                content: Payload::Inline(b"recorded annotation".to_vec()),
            }),
        );
        note.parents = ParentSet::One(parent.id);
        for records in converted(&[parent.clone(), target.clone(), note.clone()])? {
            let record = activity(&records, note.id)?;
            let Kind::Link(link) = record.kind else {
                return Err("expected explicit Link".into());
            };
            verify_eq!(
                link.relation,
                format!("{relationship:?}"),
                "relation kind survives"
            );
            verify_eq!(
                link.content,
                Payload::Inline(b"recorded annotation".to_vec()),
                "relation annotation survives"
            );
            verify_eq!(
                link.from,
                Entity::Operation(activity(&records, parent.id)?.id),
                "relation source survives"
            );
            verify_eq!(
                link.to,
                vec![Entity::Operation(activity(&records, target.id)?.id)],
                "relation target survives"
            );
            verify!(
                record
                    .legacy
                    .ok_or("missing legacy mapping")?
                    .folded
                    .is_empty(),
                "relation is not folded into another record"
            );
        }
    }
    Ok(())
}

#[test]
fn conflicting_originals_cannot_supply_completion_status_or_fold_children() -> Result {
    let first = legacy(
        1,
        OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(
                br#"{"type":"event_msg","payload":{"status":"success"}}"#.to_vec(),
            ),
            raw_hash: None,
        }),
    );
    let mut second = first.clone();
    second.kind = OpKind::Import(ImportOp {
        raw_ref: Payload::Inline(br#"{"type":"event_msg","payload":{"status":"failed"}}"#.to_vec()),
        raw_hash: None,
    });
    let mut finish = legacy(
        2,
        OpKind::Tool(ToolOp {
            tool_call_id: Payload::Inline(b"call-1".to_vec()),
            tool_name: Payload::Empty,
            stage: ToolStage::Finish,
            content: Payload::Inline(b"result".to_vec()),
        }),
    );
    finish.parents = ParentSet::One(first.id);
    let forward = converted(&[first.clone(), second.clone(), finish.clone()])?;
    let reverse = converted(&[second.clone(), first.clone(), finish.clone()])?;
    for (left, right) in forward.iter().zip(&reverse) {
        verify!(
            left.contains(&first) && left.contains(&second),
            "both original variants survive"
        );
        let record = activity(left, finish.id)?;
        verify_eq!(
            record,
            activity(right, finish.id)?,
            "conflicted source order cannot change the accepted child"
        );
        verify_eq!(
            record.parents,
            vec![first.id],
            "conflicted original retains its old address"
        );
        let Kind::Tool(tool) = record.kind else {
            return Err("expected Tool".into());
        };
        verify_eq!(
            tool.outcome.ok_or("missing completion")?.status,
            Status::Unknown,
            "conflicted status is not inferred"
        );
    }
    Ok(())
}

#[test]
fn claude_reasoning_and_parallel_tool_outcomes_keep_the_recorded_meaning() -> Result {
    let temp = tempfile::tempdir()?;
    let input = [
        serde_json::json!({"type":"assistant", "uuid":"assistant", "sessionId":"session", "cwd":"/workspace", "message":{"role":"assistant", "content":[
            {"type":"thinking","thinking":"Private reasoning"}, {"type":"text","text":"Public answer"},
            {"type":"tool_use","id":"failed-call","name":"Read","input":{}},
            {"type":"tool_use","id":"successful-call","name":"Read","input":{}},
            {"type":"tool_use","id":"unknown-call","name":"Read","input":{}}]}}),
        serde_json::json!({"type":"user", "uuid":"results", "sessionId":"session", "cwd":"/workspace", "message":{"role":"user", "content":[
            {"type":"tool_result","tool_use_id":"failed-call","is_error":true,"content":"File not found"},
            {"type":"tool_result","tool_use_id":"successful-call","is_error":false,"content":"File contents"},
            {"type":"tool_result","tool_use_id":"unknown-call","content":"Unknown outcome"}]}}),
    ];
    let text = input
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(temp.path().join("session.jsonl"), text)?;
    let batch = Provider::Claude.capture(
        temp.path(),
        &ImportOptions {
            include_thinking: true,
            ..ImportOptions::default()
        },
        &mut MemoryBlobSink::default(),
        &MemoryCursorStore::new(),
    )?;
    for records in converted(batch.operations())? {
        let activities: Vec<_> = records.iter().filter_map(Operation::view).collect();
        let messages: Vec<_> = activities
            .iter()
            .filter_map(|record| {
                if let Kind::Message(message) = &record.kind {
                    Some(message)
                } else {
                    None
                }
            })
            .collect();
        verify!(
            messages
                .iter()
                .any(|message| message.category == MessageKind::Reasoning
                    && message.blocks.iter().any(
                        |block| block.content == Payload::Inline(b"Private reasoning".to_vec())
                    )),
            "thinking remains Reasoning"
        );
        verify!(
            messages
                .iter()
                .any(|message| message.category == MessageKind::Text
                    && message
                        .blocks
                        .iter()
                        .any(|block| block.content == Payload::Inline(b"Public answer".to_vec()))),
            "public answer remains Text"
        );
        for (call, status) in [
            ("failed-call", Status::Failure),
            ("successful-call", Status::Success),
            ("unknown-call", Status::Unknown),
        ] {
            let tool = activities
                .iter()
                .find_map(|record| {
                    if let Kind::Tool(tool) = &record.kind {
                        (tool.native_call == Payload::Inline(call.as_bytes().to_vec())
                            && tool.outcome.is_some())
                        .then_some(tool)
                    } else {
                        None
                    }
                })
                .ok_or("missing finished call")?;
            verify_eq!(
                tool.outcome.as_ref().ok_or("missing outcome")?.status,
                status,
                "each call retains its own recorded outcome"
            );
        }
    }
    Ok(())
}

#[test]
fn source_prefix_contract_survives_capture_migration_and_retry() -> Result {
    use idle_history::provider::{
        CodexSourceEvidence, CodexThreadId, ProviderEvidenceSchema, ProviderFact,
    };
    let source = editchain_core::SourceId::new(editchain_core::NodeId(7), 2, 65_536);
    let bytes = b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"thread-a\"}}\n".to_vec();
    let raw_hash = *blake3::hash(&bytes).as_bytes();
    let mut raw = legacy(
        1,
        OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(bytes),
            raw_hash: Some(raw_hash),
        }),
    );
    raw.id = source.id();
    raw.source = Some(source);
    let contract = ProviderEvidence {
        schema: ProviderEvidenceSchema::V1,
        source,
        raw_hash,
        fact: ProviderFact::CodexSource(Box::new(CodexSourceEvidence {
            thread: CodexThreadId("thread-a".into()),
            parent: Some(CodexThreadId("parent-thread".into())),
            forked_from: Some(CodexThreadId("fork-thread".into())),
            agent_path: Some("root/review".into()),
            first: source,
            last: source,
            prefix_hash: [9; 32],
        })),
    };
    let payload = serde_json::to_vec(&contract)?;
    let mut metadata = legacy(
        2,
        OpKind::Note(NoteOp {
            target_ids: Vec::new(),
            relationship: NoteRelationship::ProviderEvidence,
            content: Payload::Inline(payload.clone()),
        }),
    );
    metadata.parents = ParentSet::One(raw.id);
    let variants = converted(&[raw.clone(), metadata.clone()])?;
    verify_eq!(
        variants.get(1),
        variants.get(2),
        "batch and direct capture retain byte-identical operations"
    );
    for records in variants {
        let summary = activity(&records, metadata.id)?;
        let Kind::Link(link) = &summary.kind else {
            return Err("source summary must remain a typed metadata link".into());
        };
        verify_eq!(
            &link.content,
            &Payload::Inline(payload.clone()),
            "all generation, extent, parent, fork, and path fields survive"
        );
        verify_eq!(
            link.from,
            Entity::Operation(activity(&records, raw.id)?.id),
            "the source summary names its converted exact raw occurrence"
        );
        verify_ne!(
            summary.id,
            editchain_core::activity::upgrade_id(metadata.id),
            "new summary records use a separate immutable namespace"
        );
        let retried =
            idle_history_import::activity::convert(&records, &mut MemoryBlobSink::default())?;
        verify_eq!(
            records,
            retried,
            "retrying converted records cannot change IDs or encoded fields"
        );
        let mut kinds: Vec<_> = records
            .iter()
            .filter_map(Operation::view)
            .map(|record| format!("{:?}", record.kind.name()))
            .collect();
        kinds.sort();
        verify_eq!(
            kinds,
            vec!["Link", "Original", "Session"],
            "the source summary supplements the existing Original and session snapshot"
        );
    }
    Ok(())
}
