//! Host-independent inputs and outputs for retained routing.

use super::Order;

/// Geometry reads recorded relationships without owning their interpretation.
pub trait GraphNode: Clone {
    /// Stable logical identity, independent of the currently displayed record.
    fn key(&self) -> &String;
    /// Physical or typed external identity used by the compatibility viewer.
    fn node_key(&self) -> &String;
    /// Whether this recorded node occupies the Git column.
    fn is_git(&self) -> bool {
        self.node_key().starts_with("git:")
    }
    /// All explicitly supplied layout parents.
    fn parents(&self) -> &Vec<String>;
    /// Rendering clock; never a replacement for the recorded wall time.
    fn sort_time(&self) -> u64;
    /// Adjust only the local rendering clock.
    fn set_sort_time(&mut self, time: u64);
    /// Whether paths from this node should be visually muted.
    fn muted(&self) -> bool;
    /// Whether the node must remain visible during legacy path folding.
    fn task_protected(&self) -> bool;
    /// Whether two nodes explicitly share a producer stream.
    fn same_source(&self, other: &Self) -> bool;
    /// Persistent producer identity whose active column must stay reserved.
    fn source_key(&self) -> Option<&str> {
        None
    }
    /// An explicitly recorded end releases the source's column for later work.
    fn closes_source(&self) -> bool {
        false
    }
    /// Stable order used by rank and route indexes.
    fn order(&self) -> Order {
        (std::cmp::Reverse(self.sort_time()), self.key().clone(), 1)
    }
}

/// Continuous graph fragments for one row, including offscreen connections.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowGeometry {
    /// Node column.
    pub lane: usize,
    /// Present physical parent identities.
    pub parents: Vec<String>,
    /// Columns entering the top half of the row.
    pub above: Vec<usize>,
    /// Columns leaving the bottom half of the row.
    pub below: Vec<usize>,
    /// Cross-column connections at this boundary.
    pub transitions: Vec<(usize, usize)>,
    /// Top fragments owned only by muted paths.
    pub muted_above: Vec<usize>,
    /// Bottom fragments owned only by muted paths.
    pub muted_below: Vec<usize>,
    /// Cross-column fragments owned only by muted paths.
    pub muted_transitions: Vec<(usize, usize)>,
}

/// Adapter for the retiring viewer's wire row.
pub trait GraphRow {
    /// Replace only the geometry fields of a row.
    fn set_graph(&mut self, geometry: RowGeometry, root: bool);
}

impl GraphRow for RowGeometry {
    fn set_graph(&mut self, geometry: Self, _root: bool) {
        *self = geometry;
    }
}
