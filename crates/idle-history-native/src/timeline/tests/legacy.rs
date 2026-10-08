//! Historical source records retain their readable activity and shared stream.

use std::io;

use editchain_core::{
    ActorId, Clock, ImportOp, NodeId, NoteOp, NoteRelationship, Op, OpKind, ParentSet, Payload,
    ScopeRef, SessionId, SourceId, Tags, ToolOp, ToolStage, TurnId,
};
use editchain_engine::Engine;
use idle_history::provider::{
    CodexDerivationContract, CodexDerivationEvidence, CodexLogicalChange, CodexThreadId,
    ProviderEvidence, ProviderEvidenceSchema, ProviderFact,
};

use super::{binding, id, latest};

pub(super) fn source(value: u64) -> SourceId {
    SourceId::new(NodeId(10), 1, value.saturating_mul(65_536))
}

pub(super) fn raw(value: u64, parent: Option<u64>, content: &serde_json::Value) -> io::Result<Op> {
    let bytes = serde_json::to_vec(content).map_err(io::Error::other)?;
    Ok(Op {
        id: source(value).id(),
        source: Some(source(value)),
        parents: parent.map_or(ParentSet::None, |value| ParentSet::One(source(value).id())),
        actor: ActorId(10),
        clock: Clock::UnixMs(value.saturating_mul(100)),
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_hash: Some(*blake3::hash(&bytes).as_bytes()),
            raw_ref: Payload::Inline(bytes),
        }),
    })
}

pub(super) fn tool(value: u64, parent: u64) -> Op {
    let output = SourceId::new(NodeId(value), 1, source(parent).seq.saturating_add(1));
    Op {
        id: output.id(),
        source: Some(output),
        parents: ParentSet::One(source(parent).id()),
        actor: ActorId(10),
        clock: Clock::UnixMs(parent.saturating_mul(100)),
        scope: ScopeRef::Turn(TurnId(20)),
        tags: Tags::IMPORT | Tags::TOOL,
        kind: OpKind::Tool(ToolOp {
            tool_call_id: Payload::Inline(format!("call-{value}").into_bytes()),
            tool_name: Payload::Inline(b"exec_command".to_vec()),
            stage: ToolStage::Start,
            content: Payload::Inline(br#"{"cmd":"cargo test","workdir":"/project"}"#.to_vec()),
        }),
    }
}

pub(super) fn derivation(
    value: u64,
    raw: &Op,
    changes: Vec<CodexLogicalChange>,
    outputs: Vec<SourceId>,
) -> io::Result<Op> {
    let OpKind::Import(import) = &raw.kind else {
        return Err(io::Error::other("raw fixture"));
    };
    let contract = ProviderEvidence {
        schema: ProviderEvidenceSchema::V1,
        source: raw
            .source
            .ok_or_else(|| io::Error::other("fixture source"))?,
        raw_hash: import
            .raw_hash
            .ok_or_else(|| io::Error::other("fixture hash"))?,
        fact: ProviderFact::CodexDerivation(CodexDerivationEvidence {
            thread: CodexThreadId("thread".into()),
            contract: CodexDerivationContract::OccurrencesV2,
            includes_thinking: false,
            outputs,
            changes,
        }),
    };
    Ok(Op {
        id: id(value),
        source: None,
        parents: ParentSet::One(raw.id),
        actor: raw.actor,
        clock: raw.clock,
        scope: raw.scope,
        tags: Tags::IMPORT,
        kind: OpKind::Note(NoteOp {
            target_ids: Vec::new(),
            relationship: NoteRelationship::ProviderEvidence,
            content: Payload::Inline(serde_json::to_vec(&contract).map_err(io::Error::other)?),
        }),
    })
}

#[test]
fn recorded_revisions_update_one_stable_activity_and_honor_turn_removal() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let binding = binding(directory.path());
    let mut first_key = None;
    for value in 1_u64..=3 {
        let raw = raw(
            value,
            value.checked_sub(1).filter(|value| *value != 0),
            &serde_json::json!({"type":"event_msg","payload":{"type":"item_completed"}}),
        )?;
        let tool = tool(100_u64.saturating_add(value), value);
        let output = tool
            .source
            .ok_or_else(|| io::Error::other("output source"))?;
        let change = CodexLogicalChange::Upsert {
            turn: "turn".into(),
            item: if value == 3 { "second" } else { "first" }.into(),
            incarnation: source(if value == 3 { 3 } else { 1 }),
            outputs: vec![output],
        };
        let _admitted = engine.append(&raw)?;
        let _admitted = engine.append(&tool)?;
        let _admitted = engine.append(&derivation(
            200_u64.saturating_add(value),
            &raw,
            vec![change],
            vec![output],
        )?)?;
        let window = latest(&binding)?;
        equal!(
            window.activities,
            if value == 3 { 2 } else { 1 },
            "logical updates retain one row per item"
        );
        equal!(
            window.max_lane,
            0,
            "recorded revisions retain one source lane"
        );
        let current = window
            .rows
            .iter()
            .find(|row| {
                row.address
                    .record()
                    .expect("record destination")
                    .record
                    .operation
                    == tool.id.to_string()
            })
            .ok_or_else(|| io::Error::other("latest exact tool address"))?;
        equal!(
            current.timestamp,
            Some(value.saturating_mul(100)),
            "the latest record time is displayed without moving its first appearance"
        );
        if value == 1 {
            first_key = Some(current.occurrence.clone());
        }
        if value == 2 {
            equal!(
                Some(&current.occurrence),
                first_key.as_ref(),
                "the first incarnation fixes row identity"
            );
        }
        if value == 3 {
            equal!(
                current.graph.parents.first(),
                first_key.as_ref(),
                "past physical revisions resolve to the current activity"
            );
        }
    }
    let removed = raw(4, Some(3), &serde_json::json!({"type":"turn_removed"}))?;
    let _admitted = engine.append(&removed)?;
    let _admitted = engine.append(&derivation(
        204,
        &removed,
        vec![CodexLogicalChange::RemoveTurn {
            turn: "turn".into(),
        }],
        Vec::new(),
    )?)?;
    equal!(
        latest(&binding)?.activities,
        0,
        "explicit turn removal retires current items"
    );
    Ok(())
}

