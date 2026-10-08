//! Rank/select window reads and indexed literal Find.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

use editchain_index::{
    OrderedMap,
    rank::{Axis, Measure, RankTree},
};
use history_geometry::live::{GraphNode, Order, RowGeometry};
use idle_history::{
    query::Filter,
    timeline::{
        Action, Cursor, Geometry, MAX_LIMIT, Match, Matches, Position, Progress, Response, Row,
        Transition, VERSION, View, Window,
    },
};
use serde::{Deserialize, Serialize};

use super::{
    facts,
    model::{Fact, Snapshot},
    project,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct ReadView {
    rank: RankTree<Order, String>,
}

/// Compact row data ordered beside its neighbors for bounded sequential reads.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Summary {
    row: Row,
    author: Option<String>,
    session: Option<String>,
}

pub(super) fn summary(snapshot: &Snapshot, node: &super::model::Node) -> Option<Summary> {
    let fact = snapshot.facts.get(&node.key)?;
    let mut row = fact.row.clone();
    if let Some(original) = fact
        .original
        .as_ref()
        .and_then(|id| project::exact(snapshot, id))
    {
        row.records.extend(original.row.address.record().cloned());
    }
    row.relationships.clone_from(&node.relationships);
    if row.kind == idle_history::query::ActivityKind::File && row.preview.is_empty() {
        let paths: BTreeSet<_> = snapshot
            .supporting
            .get(&fact.row.occurrence)
            .into_iter()
            .flatten()
            .filter_map(|id| snapshot.facts.get(id))
            .filter(|fact| !fact.is_observation())
            .map(|fact| fact.row.preview.as_str())
            .filter(|path| !path.is_empty())
            .collect();
        if paths.len() == 1
            && let Some(path) = paths.first()
        {
            row.preview = (*path).into();
        }
    }
    row.session = project::session(snapshot, fact)
        .cloned()
        .unwrap_or_default();
    Some(Summary {
        row,
        author: fact.author.clone(),
        session: project::session(snapshot, fact).cloned(),
    })
}

pub(super) fn execute(
    snapshot: &Snapshot,
    action: &Action,
    progress: Option<Progress>,
    cache: &mut OrderedMap<String, ReadView>,
) -> io::Result<Response> {
    match action {
        Action::Window {
            view,
            position,
            limit,
        } => window(snapshot, view, (position, *limit), progress, cache),
        Action::Find {
            view,
            text,
            cursor,
            limit,
        } => find(snapshot, view, text, (cursor.as_ref(), *limit), cache),
        Action::Members {
            revision,
            group,
            offset,
            limit,
        } => members(snapshot, revision, group, *offset, *limit),
        Action::Advance { .. } | Action::Cancel { .. } => {
            Err(io::Error::other("Expected an Activity read."))
        }
    }
}

fn validate(limit: u32) -> io::Result<usize> {
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Activity windows must contain between 1 and 500 rows.",
        ));
    }
    usize::try_from(limit).map_err(io::Error::other)
}

fn digest(value: &impl Serialize) -> io::Result<String> {
    Ok(
        blake3::hash(&serde_json::to_vec(value).map_err(io::Error::other)?)
            .to_hex()
            .to_string(),
    )
}

fn in_scope(snapshot: &Snapshot, fact: &Fact, filter: &Filter) -> bool {
    (filter.kinds.is_empty() || filter.kinds.contains(&fact.row.kind))
        && filter
            .author
            .as_ref()
            .is_none_or(|author| Some(author) == fact.author.as_ref())
        && filter
            .session
            .as_ref()
            .is_none_or(|session| Some(session) == project::session(snapshot, fact))
        && filter
            .recorder
            .as_ref()
            .is_none_or(|recorder| *recorder == fact.recorder)
        && filter
            .path
            .as_ref()
            .is_none_or(|path| Some(path) == fact.path.as_ref())
}

