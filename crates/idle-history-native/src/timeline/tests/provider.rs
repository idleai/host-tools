//! Provider source summaries survive conversion and anchor child boundaries.

use std::io;

use editchain_core::{
    ActorId, Clock, ImportOp, NodeId, NoteOp, NoteRelationship, Op, OpKind, ParentSet, Payload,
    ScopeRef, SessionId, SourceId, Tags,
};
use editchain_engine::Engine;
use idle_history::provider::{
    CodexLifecycleEvent, CodexLifecycleEvidence, CodexSourceEvidence, CodexSpawnSignal,
    CodexThreadId, ProviderEvidence, ProviderEvidenceSchema, ProviderFact,
};
use idle_history::timeline::{RelationshipKind, Source};

use super::{binding, id, latest};
use crate::timeline::model::key;

fn source(node: u64, sequence: u64) -> SourceId {
    SourceId::new(NodeId(node), 1, sequence.saturating_mul(65_536))
}

fn raw(node: u64, sequence: u64, terminal: bool) -> Op {
    let source = source(node, sequence);
    let bytes = format!(
        "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"{}\"}}}}\n",
        if terminal {
            "task_complete"
        } else {
            "recorded_activity"
        }
    )
    .into_bytes();
    Op {
        id: source.id(),
        source: Some(source),
        parents: if sequence > 1 {
            ParentSet::One(super::provider::source(node, sequence.saturating_sub(1)).id())
        } else {
            ParentSet::None
        },
        actor: ActorId(node),
        clock: Clock::UnixMs(sequence.saturating_mul(100)),
        scope: ScopeRef::Session(SessionId(node)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_hash: Some(*blake3::hash(&bytes).as_bytes()),
            raw_ref: Payload::Inline(bytes),
        }),
    }
}

fn metadata(value: u64, raw: &Op, fact: ProviderFact) -> io::Result<Op> {
    let OpKind::Import(import) = &raw.kind else {
        return Err(io::Error::other("raw fixture"));
    };
    let contract = ProviderEvidence {
        schema: ProviderEvidenceSchema::V1,
        source: raw.source.ok_or_else(|| io::Error::other("raw source"))?,
        raw_hash: import
            .raw_hash
            .ok_or_else(|| io::Error::other("raw hash"))?,
        fact,
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

fn extent(node: u64, last: u64, parent: Option<&str>) -> ProviderFact {
    ProviderFact::CodexSource(Box::new(CodexSourceEvidence {
        thread: CodexThreadId(format!("thread-{node}")),
        parent: parent.map(|value| CodexThreadId(value.into())),
        forked_from: None,
        agent_path: None,
        first: source(node, 1),
        last: source(node, last),
        prefix_hash: [0; 32],
    }))
}

fn corpus() -> io::Result<Vec<Op>> {
    let p1 = raw(1, 1, false);
    let p2 = raw(1, 2, false);
    let p3 = raw(1, 3, false);
    let c1 = raw(2, 1, false);
    let c2 = raw(2, 2, true);
    Ok(vec![
        p1,
        p2.clone(),
        p3.clone(),
        c1,
        c2.clone(),
        metadata(100, &p3, extent(1, 3, None))?,
        metadata(101, &c2, extent(2, 2, Some("thread-1")))?,
        metadata(
            102,
            &p2,
            ProviderFact::CodexLifecycle(CodexLifecycleEvidence {
                thread: CodexThreadId("thread-1".into()),
                item_id: "spawn".into(),
                turn_id: "turn".into(),
                event: CodexLifecycleEvent::Spawn {
                    activation: source(1, 2),
                    child: CodexThreadId("thread-2".into()),
                    agent_path: None,
                    signal: CodexSpawnSignal::CollabTool,
                },
            }),
        )?,
        metadata(
            103,
            &p3,
            ProviderFact::CodexLifecycle(CodexLifecycleEvidence {
                thread: CodexThreadId("thread-1".into()),
                item_id: "wait".into(),
                turn_id: "turn".into(),
                event: CodexLifecycleEvent::Completed {
                    child: CodexThreadId("thread-2".into()),
                },
            }),
        )?,
    ])
}

#[test]
fn a_forked_thread_without_a_divergence_record_remains_unresolved() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let child = raw(2, 1, false);
    let mut fact = extent(2, 1, None);
    if let ProviderFact::CodexSource(meta) = &mut fact {
        meta.forked_from = Some(CodexThreadId("recorded-base-thread".into()));
    }
    let summary = metadata(100, &child, fact)?;
    for op in idle_history_import::activity::convert(
        &[child, summary],
        &mut idle_history_import::MemoryBlobSink::default(),
    )
    .map_err(io::Error::other)?
    {
        let _admitted = engine.append(&op)?;
    }
    let window = latest(&binding(directory.path()))?;
    check!(
        window.rows.iter().any(|row| row
            .relationships
            .iter()
            .any(|relation| relation.kind == RelationshipKind::Fork
                && relation.parent.is_none()
                && relation.unresolved.is_some())),
        "recorded fork identity cannot supply an unrecorded divergence endpoint"
    );
    Ok(())
}

#[test]
fn fresh_conversion_resolves_spawn_join_and_keeps_the_terminal_after_resume() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let operations = corpus()?;
    let converted = idle_history_import::activity::convert(
        &operations,
        &mut idle_history_import::MemoryBlobSink::default(),
    )
    .map_err(io::Error::other)?;
    for op in &converted {
        let _admitted = engine.append(op)?;
    }
    let binding = binding(directory.path());
    let first = latest(&binding)?;
    let child_start = key(
        Source::Current,
        editchain_core::activity::upgrade_id(source(2, 1).id()),
    );
    let child_end = key(
        Source::Current,
        editchain_core::activity::upgrade_id(source(2, 2).id()),
    );
    let completion = key(
        Source::Current,
        editchain_core::activity::upgrade_id(source(1, 3).id()),
    );
    check!(
        first
            .rows
            .iter()
            .find(|row| row.occurrence == child_start)
            .is_some_and(|row| row
                .relationships
                .iter()
                .any(|relation| relation.kind == RelationshipKind::Spawn
                    && relation.parent.is_some())),
        "preserved source summaries establish the exact child start: {first:#?}"
    );
    check!(
        first
            .rows
            .iter()
            .find(|row| row.occurrence == completion)
            .is_some_and(|row| row
                .relationships
                .iter()
                .any(|relation| relation.kind == RelationshipKind::Completion
                    && relation.parent.as_ref() == Some(&child_end))),
        "completion retains the child's recorded terminal"
    );
    let resumed = raw(2, 3, false);
    let extra = [
        resumed.clone(),
        metadata(104, &resumed, extent(2, 3, Some("thread-1")))?,
    ];
    for op in idle_history_import::activity::convert(
        &extra,
        &mut idle_history_import::MemoryBlobSink::default(),
    )
    .map_err(io::Error::other)?
    {
        let _admitted = engine.append(&op)?;
    }
    let updated = latest(&binding)?;
    check!(
        updated
            .rows
            .iter()
            .find(|row| row.occurrence == completion)
            .is_some_and(|row| row
                .relationships
                .iter()
                .any(|relation| relation.kind == RelationshipKind::Completion
                    && relation.parent.as_ref() == Some(&child_end))),
        "a later resumed child cannot move an earlier join"
    );
    Ok(())
}

