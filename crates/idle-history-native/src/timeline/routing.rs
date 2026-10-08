//! Rebuild derived routes in bounded batches while the old revision stays readable.

use std::ops::Bound::{Excluded, Unbounded};

use editchain_index::rank::Measure;
use history_geometry::live::Order;

use super::{
    model::{BATCH, Build, Phase, Snapshot},
    read,
};

pub(super) fn prepare(ready: &Snapshot) -> Snapshot {
    let mut snapshot = ready.clone();
    snapshot.graph = history_geometry::live::LiveGraph::default();
    snapshot.groups.clear();
    snapshot.membership.clear();
    snapshot.group_details.clear();
    snapshot.task_groups.clear();
    snapshot
}

pub(super) fn advance(build: &mut Build, after: Option<Order>) {
    let mut cursor = after;
    let mut nodes = Vec::new();
    for _ in 0..BATCH {
        let next = build
            .snapshot
            .order
            .bound(cursor.as_ref().map_or(Unbounded, Excluded), true)
            .map(|(order, id)| (order.clone(), id.clone()));
        let Some((order, id)) = next else { break };
        if let Some(mut node) = build.snapshot.nodes.get(&id).cloned() {
            node.source_closed = build
                .snapshot
                .facts
                .get(&id)
                .is_some_and(|fact| fact.terminal);
            drop(build.snapshot.nodes.insert(id.clone(), node.clone()));
            drop(build.snapshot.order.insert(
                order.clone(),
                id,
                Measure {
                    expanded: 1,
                    visible: 1,
                },
            ));
            read::index_order(&mut build.snapshot, &node, false);
            nodes.push(node);
        }
        cursor = Some(order);
        build.processed = build.processed.saturating_add(1);
    }
    build.snapshot.graph.edit_stream(&[], &nodes);
    build.phase = if nodes.len() < BATCH {
        build.snapshot.routes_ready = true;
        build.snapshot.summaries_ready = true;
        build.processed = 0;
        Phase::Group { after: None }
    } else {
        Phase::Routes { after: cursor }
    };
}
