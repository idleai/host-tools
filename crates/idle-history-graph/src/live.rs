//! Retained Activity graph: the established lane planner and Git-style routes.
//! Geometry is indexed by stable row boundaries, so inserting a row never
//! rewrites every later coordinate. Native paging and WASM use this same state.

mod batch;
mod edit;
mod events;
mod lanes;
mod order;
mod routes;
#[cfg(test)]
mod tests;

mod types;
pub use types::{GraphNode, GraphRow, RowGeometry};

use editchain_index::{Map, OrderedMap, OrderedSet};
use lanes::{Lane, Lanes};
use routes::Path;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Stable rendering clock, item identity and row slot.
pub type Order = (std::cmp::Reverse<u64>, String, u8);
type Edge = (String, String);
type Point = (Order, u8);

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
enum Change {
    Add,
    Remove,
}

impl Change {
    fn apply(self, count: &mut u64) {
        *count = match self {
            Self::Add => count.saturating_add(1),
            Self::Remove => count.saturating_sub(1),
        };
    }
}

#[derive(Debug, Default, Clone, Copy, serde::Serialize, serde::Deserialize)]
struct Owners {
    active: u64,
    muted: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Spine {
    lane: Lane,
    start: Order,
    end: Order,
}

impl Owners {
    fn change(&mut self, muted: bool, change: Change) {
        let count = if muted {
            &mut self.muted
        } else {
            &mut self.active
        };
        change.apply(count);
    }
    fn present(self) -> bool {
        self.active > 0 || self.muted > 0
    }
    fn muted(self) -> bool {
        self.active == 0 && self.muted > 0
    }
}

/// Causal lanes and edge events retained across operation deltas.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(bound(
    serialize = "N: serde::Serialize",
    deserialize = "N: serde::de::DeserializeOwned"
))]
pub struct LiveGraph<N: GraphNode> {
    nodes: Map<String, N>,
    order: OrderedSet<Order>,
    incoming: Map<String, BTreeSet<String>>,
    lanes: Lanes,
    paths: Map<Edge, Path>,
    bends: OrderedMap<Order, BTreeMap<(Lane, Lane), Owners>>,
    spines: Map<String, Spine>,
    #[serde(skip)]
    changed: BTreeSet<String>,
}

impl<N: GraphNode> LiveGraph<N> {
    /// Stable item keys in the current rendering order.
    pub fn ordered_keys(&self) -> impl Iterator<Item = &String> {
        self.order.iter().map(|order| &order.1)
    }

    /// Ordered route corners as `(item boundary, display lane)` pairs.
    /// A route covers straight trunks, parent-side forks and merge jogs without
    /// allocating a point for every intervening row.
    #[must_use]
    pub fn route_points(&self, child: &str, parent: &str) -> Option<Vec<(String, usize)>> {
        let path = self.paths.get(&(child.to_owned(), parent.to_owned()))?;
        let from = (child.to_owned(), self.lanes.node(child)?);
        let to = (parent.to_owned(), self.lanes.node(parent)?);
        let mut next = BTreeMap::new();
        for (lane, start, end) in &path.runs {
            drop(next.insert((start.0.1.clone(), *lane), (end.0.1.clone(), *lane)));
        }
        for (at, from, to) in &path.bends {
            drop(next.insert((at.1.clone(), *from), (at.1.clone(), *to)));
        }
        let mut points = Vec::new();
        let mut cursor = from;
        for _step in 0..=next.len() {
            points.push((cursor.0.clone(), self.lanes.display(cursor.1)));
            if cursor == to {
                return Some(points);
            }
            cursor = next.get(&cursor)?.clone();
        }
        None
    }

    /// Endpoints whose disclosure safety may have changed in the last edit.
    pub fn changed_boundaries(&self) -> impl Iterator<Item = &String> {
        self.changed.iter()
    }

    /// Only straight interiors can disappear; preserve every real attachment,
    /// every routing bend (including passing lanes), and protected outcomes.
    #[must_use]
    pub fn foldable(&self, key: &str) -> bool {
        self.task_member(key).is_some()
            && self
                .incoming
                .get(key)
                .is_some_and(|children| children.len() == 1)
    }

    /// A task path may end at an active tip, but never cross a junction, Git
    /// attachment, protected outcome or routing bend. Callers still verify
    /// each exact parent edge and native task identity before joining members.
    #[must_use]
    pub fn task_member(&self, key: &str) -> Option<&N> {
        let node = self.nodes.get(key)?;
        let children = self.incoming.get(key);
        (!is_git(node)
            && !node.task_protected()
            && node.parents().len() == 1
            && children.is_none_or(|children| children.len() <= 1)
            && !self.bends.contains_key(&node.order())
            && node
                .parents()
                .iter()
                .chain(children.into_iter().flatten())
                .all(|key| self.nodes.get(key).is_some_and(|node| !is_git(node))))
        .then_some(node)
    }

    /// Highest occupied lane, including shared session-to-Git routing spines.
    #[must_use]
    pub fn max_lane(&self) -> usize {
        self.lanes.max_lane()
    }