#[test]
fn verified_spawn_crosses_hidden_setup_and_does_not_add_an_ambiguous_fork() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let mut operations = corpus()?;
    for operation in &mut operations {
        if let OpKind::Note(note) = &mut operation.kind {
            let Payload::Inline(content) = &mut note.content else {
                continue;
            };
            let mut contract: ProviderEvidence =
                serde_json::from_slice(content).map_err(io::Error::other)?;
            if let ProviderFact::CodexSource(meta) = &mut contract.fact
                && meta.parent.is_some()
            {
                meta.forked_from.clone_from(&meta.parent);
                *content = serde_json::to_vec(&contract).map_err(io::Error::other)?;
            }
        }
    }
    let mut marker = raw(2, 1, false);
    marker.id = id(200);
    marker.source = None;
    marker.parents = ParentSet::One(source(2, 1).id());
    marker.tags = Tags::META | Tags::NOTE;
    marker.kind = OpKind::Note(NoteOp {
        target_ids: vec![source(2, 1).id()],
        relationship: NoteRelationship::Explains,
        content: Payload::Inline(b"vscode.editor.observation.v1".to_vec()),
    });
    operations.push(marker);
    for operation in operations {
        let _admitted = engine.append(&operation)?;
    }
    let window = latest(&binding(directory.path()))?;
    let child = window
        .rows
        .iter()
        .find(|row| {
            row.address
                .record()
                .is_some_and(|address| address.record.operation == source(2, 2).id().to_string())
        })
        .ok_or_else(|| io::Error::other("first displayed child"))?;
    check!(
        child
            .relationships
            .iter()
            .any(|relation| relation.kind == RelationshipKind::Spawn
                && relation.parent.as_ref() == Some(&key(Source::Current, source(1, 2).id()))),
        "spawn survives hidden setup"
    );
    check!(
        !child
            .relationships
            .iter()
            .any(|relation| relation.kind == RelationshipKind::Fork),
        "an exact spawn does not imply an additional unresolved fork"
    );
    Ok(())
}

