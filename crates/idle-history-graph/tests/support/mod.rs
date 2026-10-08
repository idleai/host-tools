//! Render graph fixtures through the same retained graph used by the application.

use editchain_core::OpId;
use idle_history::taxonomy::ChainState;
use idle_history_graph::live::{GraphNode, LiveGraph, RowGeometry};
use std::collections::HashMap;

fn required<T>(value: Option<T>, message: &str) -> T {
    #[expect(
        clippy::expect_used,
        reason = "A missing fixture node or edge must fail the regression test immediately."
    )]
    value.expect(message)
}

#[derive(Debug, Clone)]
struct Node {
    key: String,
    parents: Vec<String>,
    time: u64,
    git: bool,
    state: ChainState,
}

impl GraphNode for Node {
    fn key(&self) -> &String {
        &self.key
    }
    fn node_key(&self) -> &String {
        &self.key
    }
    fn is_git(&self) -> bool {
        self.git
    }
    fn parents(&self) -> &Vec<String> {
        &self.parents
    }
    fn sort_time(&self) -> u64 {
        self.time
    }
    fn set_sort_time(&mut self, time: u64) {
        self.time = time;
    }
    fn muted(&self) -> bool {
        !self.state.is_active()
    }
    fn task_protected(&self) -> bool {
        false
    }
    fn same_source(&self, _other: &Self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GridPoint {
    pub row: usize,
    pub lane: usize,
}

#[derive(Debug)]
pub(super) struct LaneEdge {
    pub child: String,
    pub parent: String,
    pub points: Vec<GridPoint>,
}

#[derive(Debug)]
pub(super) struct NodeRow {
    pub node: String,
    pub lane: usize,
}

#[derive(Debug)]
pub(super) struct GraphFixture {
    graph: LiveGraph<Node>,
    pub keys: Vec<String>,
    pub rows: Vec<NodeRow>,
    pub edges: Vec<LaneEdge>,
    pub lane_at: HashMap<String, usize>,
    pub row_above: Vec<Vec<usize>>,
    pub row_below: Vec<Vec<usize>>,
    pub row_transitions: Vec<Vec<(usize, usize)>>,
    pub row_muted_above: Vec<Vec<usize>>,
    pub row_muted_below: Vec<Vec<usize>>,
    pub row_muted_transitions: Vec<Vec<(usize, usize)>>,
}

impl GraphFixture {
    pub(super) fn new(
        keys: &[String],
        parents: &impl Fn(&str) -> Vec<String>,
        git: &impl Fn(&str) -> bool,
    ) -> Self {
        Self::with_state(keys, parents, git, &|_| ChainState::Active)
    }

    pub(super) fn with_state(
        keys: &[String],
        parents: &impl Fn(&str) -> Vec<String>,
        git: &impl Fn(&str) -> bool,
        state: &impl Fn(&str) -> ChainState,
    ) -> Self {
        let nodes: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(index, key)| Node {
                key: key.clone(),
                parents: parents(key),
                git: git(key),
                state: state(key),
                time: u64::try_from(keys.len().saturating_sub(index)).unwrap_or(u64::MAX),
            })
            .collect();
        let mut graph = LiveGraph::default();
        graph.edit(&[], &nodes);
        let keys: Vec<_> = graph.ordered_keys().cloned().collect();
        let row_of: HashMap<_, _> = keys
            .iter()
            .enumerate()
            .map(|(row, key)| (key, row))
            .collect();
        let mut rows = Vec::new();
        let mut edges = Vec::new();
        for key in &keys {
            let mut row = RowGeometry::default();
            graph.decorate(key, 0, &mut row);
            for parent in &row.parents {
                if let Some(route) = graph.route_points(key, parent) {
                    edges.push(LaneEdge {
                        child: key.clone(),
                        parent: parent.clone(),
                        points: route
                            .into_iter()
                            .map(|(key, lane)| GridPoint {
                                row: *required(
                                    row_of.get(&key),
                                    "route point must name a fixture row",
                                ),
                                lane,
                            })
                            .collect(),
                    });
                }
            }
            rows.push(row);
        }
        Self {
            lane_at: keys
                .iter()
                .zip(&rows)
                .map(|(key, row)| (key.clone(), row.lane))
                .collect(),
            rows: keys
                .iter()
                .zip(&rows)
                .map(|(key, row)| NodeRow {
                    node: key.clone(),
                    lane: row.lane,
                })
                .collect(),
            row_above: rows.iter().map(|row| row.above.clone()).collect(),
            row_below: rows.iter().map(|row| row.below.clone()).collect(),
            row_transitions: rows.iter().map(|row| row.transitions.clone()).collect(),
            row_muted_above: rows.iter().map(|row| row.muted_above.clone()).collect(),
            row_muted_below: rows.iter().map(|row| row.muted_below.clone()).collect(),
            row_muted_transitions: rows
                .iter()
                .map(|row| row.muted_transitions.clone())
                .collect(),
            graph,
            keys,
            edges,
        }
    }

    pub(super) fn edge(&self, child: &str, parent: &str) -> &LaneEdge {
        required(
            self.edges
                .iter()
                .find(|edge| edge.child == child && edge.parent == parent),
            "fixture edge must exist",
        )
    }

    pub(super) fn max_lane(&self) -> usize {
        self.graph.max_lane()
    }

    pub(super) fn spine_lane(&self, child: &str, parent: &str) -> usize {
        required(
            self.edge(child, parent).points.get(1),
            "fixture route must include a spine",
        )
        .lane
    }
}

pub(super) fn graph_layout(
    nodes: &[String],
    parents: impl Fn(&str) -> Vec<String>,
    git: &impl Fn(&str) -> bool,
) -> GraphFixture {
    GraphFixture::new(nodes, &parents, git)
}

pub(super) fn operation_rows(nodes: &[OpId], parents: impl Fn(&OpId) -> Vec<OpId>) -> Vec<NodeRow> {
    let keys: Vec<_> = nodes.iter().map(ToString::to_string).collect();
    graph_layout(
        &keys,
        |key| {
            let node = required(
                nodes.iter().find(|node| node.to_string() == key),
                "fixture operation must exist",
            );
            parents(node).iter().map(ToString::to_string).collect()
        },
        &|_| false,
    )
    .rows
}
