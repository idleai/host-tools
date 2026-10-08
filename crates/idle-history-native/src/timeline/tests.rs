//! Structural fixtures exercise native windows independently of any renderer.

macro_rules! check {
    ($condition:expr, $message:expr $(,)?) => {
        if !$condition {
            return Err(std::io::Error::other($message));
        }
    };
}

macro_rules! equal {
    ($left:expr, $right:expr, $message:expr $(,)?) => {
        if let (left, right) = (&$left, &$right)
            && left != right
        {
            return Err(std::io::Error::other(format!(
                "{}: {left:?} != {right:?}",
                $message
            )));
        }
    };
}

mod checkpoint;
mod folded_routes;
mod git;
mod legacy;
mod provider;
mod records;
mod routes;
mod tasks;

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

use editchain_core::{
    Op, OpId, Payload,
    activity::{ContentUpdate, ItemId, Kind, Message, MessageKind, Operation, Stage, UpdateMode},
};
use editchain_engine::Engine;
use idle_history::{
    binding::RepositoryChainBinding,
    timeline::{Action, Position, Request, Response, Row, VERSION, View, Window},
};

use super::{execute, model::key};
use crate::history::service::Binding;

fn id(value: u64) -> OpId {
    OpId::from_bytes(*blake3::hash(&value.to_le_bytes()).as_bytes())
}

fn message(value: u64, session: u64, time: u64, parents: &[u64]) -> io::Result<Op> {
    let mut record = Operation::new(
        id(value),
        ItemId(id(value)),
        ItemId(id(session)),
        Kind::Message(Message {
            category: MessageKind::Text,
            stage: Stage::Snapshot,
            audience: Vec::new(),
            blocks: vec![ContentUpdate {
                block: ItemId(id(value)),
                position: Some(0),
                mode: UpdateMode::Replace,
                previous: None,
                media_type: Payload::Empty,
                content: Payload::Inline(
                    format!("Activity {value} in execution {session}").into_bytes(),
                ),
            }],
            coverage: None,
            outcome: None,
        }),
    );
    record.session = Some(ItemId(id(session)));
    record.time_ms = Some(time);
    record.parents = parents.iter().map(|value| id(*value)).collect();
    record.into_op().map_err(io::Error::other)
}

fn binding(directory: &std::path::Path) -> Binding {
    Binding {
        repository: RepositoryChainBinding {
            workspace_id: "fixture".into(),
            repository_id: "repository".into(),
            chain: "chain".into(),
        },
        chain_directory: directory.to_owned(),
        retained_directory: None,
        repository_directory: None,
    }
}

fn request(binding: &Binding, action: Action) -> io::Result<Response> {
    execute(
        binding,
        &Request {
            version: VERSION,
            action,
        },
    )
}

fn latest(binding: &Binding) -> io::Result<Window> {
    let mut response = request(
        binding,
        Action::Window {
            view: View::default(),
            position: Position::Latest,
            limit: 200,
        },
    )?;
    for _ in 0..1_000 {
        match response {
            Response::Building(progress) => {
                response = request(
                    binding,
                    Action::Advance {
                        build: progress.build,
                    },
                )?;
            }
            Response::Window(window) => {
                if let Some(progress) = window.rebuilding {
                    response = request(
                        binding,
                        Action::Advance {
                            build: progress.build,
                        },
                    )?;
                } else {
                    return Ok(window);
                }
            }
            Response::Found(_) | Response::Cancelled | Response::Stale => {
                return Err(io::Error::other("unexpected fixture response"));
            }
        }
    }
    Err(io::Error::other("fixture index did not finish"))
}

fn row(window: &Window, value: u64) -> io::Result<&Row> {
    window
        .rows
        .iter()
        .find(|row| {
            row.address
                .record()
                .expect("record destination")
                .record
                .operation
                == id(value).to_string()
        })
        .ok_or_else(|| io::Error::other(format!("missing activity {value}")))
}

