//! Disclosure only removes straight, connected interiors of one recorded task.

use std::collections::BTreeSet;

use history_geometry::live::GraphNode;
use idle_history::timeline::Group;

use super::{
    logical,
    model::{MAX_GROUP, Snapshot},
    read,
};

pub(super) fn rebuild(snapshot: &mut Snapshot, affected: &[String]) {
    let mut candidates: BTreeSet<_> = affected.iter().cloned().collect();
    let old: BTreeSet<_> = affected
        .iter()
        .filter_map(|id| snapshot.membership.get(id))
        .cloned()
        .collect();
    for group in old {
        if let Some(members) = snapshot.groups.remove(&group) {
            for id in members {
                if let Some(task) = snapshot.facts.get(&id).and_then(|fact| fact.task.as_ref())
                    && let Some(groups) = snapshot.task_groups.get_mut(task)
                {
                    let _removed = groups.remove(&group);
                }
                drop(snapshot.membership.remove(&id));
                read::set_visible(snapshot, &id, true);
                let _inserted = candidates.insert(id);
            }
        }
        drop(snapshot.group_details.remove(&group));
    }
    let mut ordered: Vec<_> = candidates
        .into_iter()
        .filter_map(|id| snapshot.nodes.get(&id).map(|node| (node.order(), id)))
        .collect();
    ordered.sort();
    for (_, start) in ordered.into_iter().rev() {
        if snapshot.membership.contains_key(&start) {
            continue;
        }
        let Some(task) = snapshot
            .facts
            .get(&start)
            .and_then(|fact| fact.task.clone())
        else {
            continue;
        };
        let attempt = snapshot
            .facts
            .get(&start)
            .and_then(|fact| fact.attempt.clone());
        let mut members = Vec::new();
        let mut cursor = start;
        while members.len() < MAX_GROUP && !snapshot.membership.contains_key(&cursor) {
            let Some(_node) = snapshot.graph.task_member(&cursor) else {
                break;
            };
            if before_git_attachment(snapshot, &cursor) {
                break;
            }
            if snapshot
                .facts
                .get(&cursor)
                .is_none_or(|fact| fact.task.as_ref() != Some(&task) || fact.attempt != attempt)
            {
                break;
            }
            members.push(cursor.clone());
            let Some(child) = snapshot
                .children
                .get(&cursor)
                .filter(|children| children.len() == 1)
                .and_then(|children| children.iter().next())
            else {
                break;
            };
            cursor.clone_from(child);
        }
        if members.len() < 2 {
            continue;
        }
        members.reverse();
        let Some(exit) = members.first().cloned() else {
            continue;
        };
        let Some(entry) = members.last().cloned() else {
            continue;
        };
        let identity = attempt
            .as_ref()
            .map_or_else(|| task.clone(), |attempt| format!("{task}:{attempt}"));
        let id = format!("group:{identity}:{entry}");
        let live = snapshot.nodes.get(&exit).is_none_or(|node| {
            snapshot
                .tasks
                .get(&task)
                .and_then(|states| {
                    states
                        .range(..node.order())
                        .next_back()
                        .or_else(|| states.range(node.order()..).next())
                })
                .map_or_else(
                    || {
                        snapshot
                            .facts
                            .get(&exit)
                            .is_some_and(|fact| !fact.is_legacy() && !fact.is_projected())
                    },
                    |(_, terminal)| !*terminal,
                )
        });
        let live = logical::task_live(snapshot, &task).unwrap_or(live);
        let native = snapshot
            .facts
            .get(&exit)
            .is_some_and(|fact| !fact.is_legacy() && !fact.is_projected());
        let expanded = live && native;
        let summary = logical::task_caption(snapshot, &task, native.then_some(live));
        for (index, member) in members.iter().enumerate() {
            drop(snapshot.membership.insert(member.clone(), id.clone()));
            read::set_visible(snapshot, member, expanded || index == 0);
        }
        let _inserted = snapshot
            .task_groups
            .entry(task)
            .or_default()
            .insert(id.clone());
        drop(snapshot.group_details.insert(
            id.clone(),
            Group {
                id: id.clone(),
                count: u64::try_from(members.len()).unwrap_or(u64::MAX),
                summary,
                live,
                header: true,
                expanded,
                entry,
                exit,
            },
        ));
        drop(snapshot.groups.insert(id, members));
    }
}

fn before_git_attachment(snapshot: &Snapshot, id: &str) -> bool {
    // Keep the preceding activity explicit when a work path changes Git base,
    // even while the named commit is unavailable in this checkout.
    snapshot
        .children
        .get(id)
        .into_iter()
        .flatten()
        .filter_map(|child| snapshot.nodes.get(child))
        .any(|child| {
            child
                .relationships
                .iter()
                .any(|relation| relation.kind == idle_history::timeline::RelationshipKind::Git)
        })
}