pub(super) fn scope_keys(snapshot: &Snapshot, fact: &Fact) -> Vec<String> {
    let mut keys = vec![
        format!("kind:{:?}", fact.row.kind),
        format!("recorder:{}", fact.recorder),
    ];
    keys.extend(fact.author.iter().map(|value| format!("author:{value}")));
    keys.extend(project::session(snapshot, fact).map(|value| format!("session:{value}")));
    keys.extend(fact.path.iter().map(|value| format!("path:{value}")));
    keys
}

pub(super) fn index_order(snapshot: &mut Snapshot, node: &super::model::Node, remove: bool) {
    if remove {
        drop(snapshot.summaries.remove(&node.order()));
    } else if let Some(summary) = summary(snapshot, node) {
        drop(snapshot.summaries.insert(node.order(), summary));
    }
    let Some(fact) = snapshot.facts.get(&node.key) else {
        return;
    };
    if fact.row.kind == idle_history::query::ActivityKind::Turn
        && let Some(task) = &fact.task
    {
        let states = snapshot.tasks.entry(task.clone()).or_default();
        if remove {
            let _removed = states.remove(&node.order());
        } else {
            let _old = states.insert(node.order(), fact.terminal);
        }
    }
    for key in scope_keys(snapshot, fact) {
        let rank = snapshot.scopes.entry(key).or_default();
        if remove {
            drop(rank.remove(&node.order()));
        } else {
            drop(rank.insert(
                node.order(),
                node.key.clone(),
                Measure {
                    expanded: 1,
                    visible: 1,
                },
            ));
        }
    }
}

pub(super) fn set_visible(snapshot: &mut Snapshot, id: &str, visible: bool) {
    let Some(node) = snapshot.nodes.get(id) else {
        return;
    };
    let order = node.order();
    let measure = Measure {
        expanded: 1,
        visible: u64::from(visible),
    };
    drop(snapshot.order.insert(order.clone(), id.to_owned(), measure));
    if let Some(fact) = snapshot.facts.get(id) {
        for key in scope_keys(snapshot, fact) {
            if let Some(rank) = snapshot.scopes.get_mut(&key) {
                drop(rank.insert(order.clone(), id.to_owned(), measure));
            }
        }
    }
}

fn view_rank(snapshot: &Snapshot, view: &View) -> RankTree<Order, String> {
    let mut rank = if view.filter == Filter::default() {
        snapshot.order.clone()
    } else {
        let mut scopes = Vec::new();
        if view.filter.kinds.len() == 1
            && let Some(kind) = view.filter.kinds.first()
        {
            scopes.push(format!("kind:{kind:?}"));
        }
        scopes.extend(
            view.filter
                .author
                .iter()
                .map(|value| format!("author:{value}")),
        );
        scopes.extend(
            view.filter
                .session
                .iter()
                .map(|value| format!("session:{value}")),
        );
        scopes.extend(
            view.filter
                .recorder
                .iter()
                .map(|value| format!("recorder:{value}")),
        );
        scopes.extend(view.filter.path.iter().map(|value| format!("path:{value}")));
        let empty = RankTree::default();
        let source = scopes
            .iter()
            .map(|key| snapshot.scopes.get(key).unwrap_or(&empty))
            .min_by_key(|rank| rank.measure().expanded)
            .unwrap_or(&snapshot.order);
        let direct = scopes.len() == 1 && view.filter.kinds.len() <= 1;
        let mut rank = if direct {
            source.clone()
        } else {
            RankTree::default()
        };
        for (order, id) in source.iter().take(if direct { 0 } else { usize::MAX }) {
            if snapshot
                .facts
                .get(id)
                .is_some_and(|fact| in_scope(snapshot, fact, &view.filter))
            {
                let visible = snapshot
                    .membership
                    .get(id)
                    .and_then(|group| snapshot.group_details.get(group))
                    .is_none_or(|group| group.expanded || group.exit == *id);
                drop(rank.insert(
                    order.clone(),
                    id.clone(),
                    Measure {
                        expanded: 1,
                        visible: u64::from(visible),
                    },
                ));
            }
        }
        rank
    };
    for disclosure in &view.disclosures {
        if let Some(members) = snapshot.groups.get(&disclosure.group) {
            for (index, id) in members.iter().enumerate() {
                if let Some(node) = snapshot.nodes.get(id)
                    && rank.get(&node.order()).is_some()
                {
                    drop(rank.insert(
                        node.order(),
                        id.clone(),
                        Measure {
                            expanded: 1,
                            visible: u64::from(disclosure.expanded || index == 0),
                        },
                    ));
                }
            }
        }
    }
    rank
}