#[test]
fn ten_siblings_nested_three_levels_join_and_reopen_keep_complete_routes() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let _admitted = engine.append(&message(1, 100, 5_000, &[])?)?;
    let _admitted = engine.append(&message(2, 100, 5_001, &[1])?)?;
    let mut parents = vec![2];
    for branch in 0u64..10 {
        let start = 10u64.saturating_add(branch);
        let end = 30u64.saturating_add(branch);
        let _admitted = engine.append(&message(
            start,
            200u64.saturating_add(branch),
            5_010u64.saturating_add(branch),
            &[1],
        )?)?;
        let _admitted = engine.append(&message(
            end,
            200u64.saturating_add(branch),
            8_000u64.saturating_add(branch),
            &[start],
        )?)?;
        parents.push(end);
    }
    let _admitted = engine.append(&message(50, 300, 6_000, &[10])?)?;
    let _admitted = engine.append(&message(51, 301, 6_100, &[50])?)?;
    let _admitted = engine.append(&message(52, 302, 6_200, &[51])?)?;
    let _admitted = engine.append(&message(60, 100, 9_000, &parents)?)?;
    let binding = binding(directory.path());
    let window = latest(&binding)?;
    equal!(
        row(&window, 60)?.graph.parents.len(),
        11,
        "all child terminals and the parent continuation survive"
    );
    let lanes: BTreeSet<_> = (10u64..20)
        .map(|value| row(&window, value).map(|row| row.graph.lane))
        .collect::<io::Result<_>>()?;
    equal!(lanes.len(), 10, "concurrent siblings occupy distinct rails");
    check!(
        !lanes.contains(&row(&window, 1)?.graph.lane),
        "the parent rail remains separate"
    );
    for pair in window.rows.windows(2) {
        if let [newer, older] = pair {
            equal!(
                newer.graph.below,
                older.graph.above,
                "adjacent boundaries agree"
            );
        }
    }
    let reopened = latest(&binding)?;
    equal!(
        window,
        reopened,
        "checkpoint reads preserve routing and order"
    );
    let before: BTreeMap<_, _> = window
        .rows
        .iter()
        .map(|row| (row.occurrence.clone(), row.graph.lane))
        .collect();
    let _admitted = engine.append(&message(61, 100, 9_001, &[60])?)?;
    let appended = latest(&binding)?;
    for row in &appended.rows {
        if let Some(lane) = before.get(&row.occurrence) {
            equal!(*lane, row.graph.lane, "ordinary appends preserve lanes");
        }
    }
    Ok(())
}

#[test]
fn occurrence_order_search_and_bidirectional_pages_do_not_depend_on_hash_order() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    for value in (1u64..=240).rev() {
        let parents = value
            .checked_sub(1)
            .filter(|value| *value > 0)
            .into_iter()
            .collect::<Vec<_>>();
        let _admitted = engine.append(&message(value, 500, value, &parents)?)?;
    }
    let binding = binding(directory.path());
    let window = latest(&binding)?;
    equal!(window.activities, 240, "projected activity count is exact");
    equal!(
        window.rows.first().map(|row| &row
            .address
            .record()
            .expect("record destination")
            .record
            .operation),
        Some(&id(240).to_string()),
        "latest is chronological, independent of hashed addresses"
    );
    let cursor = window.older.ok_or_else(|| io::Error::other("older page"))?;
    let Response::Window(older) = request(
        &binding,
        Action::Window {
            view: View::default(),
            position: Position::Page(cursor),
            limit: 200,
        },
    )?
    else {
        return Err(io::Error::other("older window"));
    };
    equal!(
        older.rows.len(),
        40,
        "the next bounded page has the remaining rows"
    );
    check!(older.newer.is_some(), "navigation works in both directions");
    let Response::Found(found) = request(
        &binding,
        Action::Find {
            view: View::default(),
            text: "Activity 101 in".into(),
            cursor: None,
            limit: 10,
        },
    )?
    else {
        return Err(io::Error::other("search page"));
    };
    equal!(
        found.total,
        1,
        "indexed literal Find finds the exact occurrence"
    );
    let occurrence = key(idle_history::timeline::Source::Current, id(101));
    let Response::Window(seek) = request(
        &binding,
        Action::Window {
            view: View::default(),
            position: Position::Seek(occurrence.clone()),
            limit: 20,
        },
    )?
    else {
        return Err(io::Error::other("seek window"));
    };
    check!(
        seek.rows.iter().any(|row| row.occurrence == occurrence),
        "seek centers the actual occurrence"
    );
    Ok(())
}

#[test]
fn cancellation_preserves_the_previous_complete_snapshot() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let _admitted = engine.append(&message(1, 1_000, 1, &[])?)?;
    let binding = binding(directory.path());
    let old = latest(&binding)?;
    let _admitted = engine.append(&message(2, 1_000, 2, &[1])?)?;
    let Response::Window(rebuilding) = request(
        &binding,
        Action::Window {
            view: View::default(),
            position: Position::Latest,
            limit: 20,
        },
    )?
    else {
        return Err(io::Error::other("previous window"));
    };
    equal!(
        old.revision,
        rebuilding.revision,
        "the old coherent snapshot remains readable"
    );
    let progress = rebuilding
        .rebuilding
        .ok_or_else(|| io::Error::other("build progress"))?;
    equal!(
        request(
            &binding,
            Action::Cancel {
                build: progress.build
            }
        )?,
        Response::Cancelled,
        "construction can stop between bounded batches"
    );
    let updated = latest(&binding)?;
    equal!(
        updated.activities,
        2,
        "cancelled derived work is safely rebuilt"
    );
    Ok(())
}

