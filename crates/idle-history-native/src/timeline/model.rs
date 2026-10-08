//! Small persistent facts, occurrence nodes and revision state.

use std::collections::BTreeSet;

use editchain_core::{SourceId, activity::Entity};
use editchain_index::{IndexRevision, Map, OrderedMap, OrderedSet, rank::RankTree};
use history_geometry::live::{GraphNode, LiveGraph, Order};
use idle_history::{
    provider::ProviderEvidence,
    timeline::{Group, Relationship, Row, Source},
};
use serde::{Deserialize, Serialize};

/// Algorithm identity is separate from the transport contract.
pub(super) const INDEX_VERSION: u32 = 8;
pub(super) const BATCH: usize = 500;
pub(super) const MAX_GROUP: usize = 128;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Fact {
    pub row: Row,
    pub order_time: Option<u64>,
    pub parents: Vec<String>,
    pub causes: Vec<String>,
    pub original: Option<String>,
    pub aliases: Vec<String>,
    pub author: Option<String>,
    pub session: Option<String>,
    pub recorder: String,
    pub task: Option<String>,
    pub task_title: Option<(u64, String)>,
    #[serde(default)]
    pub attempt: Option<String>,
    pub path: Option<String>,
    pub source: Option<SourceId>,
    pub raw_hash: Option<[u8; 32]>,
    pub provider: Option<ProviderEvidence>,
    pub link: Option<(Entity, Vec<Entity>, String)>,
    pub git: Option<(String, Vec<String>)>,
    pub label: Option<String>,
    pub terminal: bool,
    pub protected: bool,
    pub outcome: editchain_core::activity::Status,
    pub searchable: bool,
    pub visibility: Visibility,
    pub supports: Vec<String>,
    pub form: RecordForm,
    pub human_edit: Option<String>,
    pub note_turn: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) enum Visibility {
    Primary,
    Supporting,
    Observation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) enum RecordForm {
    Native,
    Legacy,
    Projected,
}

impl Fact {
    pub(super) fn is_legacy(&self) -> bool {
        self.form == RecordForm::Legacy
    }
    pub(super) fn is_projected(&self) -> bool {
        self.form == RecordForm::Projected
    }
    pub(super) fn is_primary(&self) -> bool {
        self.visibility == Visibility::Primary
    }
    pub(super) fn is_observation(&self) -> bool {
        self.visibility == Visibility::Observation
    }
    pub(super) fn failed(&self) -> bool {
        matches!(
            self.outcome,
            editchain_core::activity::Status::Failure | editchain_core::activity::Status::Cancelled
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Node {
    pub key: String,
    pub parents: Vec<String>,
    pub clock: u64,
    pub source: String,
    #[serde(default)]
    pub source_closed: bool,
    pub git: bool,
    pub protected: bool,
    pub relationships: Vec<Relationship>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct Connection {
    pub relation: Relationship,
    /// An exact recorded attachment may lack a locally available endpoint.
    pub boundary: Option<String>,
}

impl GraphNode for Node {
    fn source_key(&self) -> Option<&str> {
        Some(&self.source)
    }
    fn closes_source(&self) -> bool {
        self.source_closed
    }
    fn key(&self) -> &String {
        &self.key
    }
    fn node_key(&self) -> &String {
        &self.key
    }
    fn parents(&self) -> &Vec<String> {
        &self.parents
    }
    fn sort_time(&self) -> u64 {
        self.clock
    }
    fn set_sort_time(&mut self, time: u64) {
        self.clock = time;
    }
    fn muted(&self) -> bool {
        false
    }
    fn task_protected(&self) -> bool {
        self.protected
    }
    fn same_source(&self, other: &Self) -> bool {
        self.source == other.source
    }
    fn is_git(&self) -> bool {
        self.git
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct Snapshot {
    pub revision: String,
    pub accepted: IndexRevision,
    pub retained: Option<IndexRevision>,
    pub retained_directory: Option<std::path::PathBuf>,
    pub facts: OrderedMap<String, Fact>,
    pub aliases: Map<String, BTreeSet<String>>,
    pub outputs: Map<String, BTreeSet<String>>,
    pub supporting: Map<String, BTreeSet<String>>,
    pub logical: super::logical::State,
    pub references: Map<String, OrderedSet<String>>,
    pub items: Map<String, OrderedSet<String>>,
    pub labels: Map<String, OrderedMap<(u64, String), String>>,
    pub providers: OrderedSet<String>,
    pub registry: super::providers::Registry,
    pub links: OrderedSet<String>,
    pub generations: Map<(u64, u32), OrderedMap<u64, String>>,
    pub terminals: Map<(u64, u32), OrderedMap<u64, String>>,
    pub git: Map<String, BTreeSet<String>>,
    pub git_source: Option<crate::git::Observation>,
    pub live_git: OrderedSet<String>,
    pub live_git_labels: BTreeSet<String>,
    pub extra: Map<String, Vec<Connection>>,
    pub contributions: Map<String, Vec<(String, Connection)>>,
    pub nodes: OrderedMap<String, Node>,
    pub graph: LiveGraph<Node>,
    pub order: RankTree<Order, String>,
    #[serde(default)]
    pub summaries: OrderedMap<Order, super::read::Summary>,
    #[serde(default)]
    pub summaries_ready: bool,
    #[serde(default)]
    pub routes_ready: bool,
    pub scopes: Map<String, RankTree<Order, String>>,
    pub children: Map<String, OrderedSet<String>>,
    pub groups: Map<String, Vec<String>>,
    pub membership: Map<String, String>,
    pub group_details: Map<String, Group>,
    pub tasks: Map<String, OrderedMap<Order, bool>>,
    pub task_groups: Map<String, OrderedSet<String>>,
    pub text: Map<String, String>,
    pub words: Map<Vec<u8>, OrderedSet<String>>,
    pub gaps: BTreeSet<String>,
    pub unavailable: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum Phase {
    Git(super::git::Walk),
    Scan {
        source: Source,
        after: Option<editchain_core::OpId>,
    },
    Resolve {
        after: Option<String>,
    },
    Materialize,
    Project {
        after: Option<String>,
    },
    PrepareOrder {
        after: Option<String>,
    },
    Order,
    Group {
        after: Option<Order>,
    },
    Changes {
        after: IndexRevision,
    },
    Relations,
    MaterializeChanges,
    Repair,
    Summaries {
        after: Option<Order>,
    },
    Routes {
        after: Option<Order>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Build {
    pub id: String,
    pub snapshot: Snapshot,
    pub phase: Phase,
    pub processed: u64,
    pub ready: OrderedSet<(u64, String)>,
    pub waiting: Map<String, u64>,
    pub dirty: OrderedSet<String>,
    pub relation_dirty: OrderedSet<String>,
    pub group_dirty: OrderedSet<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Checkpoint {
    pub version: u32,
    pub serial: u64,
    pub ready: Option<Snapshot>,
    pub build: Option<Build>,
    pub views: OrderedMap<String, super::read::ReadView>,
}

impl Default for Checkpoint {
    fn default() -> Self {
        Self {
            version: INDEX_VERSION,
            serial: 0,
            ready: None,
            build: None,
            views: OrderedMap::default(),
        }
    }
}

pub(super) fn key(source: Source, operation: impl std::fmt::Display) -> String {
    format!(
        "{}:{operation}",
        match source {
            Source::Current => "current",
            Source::Retained => "retained",
        }
    )
}