pub(super) fn window(
    snapshot: &Snapshot,
    view: &View,
    request: (&Position, u32),
    progress: Option<Progress>,
    cache: &mut OrderedMap<String, ReadView>,
) -> io::Result<Response> {
    let (position, limit) = request;
    let limit = validate(limit)?;
    if view.disclosures.len() > 2_000 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Too many Activity disclosure choices.",
        ));
    }
    let mut view = view.clone();
    if let Position::Seek(id) = position
        && let Some(group) = snapshot.membership.get(id)
    {
        view.disclosures.retain(|choice| choice.group != *group);
        view.disclosures.push(idle_history::timeline::Disclosure {
            group: group.clone(),
            expanded: true,
        });
    }
    view.disclosures
        .sort_by(|left, right| left.group.cmp(&right.group));
    let view_id = digest(&view)?;
    if let Position::Page(cursor) = position
        && (cursor.revision != snapshot.revision || cursor.view != view_id)
    {
        return Ok(Response::Stale);
    }
    if !cache.contains_key(&view_id) {
        trim(cache);
        drop(cache.insert(
            view_id.clone(),
            ReadView {
                rank: view_rank(snapshot, &view),
            },
        ));
    }
    let rank = &cache
        .get(&view_id)
        .ok_or_else(|| io::Error::other("Activity view is unavailable."))?
        .rank;
    let offset = match position {
        Position::Latest | Position::Refresh(None) => 0,
        Position::Page(cursor) => cursor.offset,
        Position::Seek(id) | Position::Refresh(Some(id)) => snapshot
            .nodes
            .get(id)
            .and_then(|node| rank.rank(&node.order()))
            .map_or(0, |position| {
                position
                    .visible
                    .saturating_sub(u64::try_from(limit / 2).unwrap_or(0))
            }),
    }
    .min(rank.measure().visible.saturating_sub(1));
    let mut ids = Vec::new();
    for index in 0..limit {
        let Some((order, id, _)) = rank.select(
            offset.saturating_add(u64::try_from(index).unwrap_or(u64::MAX)),
            Axis::Visible,
        ) else {
            break;
        };
        ids.push((order.clone(), id.clone()));
    }
    let rows = rows(snapshot, &ids, &view);
    let after = offset.saturating_add(u64::try_from(rows.len()).unwrap_or(u64::MAX));
    let cursor = |offset| Cursor {
        revision: snapshot.revision.clone(),
        view: view_id.clone(),
        offset,
    };
    Ok(Response::Window(Window {
        version: VERSION,
        revision: snapshot.revision.clone(),
        rows,
        newer: (offset > 0)
            .then(|| cursor(offset.saturating_sub(u64::try_from(limit).unwrap_or(u64::MAX)))),
        older: (after < rank.measure().visible).then(|| cursor(after)),
        offset,
        activities: rank.measure().expanded,
        visible_rows: rank.measure().visible,
        max_lane: lane(snapshot.graph.max_lane()),
        gaps: snapshot.gaps.iter().cloned().collect(),
        rebuilding: progress,
    }))
}

