//! Captions follow graph endpoints; relation chips retain their recorded meaning.

use idle_history::timeline::{RelationshipKind, Row, Target};

pub(super) fn apply(row: &mut Row, session: Option<&str>) {
    for relation in &row.relationships {
        let label = match relation.kind {
            RelationshipKind::Spawn => "subagent",
            RelationshipKind::Completion => "return",
            RelationshipKind::Fork => "fork",
            RelationshipKind::Parent | RelationshipKind::Git => continue,
        };
        if !row.tags.iter().any(|tag| tag == label) {
            row.tags.push(label.into());
        }
    }
    let graph = &row.graph;
    let above = graph.above.contains(&graph.lane)
        || graph.transitions.iter().any(|turn| turn.1 == graph.lane);
    let below = graph.below.contains(&graph.lane)
        || graph.transitions.iter().any(|turn| turn.0 == graph.lane);
    if above && below {
        return;
    }
    row.presentation.session_label = match &row.address {
        Target::Commit { repository, .. } => format!("Git · repo {}", short(repository)),
        Target::Record(_) => session.map_or_else(
            || "EditChain ops".into(),
            |id| {
                if row.session.is_empty() || row.session == id {
                    format!("Session {}", short(id))
                } else {
                    row.session
                        .strip_prefix("Human work · ")
                        .unwrap_or(&row.session)
                        .into()
                }
            },
        ),
        Target::CommitFile(_) => String::new(),
    };
}

fn short(id: &str) -> String {
    id.chars()
        .skip(id.chars().count().saturating_sub(12))
        .collect()
}
