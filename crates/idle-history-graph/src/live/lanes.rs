//! Stable lane identities and columns; new shared anchors use free columns.

use super::{GraphNode, Order, events::Coverage, is_git};
use editchain_index::Map;
use std::collections::BTreeMap;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub(super) enum Lane {
    Git,
    Spine(usize),
    Operation(usize),
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Lanes {
    nodes: Map<String, Lane>,
    git: Coverage,
    git_present: bool,
    spines: BTreeMap<usize, Coverage>,
    operations: BTreeMap<usize, Coverage>,
    positions: BTreeMap<Lane, usize>,
    next_position: usize,
    #[serde(default)]
    owners: BTreeMap<Lane, (String, Order)>,
    #[serde(default)]
    sources: Map<String, (Order, bool)>,
}

impl Lanes {
    pub(super) fn bootstrap<N: GraphNode>(
        &mut self,
        plan: &crate::layout::LanePlan,
        nodes: &Map<String, N>,
    ) {
        self.git_present = nodes.values().any(is_git);
        for index in 0..plan.spine_count {
            let _: &mut Coverage = self.spines.entry(index).or_default();
            let _: Option<usize> = self.positions.insert(Lane::Spine(index), index);
        }
        self.next_position = plan.spine_count;
        let base = usize::from(self.git_present).saturating_add(plan.spine_count);
        // Preserve the established bootstrap plan, independently of map iteration order.
        for index in plan.nodes.values().filter(|index| **index >= base) {
            let position = index.saturating_sub(usize::from(self.git_present));
            let lane = Lane::Operation(index.saturating_sub(base));
            let _: Option<usize> = self.positions.insert(lane, position);
            self.next_position = self.next_position.max(position.saturating_add(1));
        }
        for (key, index) in &plan.nodes {
            let Some(node) = nodes.get(key) else {
                continue;
            };
            let lane = if is_git(node) {
                Lane::Git
            } else {
                Lane::Operation(index.saturating_sub(base))
            };
            self.put(node, lane);
        }
    }
    pub(super) fn node(&self, key: &str) -> Option<Lane> {
        self.nodes.get(key).copied()
    }
    pub(super) fn put<N: GraphNode>(&mut self, node: &N, lane: Lane) {
        self.git_present |= is_git(node);
        let _: Option<Lane> = self.nodes.insert(node.key().clone(), lane);
        let _: bool = self.coverage(lane).dots.insert(node.order());
        if let Some(source) = node.source_key() {
            let order = node.order();
            if self
                .sources
                .get(source)
                .is_none_or(|(latest, _)| order <= *latest)
            {
                drop(
                    self.sources
                        .insert(source.to_owned(), (order.clone(), node.closes_source())),
                );
            }
            if self
                .owners
                .get(&lane)
                .is_none_or(|(_, latest)| order <= *latest)
            {
                drop(self.owners.insert(lane, (source.to_owned(), order)));
            }
        }
    }
    pub(super) fn remove_dot<N: GraphNode>(&mut self, node: &N, forget: bool) {
        if let Some(lane) = self.node(node.key()) {
            let _: bool = self.coverage(lane).dots.remove(&node.order());
            if forget {
                let _: Option<Lane> = self.nodes.remove(node.key());
            }
            if self.coverage(lane).empty() {
                drop(self.owners.remove(&lane));
            }
        }
        if let Some(source) = node.source_key()
            && let Some((latest, closed)) = self.sources.get_mut(source)
            && *latest == node.order()
        {
            // Retraction cannot establish that the remaining source has ended.
            *closed = false;
        }
    }
    pub(super) fn coverage(&mut self, lane: Lane) -> &mut Coverage {
        if lane != Lane::Git && !self.positions.contains_key(&lane) {
            let _: Option<usize> = self.positions.insert(lane, self.next_position);
            self.next_position = self.next_position.saturating_add(1);
        }
        let (collection, index) = match lane {
            Lane::Git => return &mut self.git,
            Lane::Spine(index) => (&mut self.spines, index),
            Lane::Operation(index) => (&mut self.operations, index),
        };
        collection.entry(index).or_default()
    }
    pub(super) fn allocate(
        &mut self,
        start: &Order,
        end: &Order,
        avoid: Option<Lane>,
        source: Option<&str>,
    ) -> Lane {
        let index = self
            .operations
            .iter()
            .find_map(|(index, lane)| {
                let identity = Lane::Operation(*index);
                (avoid != Some(identity)
                    && self.permits(identity, source)
                    && lane.available(start, end))
                .then_some(*index)
            })
            .unwrap_or(self.operations.len());
        let lane = Lane::Operation(index);
        let _coverage = self.coverage(lane);
        lane
    }
    pub(super) fn permits(&self, lane: Lane, source: Option<&str>) -> bool {
        source.is_none_or(|source| {
            self.owners.get(&lane).is_none_or(|(owner, _)| {
                owner == source || self.sources.get(owner).is_some_and(|(_, closed)| *closed)
            })
        })
    }
    pub(super) fn display(&self, lane: Lane) -> usize {
        match lane {
            Lane::Git => 0,
            Lane::Spine(_) | Lane::Operation(_) => self
                .positions
                .get(&lane)
                .copied()
                .unwrap_or(0)
                .saturating_add(usize::from(self.git_present)),
        }
    }
    pub(super) fn max_lane(&self) -> usize {
        self.iter()
            .filter(|(_, coverage)| !coverage.empty())
            .map(|(lane, _)| self.display(lane))
            .max()
            .unwrap_or(0)
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = (Lane, &Coverage)> {
        std::iter::once((Lane::Git, &self.git))
            .chain(
                self.spines
                    .iter()
                    .map(|(index, lane)| (Lane::Spine(*index), lane)),
            )
            .chain(
                self.operations
                    .iter()
                    .map(|(index, lane)| (Lane::Operation(*index), lane)),
            )
    }
}