fn lane(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn rows(snapshot: &Snapshot, ids: &[(Order, String)], view: &View) -> Vec<Row> {
    let entries: BTreeSet<_> = ids
        .iter()
        .filter_map(|(_, id)| {
            snapshot
                .membership
                .get(id)
                .and_then(|group| snapshot.group_details.get(group))
                .map(|group| group.entry.clone())
        })
        .collect();
    let mut boundaries: Vec<_> = ids
        .iter()
        .filter_map(|(order, id)| {
            if let Some(summary) = snapshot.summaries.get(order) {
                let parents = summary
                    .row
                    .relationships
                    .iter()
                    .filter(|relation| relation.unresolved.is_none())
                    .filter_map(|relation| relation.parent.clone())
                    .filter(|parent| parent != id)
                    .collect::<BTreeSet<_>>();
                Some((order.clone(), parents.into_iter().collect()))
            } else {
                snapshot
                    .nodes
                    .get(id)
                    .map(|node| (node.order(), node.parents.clone()))
            }
        })
        .collect();
    boundaries.extend(entries.iter().filter_map(|id| {
        snapshot
            .nodes
            .get(id)
            .map(|node| (node.order(), node.parents.clone()))
    }));
    let graphs = snapshot.graph.decorate_bounds(&boundaries);
    ids.iter()
        .filter_map(|(order, id)| row(snapshot, order, id, view, &graphs))
        .collect()
}

fn row(
    snapshot: &Snapshot,
    order: &Order,
    id: &str,
    view: &View,
    graphs: &BTreeMap<String, RowGeometry>,
) -> Option<Row> {
    let fallback;
    let summary = if let Some(summary) = snapshot.summaries.get(order) {
        summary
    } else {
        fallback = summary(snapshot, snapshot.nodes.get(id)?)?;
        &fallback
    };
    let mut row = summary.row.clone();
    for (identity, output) in [
        (summary.author.as_ref(), &mut row.author),
        (summary.session.as_ref(), &mut row.session),
    ] {
        if let Some(label) = identity
            .and_then(|key| snapshot.labels.get(key))
            .and_then(|labels| labels.last_key_value())
            .map(|(_, label)| label)
            && !label.is_empty()
        {
            output.clone_from(label);
        }
    }
    let mut graph = graphs.get(id)?.clone();
    if let Some(group) = snapshot
        .membership
        .get(id)
        .and_then(|key| snapshot.group_details.get(key))
    {
        let mut group = group.clone();
        group.header = group.exit == id;
        group.expanded = view
            .disclosures
            .iter()
            .find(|choice| choice.group == group.id)
            .map_or(group.expanded, |choice| choice.expanded);
        if group.header && !group.expanded {
            if !group.summary.is_empty() {
                row.preview.clone_from(&group.summary);
                row.tags.clear();
            }
            for member in snapshot
                .groups
                .get(&group.id)
                .into_iter()
                .flatten()
                .filter_map(|id| snapshot.facts.get(id))
            {
                row.records.extend(member.row.address.record().cloned());
                if let Some(original) = member
                    .original
                    .as_ref()
                    .and_then(|id| project::exact(snapshot, id))
                {
                    row.records.extend(original.row.address.record().cloned());
                }
            }
            let entry = graphs.get(&group.entry)?;
            // Safe interiors are straight paths. Keep the header's own rails:
            // another task can start or end between this group's members.
            graph.parents.clone_from(&entry.parents);
        }
        row.group = Some(group);
    }
    row.records.sort();
    row.records.dedup();
    row.graph = Geometry {
        lane: lane(graph.lane),
        above: graph.above.into_iter().map(lane).collect(),
        below: graph.below.into_iter().map(lane).collect(),
        transitions: graph
            .transitions
            .into_iter()
            .map(|(from, to)| Transition(lane(from), lane(to)))
            .collect(),
        muted_above: graph.muted_above.into_iter().map(lane).collect(),
        muted_below: graph.muted_below.into_iter().map(lane).collect(),
        muted_transitions: graph
            .muted_transitions
            .into_iter()
            .map(|(from, to)| Transition(lane(from), lane(to)))
            .collect(),
        parents: graph.parents,
        clipped_below: view.filter != Filter::default(),
    };
    Some(row)
}

fn members(
    snapshot: &Snapshot,
    revision: &str,
    group: &str,
    offset: u64,
    limit: u32,
) -> io::Result<Response> {
    let limit = validate(limit)?;
    if revision != snapshot.revision {
        return Ok(Response::Stale);
    }
    let Some(members) = snapshot.groups.get(group) else {
        return Ok(Response::Stale);
    };
    let view = View {
        disclosures: vec![idle_history::timeline::Disclosure {
            group: group.into(),
            expanded: true,
        }],
        ..View::default()
    };
    let ids = members
        .iter()
        .skip(usize::try_from(offset).unwrap_or(usize::MAX))
        .take(limit)
        .filter_map(|id| {
            snapshot
                .nodes
                .get(id)
                .map(|node| (node.order(), id.clone()))
        })
        .collect::<Vec<_>>();
    let rows = rows(snapshot, &ids, &view);
    Ok(Response::Window(Window {
        version: VERSION,
        revision: revision.into(),
        rows,
        newer: None,
        older: None,
        offset,
        activities: u64::try_from(members.len()).unwrap_or(u64::MAX),
        visible_rows: u64::try_from(members.len()).unwrap_or(u64::MAX),
        max_lane: lane(snapshot.graph.max_lane()),
        gaps: Vec::new(),
        rebuilding: None,
    }))
}

fn find(
    snapshot: &Snapshot,
    view: &View,
    text: &str,
    request: (Option<&Cursor>, u32),
    cache: &mut OrderedMap<String, ReadView>,
) -> io::Result<Response> {
    let (cursor, limit) = request;
    let limit = validate(limit)?;
    if text.is_empty() || text.len() > 4_096 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Find requires between 1 and 4096 bytes of text.",
        ));
    }
    let text = text.to_lowercase();
    let view_id = digest(&(&view.filter, &text))?;
    if cursor.is_some_and(|cursor| cursor.revision != snapshot.revision || cursor.view != view_id) {
        return Ok(Response::Stale);
    }
    if !cache.contains_key(&view_id) {
        trim(cache);
        drop(cache.insert(
            view_id.clone(),
            ReadView {
                rank: search(snapshot, view, &text),
            },
        ));
    }
    let rank = &cache
        .get(&view_id)
        .ok_or_else(|| io::Error::other("Find results are unavailable."))?
        .rank;
    let offset = cursor.map_or(0, |cursor| cursor.offset);
    let total = rank.measure().visible;
    let matches: Vec<_> = (0..limit)
        .filter_map(|index| {
            rank.select(
                offset.saturating_add(u64::try_from(index).unwrap_or(u64::MAX)),
                Axis::Visible,
            )
        })
        .filter_map(|(_, id, _)| snapshot.facts.get(id))
        .map(|fact| Match {
            occurrence: fact.row.occurrence.clone(),
            group: snapshot.membership.get(&fact.row.occurrence).cloned(),
            address: fact.row.address.clone(),
            preview: fact.row.preview.clone(),
        })
        .collect();
    let next = offset.saturating_add(u64::try_from(matches.len()).unwrap_or(u64::MAX));
    Ok(Response::Found(Matches {
        revision: snapshot.revision.clone(),
        matches,
        total,
        next: (next < total).then(|| Cursor {
            revision: snapshot.revision.clone(),
            view: view_id,
            offset: next,
        }),
        unavailable: snapshot.unavailable,
    }))
}

fn search(snapshot: &Snapshot, view: &View, text: &str) -> RankTree<Order, String> {
    let mut rank = RankTree::default();
    let words = facts::grams(text);
    if words.iter().all(|word| snapshot.words.contains_key(word))
        && let Some(postings) = words
            .iter()
            .filter_map(|word| snapshot.words.get(word))
            .min_by_key(|values| values.len())
    {
        for id in postings.iter() {
            if snapshot
                .text
                .get(id)
                .is_none_or(|content| !content.contains(text))
            {
                continue;
            }
            for occurrence in project::endpoints(snapshot, id, "") {
                if let Some(node) = snapshot.nodes.get(&occurrence)
                    && snapshot
                        .facts
                        .get(&occurrence)
                        .is_some_and(|fact| in_scope(snapshot, fact, &view.filter))
                {
                    drop(rank.insert(
                        node.order(),
                        occurrence,
                        Measure {
                            expanded: 1,
                            visible: 1,
                        },
                    ));
                }
            }
        }
    }
    rank
}

fn trim(cache: &mut OrderedMap<String, ReadView>) {
    while cache.len() >= 8 {
        if let Some(old) = cache.first_key_value().map(|(key, _)| key.clone()) {
            drop(cache.remove(&old));
        }
    }
}
