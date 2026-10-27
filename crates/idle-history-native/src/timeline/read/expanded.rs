//! Weighted physical blocks let inline details cross native page boundaries.

use std::{collections::BTreeMap, io};

use editchain_index::rank::{Axis, Measure, RankTree};
use idle_history::timeline::{Row, View};
use idle_history_graph::live::{GraphNode, Order};

use super::{ReadView, Snapshot};
use crate::timeline::details;

pub(super) fn view(
    snapshot: &Snapshot,
    view: &View,
    mut rank: RankTree<Order, String>,
) -> io::Result<ReadView> {
    let mut children = BTreeMap::new();
    let mut bytes = 0_usize;
    for id in &view.details {
        let Some(node) = snapshot.nodes.get(id) else {
            continue;
        };
        let Some(position) = rank.rank(&node.order()) else {
            continue;
        };
        if rank
            .select(position.visible, Axis::Visible)
            .is_none_or(|(_, current, _)| current != id)
            || folded(snapshot, view, id)
        {
            continue;
        }
        let Some(summary) = snapshot.summaries.get(&node.order()) else {
            continue;
        };
        let rows = details::load(snapshot, &summary.row)?;
        bytes = bytes.saturating_add(serde_json::to_vec(&rows).map_err(io::Error::other)?.len());
        if bytes > idle_history::timeline::MAX_CACHED_BYTES / 2 {
            return Err(io::Error::other(
                "Open Activity details exceed the read cache limit. Close other item details before opening this item.",
            ));
        }
        drop(rank.insert(
            node.order(),
            id.clone(),
            Measure {
                expanded: 1,
                visible: 1_u64.saturating_add(u64::try_from(rows.len()).unwrap_or(u64::MAX)),
            },
        ));
        drop(children.insert(id.clone(), rows));
    }
    Ok(ReadView { rank, children })
}

pub(super) fn folded(snapshot: &Snapshot, view: &View, id: &str) -> bool {
    snapshot
        .membership
        .get(id)
        .and_then(|group| snapshot.group_details.get(group))
        .is_some_and(|group| {
            group.exit == id
                && !view
                    .disclosures
                    .iter()
                    .find(|choice| choice.group == group.id)
                    .map_or(group.expanded, |choice| choice.expanded)
        })
        && !view.exposed.iter().any(|exposed| exposed == id)
}

pub(super) fn rows(
    snapshot: &Snapshot,
    slots: &[(Order, String, usize)],
    view: &View,
    read_view: &ReadView,
) -> Vec<Row> {
    let mut ids: Vec<_> = slots
        .iter()
        .map(|(order, id, _)| (order.clone(), id.clone()))
        .collect();
    ids.dedup_by(|left, right| left.1 == right.1);
    let parents: BTreeMap<_, _> = super::rows(snapshot, &ids, view)
        .into_iter()
        .map(|mut row| {
            if let Some(children) = read_view.children.get(&row.occurrence) {
                row.details = Some(idle_history::timeline::Details {
                    count: Some(u64::try_from(children.len()).unwrap_or(u64::MAX)),
                    expanded: true,
                });
            }
            (row.occurrence.clone(), row)
        })
        .collect();
    slots
        .iter()
        .filter_map(|(_, id, index)| {
            let parent = parents.get(id)?;
            if *index == 0 {
                return Some(parent.clone());
            }
            let mut child = read_view
                .children
                .get(id)?
                .get(index.saturating_sub(1))?
                .clone();
            details::decorate(&mut child, parent);
            Some(child)
        })
        .collect()
}