    /// Decorate a root or detail row with the same graph contract as Activity.
    pub fn decorate(&self, key: &str, slot: u64, output: &mut impl GraphRow) {
        let Some(meta) = self.nodes.get(key) else {
            return;
        };
        let order = meta.order();
        let mut row = self.attachments(meta, slot);
        for (lane, coverage) in self.lanes.iter() {
            let lane = self.lanes.display(lane);
            let above = coverage.at(&(order.clone(), if slot == 0 { 0 } else { 2 }));
            let below = coverage.at(&(order.clone(), 2));
            if above.present() {
                row.above.push(lane);
            }
            if below.present() {
                row.below.push(lane);
            }
            if above.muted() {
                row.muted_above.push(lane);
            }
            if below.muted() {
                row.muted_below.push(lane);
            }
        }
        output.set_graph(row, slot == 0);
    }

    fn attachments(&self, meta: &N, slot: u64) -> RowGeometry {
        let mut row = RowGeometry {
            lane: self
                .lanes
                .node(meta.key())
                .map_or(0, |lane| self.lanes.display(lane)),
            ..RowGeometry::default()
        };
        if slot == 0 {
            row.parents = meta
                .parents()
                .iter()
                .filter_map(|parent| self.nodes.get(parent).map(|node| node.node_key().clone()))
                .collect();
            self.fill_bends(&meta.order(), &mut row);
        }
        row
    }

    fn fill_bends(&self, order: &Order, row: &mut RowGeometry) {
        for (lanes, owners) in self.bends.get(order).into_iter().flatten() {
            let lanes = (self.lanes.display(lanes.0), self.lanes.display(lanes.1));
            row.transitions.push(lanes);
            if owners.muted() {
                row.muted_transitions.push(lanes);
            }
        }
        row.transitions.sort_unstable();
        row.muted_transitions.sort_unstable();
    }

    fn bootstrap(&mut self) {
        self.lanes = Lanes::default();
        let keys: Vec<_> = self.order.iter().map(|order| order.1.clone()).collect();
        let parents: HashMap<_, _> = self
            .nodes
            .iter()
            .map(|(key, node)| (key.clone(), node.parents().clone()))
            .collect();
        let plan = crate::layout::plan_lanes(&keys, &parents, &|key| {
            self.nodes.get(key).is_some_and(is_git)
        });
        self.lanes.bootstrap(&plan, &self.nodes);
        self.spines.clear();
        for ((child, parent), lane) in plan.spines {
            if let (Some(child), Some(target)) = (self.nodes.get(&child), self.nodes.get(&parent)) {
                let start = child.order();
                let _: &mut Spine = self
                    .spines
                    .entry(parent)
                    .and_modify(|spine| {
                        spine.start = spine.start.clone().min(start.clone());
                    })
                    .or_insert(Spine {
                        lane: Lane::Spine(lane.saturating_sub(1)),
                        start,
                        end: target.order(),
                    });
            }
        }
        for key in keys {
            self.add_paths(&key);
        }
    }

    fn add_paths(&mut self, key: &str) {
        let parents = self
            .nodes
            .get(key)
            .map(|node| node.parents().clone())
            .unwrap_or_default();
        for parent in parents {
            if let Some(old) = self.paths.remove(&(key.to_owned(), parent.clone())) {
                self.paint(&old, Change::Remove);
            }
            if let Some(path) = self.route(key, &parent) {
                self.paint(&path, Change::Add);
                drop(self.paths.insert((key.to_owned(), parent), path));
            }
        }
    }

    fn remove_paths(&mut self, key: &str) {
        let parents = self
            .nodes
            .get(key)
            .map(|node| node.parents().clone())
            .unwrap_or_default();
        for parent in parents {
            if let Some(path) = self.paths.remove(&(key.to_owned(), parent)) {
                self.paint(&path, Change::Remove);
            }
        }
    }

    fn paint(&mut self, path: &Path, change: Change) {
        for (lane, start, end) in &path.runs {
            self.lanes
                .coverage(*lane)
                .change(start, end, path.muted, change);
        }
        for (at, from, to) in &path.bends {
            let _: bool = self.changed.insert(at.1.clone());
            let bends = self.bends.entry(at.clone()).or_default();
            let owners = bends.entry((*from, *to)).or_default();
            owners.change(path.muted, change);
            if !owners.present() {
                let _: Option<Owners> = bends.remove(&(*from, *to));
            }
            if bends.is_empty() {
                drop(self.bends.remove(at));
            }
        }
    }
}

fn is_git<N: GraphNode>(node: &N) -> bool {
    node.is_git()
}

impl<N: GraphNode> Default for LiveGraph<N> {
    fn default() -> Self {
        Self {
            nodes: Map::default(),
            order: OrderedSet::default(),
            incoming: Map::default(),
            lanes: Lanes::default(),
            paths: Map::default(),
            bends: OrderedMap::default(),
            spines: Map::default(),
            changed: BTreeSet::default(),
        }
    }
}
