//! Window queries share lane-prefix traversals across their row boundaries.

use std::collections::BTreeMap;

use super::{GraphNode, LiveGraph, Order, RowGeometry};

impl<N: GraphNode> LiveGraph<N> {
    /// Root-row geometry for a bounded window, including paths across its edges.
    /// Unknown keys are omitted. Supplied keys need not be in causal order.
    #[must_use]
    pub fn decorate_many(&self, keys: &[String]) -> BTreeMap<String, RowGeometry> {
        let boundaries: Vec<_> = keys
            .iter()
            .filter_map(|key| {
                self.nodes.get(key).map(|node| {
                    (
                        node.order(),
                        node.parents()
                            .iter()
                            .filter_map(|parent| {
                                self.nodes.get(parent).map(|node| node.node_key().clone())
                            })
                            .collect(),
                    )
                })
            })
            .collect();
        self.decorate_bounds(&boundaries)
    }

    /// Decorate retained order boundaries and their already resolved parent IDs.
    /// The caller supplies metadata from the same graph revision. This avoids
    /// loading full node records again when a native read index retains them.
    #[must_use]
    pub fn decorate_bounds(
        &self,
        boundaries: &[(Order, Vec<String>)],
    ) -> BTreeMap<String, RowGeometry> {
        let mut rows: Vec<_> = boundaries
            .iter()
            .filter_map(|(order, parents)| {
                let lane = self.lanes.node(&order.1)?;
                let mut row = RowGeometry {
                    lane: self.lanes.display(lane),
                    parents: parents.clone(),
                    ..RowGeometry::default()
                };
                self.fill_bends(order, &mut row);
                Some((&order.1, order.clone(), row))
            })
            .collect();
        let points: Vec<_> = rows
            .iter()
            .flat_map(|(_, order, _)| [(order.clone(), 0), (order.clone(), 2)])
            .collect();
        for (lane, coverage) in self.lanes.iter() {
            let lane = self.lanes.display(lane);
            let owners = coverage.at_many(&points);
            for ((_, _, row), pair) in rows.iter_mut().zip(owners.chunks_exact(2)) {
                if let [above, below] = pair {
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
            }
        }
        rows.into_iter()
            .map(|(key, _, row)| (key.clone(), row))
            .collect()
    }
}
