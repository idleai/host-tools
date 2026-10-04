//! Read Git on the file-owning host, independently of editor view lifetimes.

use editchain_git::{RepositoryCatalog, open_repository};
use idle_history::human::HumanGitContext;
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

/// Observe exact worktree roots and HEAD objects without deriving authorship.
/// # Errors
/// Returns discovery, Git, clock or repository-count errors.
pub fn observe_context(workspace: &Path) -> crate::Result<serde_json::Value> {
    if !workspace.is_absolute() {
        return Err("capture workspace must be absolute".into());
    }
    let catalog = RepositoryCatalog::discover(workspace)?;
    let mut repositories = Vec::new();
    for discovery in catalog.entries() {
        let Some(root) = &discovery.worktree_root else {
            continue;
        };
        let handle = open_repository(discovery)?;
        repositories.push(HumanGitContext {
            repository: discovery.id.0.to_string(),
            root: root.to_string_lossy().into_owned(),
            head: handle.repo.head_id().ok().map(|oid| oid.to_string()),
        });
    }
    if repositories.len() > 64 {
        return Err("workspace exceeds the 64-repository editor context limit".into());
    }
    let observed_ms = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    Ok(
        serde_json::json!({"observed_ms": observed_ms, "workspace_path": workspace, "repositories": repositories}),
    )
}