#[test]
fn logical_item_updates_do_not_manufacture_cycles() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let _admitted = engine.append(&message(1, 700, 10, &[])?)?;
    let _admitted = engine.append(&message(2, 700, 9, &[1])?)?;
    let mut third = message(3, 700, 8, &[2])?;
    if let editchain_core::OpKind::Activity(operation) = &mut third.kind {
        operation.item = ItemId(id(1));
    }
    let _admitted = engine.append(&third)?;
    let window = latest(&binding(directory.path()))?;
    equal!(
        window.rows.len(),
        3,
        "A1, B and A2 remain distinct occurrences"
    );
    equal!(
        row(&window, 1)?.item,
        row(&window, 3)?.item,
        "logical continuity is retained"
    );
    equal!(
        window.rows.first().map(|row| row
            .address
            .record()
            .expect("record destination")
            .record
            .operation
            .clone()),
        Some(id(3).to_string()),
        "causal order wins over skewed wall clocks"
    );
    check!(window.gaps.is_empty(), "no false cycle is introduced");
    equal!(
        row(&window, 3)?.timestamp,
        Some(8),
        "recorded timestamps are unchanged"
    );
    Ok(())
}

#[test]
fn completed_groups_fold_safe_interiors_and_split_on_late_boundaries() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    for value in 1u64..=9 {
        let mut op = message(
            value,
            800,
            value,
            &value
                .checked_sub(1)
                .filter(|value| *value > 0)
                .into_iter()
                .collect::<Vec<_>>(),
        )?;
        if let editchain_core::OpKind::Activity(operation) = &mut op.kind {
            operation.turn = Some(ItemId(id(900)));
        }
        let _admitted = engine.append(&op)?;
    }
    let mut finished = Operation::new(
        id(10),
        ItemId(id(900)),
        ItemId(id(800)),
        Kind::Turn(editchain_core::activity::Turn {
            action: editchain_core::activity::TurnAction::Finished,
            triggers: Vec::new(),
            attempt: ItemId(id(901)),
            outcome: Some(editchain_core::activity::Completion {
                status: editchain_core::activity::Status::Success,
                detail: Payload::Empty,
            }),
        }),
    );
    finished.time_ms = Some(10);
    finished.turn = Some(finished.item);
    finished.parents = vec![id(9)];
    finished.session = Some(ItemId(id(800)));
    let _admitted = engine.append(&finished.into_op().map_err(io::Error::other)?)?;
    let binding = binding(directory.path());
    let folded = latest(&binding)?;
    let group = folded
        .rows
        .iter()
        .find_map(|row| row.group.clone())
        .ok_or_else(|| io::Error::other("safe group"))?;
    check!(
        !group.live && !group.expanded,
        "recorded completion supplies the folded default"
    );
    equal!(
        group.count,
        8,
        "root and protected completion stay separate"
    );
    equal!(
        folded.activities,
        10,
        "folding does not change activity counts"
    );
    let view = View {
        disclosures: vec![idle_history::timeline::Disclosure {
            group: group.id.clone(),
            expanded: true,
        }],
        ..View::default()
    };
    let Response::Window(expanded) = request(
        &binding,
        Action::Window {
            view,
            position: Position::Latest,
            limit: 200,
        },
    )?
    else {
        return Err(io::Error::other("expanded window"));
    };
    equal!(
        expanded.rows.len(),
        10,
        "disclosure reveals every exact occurrence"
    );
    let _admitted = engine.append(&message(99, 999, 20, &[5])?)?;
    let split = latest(&binding)?;
    check!(
        split.rows.iter().any(|row| row
            .address
            .record()
            .expect("record destination")
            .record
            .operation
            == id(5).to_string()),
        "late forks expose their exact parent boundary"
    );
    equal!(split.activities, 11, "split summaries keep exact counts");
    let unresolved = Operation::new(
        id(100),
        ItemId(id(100)),
        ItemId(id(800)),
        Kind::Link(editchain_core::activity::Link {
            from: editchain_core::activity::Entity::Operation(id(3)),
            relation: "LogicalParent".into(),
            to: vec![editchain_core::activity::Entity::Operation(id(1000))],
            content: Payload::Empty,
        }),
    );
    let _admitted = engine.append(&unresolved.into_op().map_err(io::Error::other)?)?;
    let incomplete = latest(&binding)?;
    let visible = row(&incomplete, 3)?;
    check!(
        visible.group.is_none()
            && visible
                .relationships
                .iter()
                .any(|relation| relation.unresolved.is_some()),
        "folding cannot hide an activity with an unresolved attachment"
    );
    Ok(())
}

