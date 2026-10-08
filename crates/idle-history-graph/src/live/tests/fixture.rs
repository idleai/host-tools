use super::super::GraphNode;

pub(super) type HistoryRow = crate::live::RowGeometry;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct LiveBlockMeta {
    pub(super) source_stream: Option<(String, u32)>,
    pub(super) task_group: Option<String>,
    pub(super) task_summary: Option<String>,
    pub(super) task_protected: bool,
    pub(super) key: String,
    pub(super) sort_time: u64,
    pub(super) row_count: u64,
    pub(super) spans: Vec<()>,
    pub(super) node_key: String,
    pub(super) human_stream: Option<String>,
    #[serde(default)]
    pub(super) source_closed: bool,
    pub(super) parents: Vec<String>,
    pub(super) chain_state: idle_history::taxonomy::ChainState,
}
impl GraphNode for LiveBlockMeta {
    fn source_key(&self) -> Option<&str> {
        self.human_stream.as_deref()
    }
    fn closes_source(&self) -> bool {
        self.source_closed
    }
    fn key(&self) -> &String {
        &self.key
    }
    fn node_key(&self) -> &String {
        &self.node_key
    }
    fn parents(&self) -> &Vec<String> {
        &self.parents
    }
    fn sort_time(&self) -> u64 {
        self.sort_time
    }
    fn set_sort_time(&mut self, time: u64) {
        self.sort_time = time;
    }
    fn muted(&self) -> bool {
        !self.chain_state.is_active()
    }
    fn task_protected(&self) -> bool {
        self.task_protected
    }
    fn same_source(&self, other: &Self) -> bool {
        if self.human_stream.is_some() || other.human_stream.is_some() {
            self.human_stream == other.human_stream
        } else {
            self.source_stream
                .as_ref()
                .zip(other.source_stream.as_ref())
                .is_none_or(|(left, right)| left == right)
        }
    }
}