#[test]
fn retained_replay_and_archive_repair_keep_the_same_exact_attachments() -> io::Result<()> {
    use editchain_core::activity::Operation;
    use idle_history::timeline::Window;
    use std::collections::{BTreeMap, BTreeSet};

    let operations = corpus()?;
    let converted = idle_history_import::activity::convert(
        &operations,
        &mut idle_history_import::MemoryBlobSink::default(),
    )
    .map_err(io::Error::other)?;
    let aliases: BTreeMap<_, _> = converted
        .iter()
        .filter_map(|op| {
            let record = Operation::view(op)?;
            Some((op.id.to_string(), record.legacy?.operation.to_string()))
        })
        .collect();
    let signatures = |window: &Window| {
        window
            .rows
            .iter()
            .map(|row| {
                let operation = aliases
                    .get(
                        &row.address
                            .record()
                            .expect("record destination")
                            .record
                            .operation,
                    )
                    .unwrap_or(
                        &row.address
                            .record()
                            .expect("record destination")
                            .record
                            .operation,
                    )
                    .clone();
                let parents: BTreeSet<_> = row
                    .relationships
                    .iter()
                    .filter_map(|relation| {
                        let parent = relation.parent.as_ref()?.split_once(':')?.1;
                        Some((
                            format!("{:?}", relation.kind),
                            aliases
                                .get(parent)
                                .cloned()
                                .unwrap_or_else(|| parent.into()),
                        ))
                    })
                    .collect();
                (operation, parents)
            })
            .collect::<BTreeMap<_, _>>()
    };
    let fresh = tempfile::tempdir()?;
    let engine = Engine::open(fresh.path())?;
    for operation in &converted {
        let _admitted = engine.append(operation)?;
    }
    let expected = signatures(&latest(&binding(fresh.path()))?);
    let archive = tempfile::tempdir()?;
    let old = Engine::open(archive.path())?;
    for operation in &operations {
        let _admitted = old.append(operation)?;
    }
    let empty = tempfile::tempdir()?;
    let _empty = Engine::open(empty.path())?;
    let mut retained = binding(empty.path());
    retained.retained_directory = Some(archive.path().to_owned());
    let replay = latest(&retained)?;
    equal!(
        signatures(&replay),
        expected,
        "retained replay has the same spawn and completion attachments"
    );
    check!(
        replay.rows.iter().all(
            |row| row.address.record().expect("record destination").source == Source::Retained
        ),
        "retained actions keep their exact source"
    );
    let repaired = tempfile::tempdir()?;
    let engine = Engine::open(repaired.path())?;
    for operation in &converted {
        if !matches!(
            Operation::view(operation).map(|record| record.kind),
            Some(editchain_core::activity::Kind::Link(_))
        ) {
            let _admitted = engine.append(operation)?;
        }
    }
    let mut bound = binding(repaired.path());
    bound.retained_directory = Some(archive.path().to_owned());
    let recovery = latest(&bound)?;
    equal!(
        signatures(&recovery),
        expected,
        "bound archive metadata repairs earlier conversions without duplicate rows"
    );
    check!(
        recovery
            .rows
            .iter()
            .all(|row| row.address.record().expect("record destination").source == Source::Current),
        "converted rows remain the primary exact addresses"
    );
    Ok(())
}

#[test]
fn missing_prefix_and_ambiguous_activations_stay_unresolved_until_exact_records_arrive()
-> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let operations = corpus()?;
    let converted = idle_history_import::activity::convert(
        &operations,
        &mut idle_history_import::MemoryBlobSink::default(),
    )
    .map_err(io::Error::other)?;
    let missing = editchain_core::activity::upgrade_id(source(2, 1).id());
    for operation in converted.iter().filter(|operation| operation.id != missing) {
        let _admitted = engine.append(operation)?;
    }
    let bound = binding(directory.path());
    let first = latest(&bound)?;
    check!(
        first
            .rows
            .iter()
            .flat_map(|row| &row.relationships)
            .any(|relation| relation.kind == RelationshipKind::Completion
                && relation.unresolved.is_some()),
        "an incomplete source prefix cannot establish a completion"
    );
    let operation = converted
        .iter()
        .find(|operation| operation.id == missing)
        .ok_or_else(|| io::Error::other("missing fixture source"))?;
    let _admitted = engine.append(operation)?;
    let repaired = latest(&bound)?;
    check!(
        repaired
            .rows
            .iter()
            .flat_map(|row| &row.relationships)
            .any(|relation| relation.kind == RelationshipKind::Completion
                && relation.parent.is_some()),
        "a late raw record repairs the exact prefix and attachment"
    );
    let other = metadata(
        105,
        &raw(1, 1, false),
        ProviderFact::CodexLifecycle(CodexLifecycleEvidence {
            thread: CodexThreadId("thread-1".into()),
            item_id: "other-spawn".into(),
            turn_id: "other-turn".into(),
            event: CodexLifecycleEvent::Spawn {
                activation: source(1, 1),
                child: CodexThreadId("thread-2".into()),
                agent_path: None,
                signal: CodexSpawnSignal::CollabTool,
            },
        }),
    )?;
    for operation in idle_history_import::activity::convert(
        &[other],
        &mut idle_history_import::MemoryBlobSink::default(),
    )
    .map_err(io::Error::other)?
    {
        let _admitted = engine.append(&operation)?;
    }
    let ambiguous = latest(&bound)?;
    check!(
        ambiguous
            .rows
            .iter()
            .flat_map(|row| &row.relationships)
            .filter(|relation| relation.kind == RelationshipKind::Spawn)
            .all(|relation| relation.parent.is_none()),
        "different activation records cannot be guessed from names or time"
    );
    check!(
        ambiguous
            .rows
            .iter()
            .flat_map(|row| &row.relationships)
            .filter(|relation| relation.kind == RelationshipKind::Completion)
            .all(|relation| relation.parent.is_none()),
        "ambiguous activations cannot establish a child execution boundary for completion"
    );
    Ok(())
}
