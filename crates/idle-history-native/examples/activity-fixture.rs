//! Export real native timeline windows for browser and packaged editor checks.

use editchain_git as _;
use sha2 as _;
#[path = "activity-fixture/cases.rs"]
mod cases;

use editchain_core::{
    Op, OpId, Payload,
    activity::{
        ContentUpdate, ItemId, Kind, Message, MessageKind, Operation, Stage, Turn, TurnAction,
        UpdateMode,
    },
};
use editchain_engine::Engine;
use idle_history::{
    binding::RepositoryChainBinding,
    timeline::{Action, Disclosure, Position, Request, Response, VERSION, View, Window},
};
use idle_history_native::{history::service::Binding, timeline};
use std::{io, path::PathBuf};
use {
    editchain_index as _, idle_editor_capture as _, idle_history_graph as _,
    idle_history_import as _, idle_host_io as _, idle_protocol as _, idle_repository as _,
    serde as _, tempfile as _,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn id(value: u64) -> OpId {
    OpId::from_bytes(*blake3::hash(&value.to_le_bytes()).as_bytes())
}

fn activity(value: u64, session: u64, parents: &[u64], title: &str) -> Result<Op> {
    let kind = Kind::Message(Message {
        category: MessageKind::Text,
        stage: Stage::Finished,
        audience: Vec::new(),
        blocks: vec![ContentUpdate {
            block: ItemId(id(value)),
            position: Some(0),
            mode: UpdateMode::Replace,
            previous: None,
            media_type: Payload::Empty,
            content: Payload::Inline(title.as_bytes().to_vec()),
        }],
        coverage: None,
        outcome: None,
    });
    let mut op = Operation::new(id(value), ItemId(id(value)), ItemId(id(session)), kind);
    op.time_ms = Some(1_791_383_400_000u64.saturating_add(value.saturating_mul(1000)));
    op.session = Some(ItemId(id(session)));
    op.author = Some(ItemId(id(session)));
    op.parents = parents.iter().map(|value| id(*value)).collect();
    if session == 100 {
        op.turn = Some(ItemId(id(999_000)));
    }
    op.into_op().map_err(Into::into)
}

fn records(engine: &Engine) -> Result {
    let mut writer = engine.writer()?;
    let before = writer.store_blob(b"fn render() { old_history(); }\n")?.id;
    let after = writer
        .store_blob(b"fn render() { activity_timeline(); }\n")?
        .id;
    for session in 100u64..=113 {
        let mut op = Operation::new(
            id(session.saturating_add(10_000)),
            ItemId(id(session)),
            ItemId(id(session)),
            Kind::Author(editchain_core::activity::Author {
                label: Payload::Inline(if session == 100 {
                    b"ambientlight".to_vec()
                } else {
                    format!("Agent {}", session.saturating_sub(100)).into_bytes()
                }),
                role: if session == 100 {
                    editchain_core::activity::AuthorRole::Person
                } else {
                    editchain_core::activity::AuthorRole::Agent
                },
                native_role: Payload::Empty,
                metadata: Payload::Empty,
            }),
        );
        op.time_ms = Some(1);
        let _admitted = writer.append(&op.into_op()?)?;
    }
    let _admitted = writer.append(&activity(
        1,
        100,
        &[],
        "Review the Activity editor and native history actions",
    )?)?;
    for branch in 1u64..=10 {
        let start = branch.saturating_add(10);
        let _admitted = writer.append(&activity(
            start,
            100u64.saturating_add(branch),
            &[1],
            &format!("Agent {branch}: inspect recorded relationships"),
        )?)?;
    }
    for (value, session, parent, title) in [
        (30, 111, 11, "Nested agent: inspect terminal boundaries"),
        (31, 112, 30, "Nested review: exact source generations"),
        (32, 113, 31, "Third level: check resumed child sessions"),
    ] {
        let _admitted = writer.append(&activity(value, session, &[parent], title)?)?;
    }
    for value in 40u64..=58 {
        let mut record = activity(
            value,
            100,
            &[if value == 40 {
                1
            } else {
                value.saturating_sub(1)
            }],
            "Parent execution continues while child reviews run",
        )?;
        if value == 50
            && let editchain_core::OpKind::Activity(op) = &mut record.kind
        {
            op.kind = Kind::File(editchain_core::activity::File {
                action: editchain_core::activity::FileAction::Change,
                path: editchain_core::PathId(1),
                name: Payload::Inline(b"src/history/editor.rs".to_vec()),
                renamed_to: None,
                revision: Some(ItemId(id(value))),
                before: Some(before),
                after: Some(after),
                edit: editchain_core::FileEdit::None,
                text_edits: Vec::new(),
                change: None,
                caused_by: None,
                ranges: Vec::new(),
                text_ranges: Vec::new(),
                duration_ms: None,
            });
        }
        let _admitted = writer.append(&record)?;
    }
    let mut parents = vec![58];
    for branch in 1u64..=10 {
        let value = branch.saturating_add(60);
        let _admitted = writer.append(&activity(
            value,
            100u64.saturating_add(branch),
            &[branch.saturating_add(10)],
            &format!("Agent {branch}: review complete"),
        )?)?;
        parents.push(value);
    }
    let _admitted = writer.append(&activity(
        80,
        100,
        &parents,
        "Join all ten reviews and retain the parent continuation",
    )?)?;
    let mut end = Operation::new(
        id(90),
        ItemId(id(999_000)),
        ItemId(id(100)),
        Kind::Turn(Turn {
            action: TurnAction::Finished,
            triggers: Vec::new(),
            attempt: ItemId(id(999_001)),
            outcome: Some(editchain_core::activity::Completion {
                status: editchain_core::activity::Status::Success,
                detail: Payload::Empty,
            }),
        }),
    );
    end.turn = Some(end.item);
    end.time_ms = Some(1_791_383_490_000);
    end.parents = vec![id(80)];
    end.session = Some(ItemId(id(100)));
    let _admitted = writer.append(&end.into_op()?)?;
    // An independent long-lived source supplies enough rows for paging and Find.
    for value in 100u64..=650 {
        let parent = if value == 100 {
            90
        } else {
            value.saturating_sub(1)
        };
        let _admitted = writer.append(&activity(
            value,
            500,
            &[parent],
            &format!("Check src/history/editor.rs — activity {value}"),
        )?)?;
    }
    Ok(())
}

fn read(binding: &Binding, action: Action) -> Result<Window> {
    let mut response = timeline::execute(
        binding,
        &Request {
            version: VERSION,
            action,
        },
    )?;
    loop {
        let progress = match response {
            Response::Building(progress) => progress,
            Response::Window(window) => {
                if window.rebuilding.is_none() {
                    return Ok(window);
                }
                window.rebuilding.ok_or("build progress")?
            }
            Response::Found(_) | Response::Cancelled | Response::Stale => {
                return Err("Unexpected fixture response".into());
            }
        };
        response = timeline::execute(
            binding,
            &Request {
                version: VERSION,
                action: Action::Advance {
                    build: progress.build,
                },
            },
        )?;
    }
}

fn main() -> Result {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("Fixture output directory required")?,
    );
    let chain = root.join("chain");
    std::fs::create_dir_all(&root)?;
    let engine = Engine::open(&chain)?;
    records(&engine)?;
    let binding = Binding {
        repository: RepositoryChainBinding {
            workspace_id: "activity-fixture".into(),
            repository_id: "activity-fixture".into(),
            chain: "activity-fixture".into(),
        },
        chain_directory: chain,
        retained_directory: None,
        repository_directory: None,
    };
    let _latest = read(
        &binding,
        Action::Window {
            view: View::default(),
            position: Position::Latest,
            limit: 200,
        },
    )?;
    let focus = format!("current:{}", id(80));
    let topology = read(
        &binding,
        Action::Window {
            view: View::default(),
            position: Position::Seek(focus.clone()),
            limit: 200,
        },
    )?;
    let disclosures: Vec<_> = topology
        .rows
        .iter()
        .filter_map(|row| row.group.as_ref())
        .map(|group| Disclosure {
            group: group.id.clone(),
            expanded: true,
        })
        .collect();
    let expanded_view = View {
        disclosures,
        ..View::default()
    };
    let mut windows = vec![read(
        &binding,
        Action::Window {
            view: expanded_view.clone(),
            position: Position::Latest,
            limit: 500,
        },
    )?];
    while let Some(cursor) = windows.last().and_then(|window| window.older.clone()) {
        windows.push(read(
            &binding,
            Action::Window {
                view: expanded_view.clone(),
                position: Position::Page(cursor),
                limit: 500,
            },
        )?);
    }
    let value = serde_json::json!({"topology":topology,"windows":windows,"focus":focus,"binding":binding.repository});
    std::fs::write(
        root.join("timeline.json"),
        serde_json::to_vec(&value).map_err(io::Error::other)?,
    )?;
    cases::export(&root)?;
    Ok(())
}
