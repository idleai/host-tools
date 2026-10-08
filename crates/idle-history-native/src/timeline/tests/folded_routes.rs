//! Collapsing interleaved tasks must preserve every visible graph connection.

use super::*;

fn connected(rows: &[Row]) -> io::Result<()> {
    for pair in rows.windows(2) {
        if let [newer, older] = pair {
            equal!(
                newer.graph.below,
                older.graph.above,
                format!(
                    "disconnected rows {} and {}",
                    newer.occurrence, older.occurrence
                )
            );
            equal!(
                newer.graph.muted_below,
                older.graph.muted_above,
                "muted paths remain continuous"
            );
        }
    }
    Ok(())
}

fn paged(binding: &Binding, view: &View, limit: u32) -> io::Result<Vec<Row>> {
    let mut position = Position::Latest;
    let mut rows = Vec::new();
    loop {
        let Response::Window(window) = request(
            binding,
            Action::Window {
                view: view.clone(),
                position,
                limit,
            },
        )?
        else {
            return Err(io::Error::other("indexed graph window"));
        };
        rows.extend(window.rows);
        let Some(cursor) = window.older else {
            return Ok(rows);
        };
        position = Position::Page(cursor);
    }
}

#[test]
fn interleaved_task_disclosures_keep_connections_across_page_boundaries() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let bound = binding(directory.path());
    for (value, source, parent, task) in [
        (1, 800, None, Some(900)),
        (2, 801, None, Some(901)),
        (3, 800, Some(1), Some(900)),
        (4, 801, Some(2), Some(901)),
        (5, 801, Some(4), Some(901)),
        (6, 802, None, None),
        (7, 800, Some(3), Some(900)),
        (8, 801, Some(5), Some(901)),
        (10, 800, Some(7), Some(900)),
        (11, 802, Some(6), None),
    ] {
        let mut op = message(
            value,
            source,
            value,
            &parent.into_iter().collect::<Vec<_>>(),
        )?;
        if let editchain_core::OpKind::Activity(operation) = &mut op.kind {
            operation.turn = task.map(|value| ItemId(id(value)));
        }
        let _admitted = engine.append(&op)?;
    }
    let expanded = latest(&bound)?;
    connected(&expanded.rows)?;
    let groups = [10, 8]
        .into_iter()
        .map(|value| {
            row(&expanded, value)?
                .group
                .clone()
                .ok_or_else(|| io::Error::other("interleaved task group"))
        })
        .collect::<io::Result<Vec<_>>>()?;
    check!(
        groups
            .iter()
            .all(|group| group.count == 3 && group.expanded),
        "both tasks have three initially expanded members"
    );
    for choices in [[false, false], [true, false], [false, true], [true, true]] {
        let view = View {
            disclosures: groups
                .iter()
                .zip(choices)
                .map(|(group, expanded)| idle_history::timeline::Disclosure {
                    group: group.id.clone(),
                    expanded,
                })
                .collect(),
            ..View::default()
        };
        let rows = paged(&bound, &view, 200)?;
        connected(&rows)?;
        equal!(
            rows.len(),
            6 + choices.into_iter().filter(|expanded| *expanded).count() * 2,
            "disclosure removes only the chosen task interiors"
        );
        for header in rows
            .iter()
            .filter(|row| row.group.as_ref().is_some_and(|group| !group.expanded))
        {
            let group = header.group.as_ref().expect("collapsed task");
            let entry = expanded
                .rows
                .iter()
                .find(|row| row.occurrence == group.entry)
                .ok_or_else(|| io::Error::other("expanded entry"))?;
            equal!(
                header.graph.parents,
                entry.graph.parents,
                "the task still points to its external parent"
            );
        }
        for limit in [1, 2, 3] {
            let pages = paged(&bound, &view, limit)?;
            connected(&pages)?;
            equal!(pages, rows, "page size cannot change retained connections");
        }
    }
    Ok(())
}
