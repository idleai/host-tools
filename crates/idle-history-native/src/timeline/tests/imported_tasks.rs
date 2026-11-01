//! Live import conversion must keep completed tasks usable in Activity.

use super::*;
use editchain_core::{NoteOp, NoteRelationship, OpKind, ScopeRef, Tags, ToolStage, TurnId};
use idle_history::provider::CodexLogicalChange;
use idle_history::timeline::Disclosure;

fn transcript() -> io::Result<Vec<Op>> {
    let mut operations = Vec::new();
    for value in 1_u64..=10 {
        let turn = if value <= 5 { "first" } else { "second" };
        let position = value.saturating_sub(1) % 5;
        let original = legacy::raw(
            value,
            value.checked_sub(1).filter(|parent| *parent > 0),
            &serde_json::json!({"type":"event_msg","payload":{
                "type":if position == 4 { "task_complete" } else { "item_completed" },
                "turn_id":turn,
                "item":{"status":"completed","exit_code":0}
            }}),
        )?;
        let mut output = legacy::tool(100_u64.saturating_add(value), value);
        let source = output
            .source
            .ok_or_else(|| io::Error::other("output source"))?;
        let changes = if position == 4 {
            output.kind = OpKind::Note(NoteOp {
                target_ids: Vec::new(),
                relationship: NoteRelationship::Explains,
                content: Payload::Inline(format!("{turn}: completed (4 items)").into_bytes()),
            });
            output.scope = ScopeRef::Turn(TurnId(value));
            Vec::new()
        } else {
            if position == 0 {
                output.tags |= Tags::HUMAN;
                output.kind = OpKind::Message(editchain_core::MessageOp {
                    content: Payload::Inline(format!("Review {turn} task").into_bytes()),
                    content_type: Payload::Empty,
                });
            } else if let OpKind::Tool(tool) = &mut output.kind {
                tool.stage = ToolStage::Finish;
                tool.content = Payload::Inline(format!("Result {turn} {position}").into_bytes());
            }
            vec![CodexLogicalChange::Upsert {
                turn: turn.into(),
                item: value.to_string(),
                incarnation: legacy::source(value),
                outputs: vec![source],
            }]
        };
        let contract = legacy::derivation(
            200_u64.saturating_add(value),
            &original,
            changes,
            vec![source],
        )?;
        operations.extend([original, output, contract]);
    }
    Ok(operations)
}

#[test]
fn converted_tasks_fold_independently_and_find_reveals_exact_children() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let operations = idle_history_import::activity::convert(
        &transcript()?,
        &mut idle_history_import::MemoryBlobSink::default(),
    )
    .map_err(io::Error::other)?;
    for operation in &operations {
        let _admitted = engine.append(operation)?;
    }
    let bound = binding(directory.path());
    let window = latest(&bound)?;
    let groups: Vec<_> = window
        .rows
        .iter()
        .filter_map(|row| row.group.clone())
        .collect();
    equal!(
        groups.len(),
        2,
        "each converted task has its own disclosure"
    );
    check!(
        groups.iter().all(|group| !group.live && !group.expanded),
        "completed tasks start folded"
    );
    check!(
        groups
            .iter()
            .any(|group| group.summary == "Review first task · Completed"),
        "the first prompt supplies the task title"
    );
    for selected in &groups {
        let Response::Window(expanded) = request(
            &bound,
            Action::Window {
                view: View {
                    disclosures: vec![Disclosure {
                        group: selected.id.clone(),
                        expanded: true,
                    }],
                    ..View::default()
                },
                position: Position::Latest,
                limit: 200,
            },
        )?
        else {
            return Err(io::Error::other("expanded task"));
        };
        equal!(
            u64::try_from(expanded.rows.len()).map_err(io::Error::other)?,
            u64::try_from(window.rows.len())
                .map_err(io::Error::other)?
                .saturating_add(selected.count)
                .saturating_sub(1),
            "only the selected task opens"
        );
    }
    let Response::Found(found) = request(
        &bound,
        Action::Find {
            view: View::default(),
            text: "Result first 2".into(),
            cursor: None,
            limit: 10,
        },
    )?
    else {
        return Err(io::Error::other("Find result"));
    };
    equal!(
        found.total,
        1,
        "a folded child stays searchable exactly once"
    );
    equal!(
        latest(&bound)?,
        window,
        "reopening keeps task identity and state"
    );
    Ok(())
}