#[test]
fn normalized_tools_share_the_raw_session_and_skip_editor_markers() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    for value in [1_u64, 2] {
        let _admitted = engine.append(&raw(
            value,
            value.checked_sub(1).filter(|value| *value != 0),
            &serde_json::json!({"type":"response_item","payload":{
                "type":"function_call", "name":"exec_command",
                "arguments":"{\"cmd\":\"cargo test\"}"
            }}),
        )?)?;
        let _admitted = engine.append(&tool(value.saturating_add(100), value))?;
    }
    let observation = raw(3, Some(2), &serde_json::json!({"source":"vscode.editor"}))?;
    let _admitted = engine.append(&observation)?;
    let _admitted = engine.append(&Op {
        id: id(103),
        source: None,
        parents: ParentSet::One(observation.id),
        actor: observation.actor,
        clock: observation.clock,
        scope: observation.scope,
        tags: Tags::META | Tags::NOTE,
        kind: OpKind::Note(NoteOp {
            target_ids: vec![observation.id],
            relationship: NoteRelationship::Explains,
            content: Payload::Inline(b"vscode.editor.observation.v1".to_vec()),
        }),
    })?;
    let window = latest(&binding(directory.path()))?;
    equal!(window.activities, 2, "one row per recorded tool action");
    equal!(
        window.max_lane,
        0,
        "tool recorders are not independent sessions"
    );
    check!(
        window.rows.iter().all(|row| row.preview == "cargo test"),
        "commands render their recorded arguments without JSON envelopes"
    );
    check!(
        window.rows.iter().all(|row| row.title == "run"),
        "the activity column describes execution"
    );
    Ok(())
}

#[test]
fn childless_item_notifications_stay_hidden_and_recorded_exec_failures_stay_visible()
-> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    for value in 1_u64..=6 {
        let content = if value == 6 {
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"Reasoning","summary_text":["Private plan"]}}})
        } else if value == 3 {
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","output":[{"type":"input_text","text":"Script failed\nWall time 0.5 seconds\nOutput:\nfailed command"}]}})
        } else {
            serde_json::json!({"type":"response_item","payload":{"type":"message","content":"Script failed\nWall time 0.5 seconds\nOutput:\nquoted text"}})
        };
        let original = raw(
            value,
            value.checked_sub(1).filter(|value| *value != 0),
            &content,
        )?;
        let activity = tool(100_u64.saturating_add(value), value);
        let outputs = if value == 6 {
            Vec::new()
        } else {
            vec![
                activity
                    .source
                    .ok_or_else(|| io::Error::other("fixture source"))?,
            ]
        };
        let _admitted = engine.append(&original)?;
        if value != 6 {
            let _admitted = engine.append(&activity)?;
        }
        let _admitted = engine.append(&derivation(
            200_u64.saturating_add(value),
            &original,
            vec![CodexLogicalChange::Upsert {
                turn: "turn".into(),
                item: value.to_string(),
                incarnation: source(value),
                outputs: outputs.clone(),
            }],
            outputs,
        )?)?;
    }
    let window = latest(&binding(directory.path()))?;
    equal!(
        window.activities,
        5,
        "a lifecycle notification without selected content is not an activity"
    );
    let failed = window
        .rows
        .iter()
        .find(|row| {
            row.address
                .record()
                .is_some_and(|address| address.record.operation == tool(103, 3).id.to_string())
        })
        .ok_or_else(|| io::Error::other("explicit failed command"))?;
    check!(
        failed.group.is_none() && failed.tags.iter().any(|tag| tag == "failed"),
        "canonical failure stays outside collapsed groups"
    );
    check!(
        window
            .rows
            .iter()
            .any(|row| row.group.as_ref().is_some_and(|group| group.count == 2)),
        "quoted failure text does not prevent normal folding"
    );
    Ok(())
}