#[test]
fn clipped_windows_include_rails_with_both_endpoints_outside() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let _admitted = engine.append(&message(1, 10_000, 1, &[])?)?;
    let _admitted = engine.append(&message(2, 20_000, 2, &[1])?)?;
    let _admitted = engine.append(&message(3, 20_000, 10_000, &[2])?)?;
    for value in 10u64..100 {
        let parent = if value == 10 {
            1
        } else {
            value.saturating_sub(1)
        };
        let _admitted = engine.append(&message(value, 10_000, value, &[parent])?)?;
    }
    let binding = binding(directory.path());
    let all = latest(&binding)?;
    let lane = row(&all, 3)?.graph.lane;
    let Response::Window(middle) = request(
        &binding,
        Action::Window {
            view: View::default(),
            position: Position::Seek(key(idle_history::timeline::Source::Current, id(50))),
            limit: 10,
        },
    )?
    else {
        return Err(io::Error::other("middle window"));
    };
    check!(
        !middle
            .rows
            .iter()
            .any(|row| [id(2).to_string(), id(3).to_string()].contains(
                &row.address
                    .record()
                    .expect("record destination")
                    .record
                    .operation
            )),
        "the long rail's endpoints are outside the window"
    );
    check!(
        middle
            .rows
            .iter()
            .all(|row| row.graph.above.contains(&lane) && row.graph.below.contains(&lane)),
        "indexed route coverage crosses the entire window"
    );
    Ok(())
}

#[test]
fn live_group_identity_survives_appends_and_tool_attempts_remain_separate() -> io::Result<()> {
    use editchain_core::activity::{OutputChannel, Tool};
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let tool = |value: u64, attempt: u64| -> io::Result<Op> {
        let mut record = Operation::new(
            id(value),
            ItemId(id(200)),
            ItemId(id(300)),
            Kind::Tool(Tool {
                native_call: Payload::Inline(b"call".to_vec()),
                name: Payload::Inline(b"review".to_vec()),
                stage: Stage::Updated,
                attempt: ItemId(id(attempt)),
                parent_call: None,
                arguments: Payload::Empty,
                channel: OutputChannel::Stdout,
                output: None,
                terminal: None,
                outcome: None,
            }),
        );
        record.turn = Some(ItemId(id(400)));
        record.session = Some(ItemId(id(500)));
        record.time_ms = Some(value);
        record.parents = value
            .checked_sub(1)
            .filter(|value| *value > 0)
            .map(id)
            .into_iter()
            .collect();
        record.into_op().map_err(io::Error::other)
    };
    for value in 1..=4 {
        let _admitted = engine.append(&tool(value, 600)?)?;
    }
    let bound = binding(directory.path());
    let before = latest(&bound)?;
    let first = row(&before, 4)?
        .group
        .clone()
        .ok_or_else(|| io::Error::other("live attempt group"))?;
    check!(first.live && first.expanded, "live work starts expanded");
    let _admitted = engine.append(&tool(5, 600)?)?;
    let appended = latest(&bound)?;
    let next = row(&appended, 5)?
        .group
        .clone()
        .ok_or_else(|| io::Error::other("extended group"))?;
    equal!(
        next.id,
        first.id,
        "ordinary progress extends the same entry-anchored group"
    );
    equal!(
        next.count,
        first.count.saturating_add(1),
        "the count grows by one occurrence"
    );
    for value in 6..=8 {
        let _admitted = engine.append(&tool(value, 601)?)?;
    }
    let retry = latest(&bound)?;
    let previous = row(&retry, 5)?
        .group
        .clone()
        .ok_or_else(|| io::Error::other("first attempt"))?;
    let repeated = row(&retry, 8)?
        .group
        .clone()
        .ok_or_else(|| io::Error::other("second attempt"))?;
    check!(
        previous.id != repeated.id,
        "independent attempts cannot share a folded summary"
    );
    equal!(
        previous.count,
        4,
        "the earlier attempt keeps its constituent occurrences"
    );
    equal!(repeated.count, 3, "the retry has its own count");
    Ok(())
}
