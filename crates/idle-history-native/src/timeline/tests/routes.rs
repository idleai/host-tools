use super::*;

#[test]
fn recorded_nested_and_independent_sources_keep_separate_columns() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let bound = binding(directory.path());
    for (value, source, parents, lane) in [
        (1, 1000, vec![], 0),
        (2, 1001, vec![1], 1),
        (3, 1002, vec![2], 2),
        (4, 1000, vec![1], 0),
        (5, 1003, vec![], 3),
        (6, 1002, vec![3], 2),
        (7, 1001, vec![2, 6], 1),
        (8, 1000, vec![4, 7], 0),
    ] {
        let _accepted = engine.append(&message(value, source, value, &parents)?)?;
        let window = latest(&bound)?;
        equal!(
            row(&window, value)?.graph.lane,
            lane,
            "recorded source keeps its column"
        );
        equal!(row(&window, 1)?.graph.lane, 0, "main column remains stable");
    }
    let window = latest(&bound)?;
    check!(
        row(&window, 5)?
            .graph
            .transitions
            .iter()
            .all(|transition| transition.0 != 3 && transition.1 != 3),
        "independent activity has no invented attachment"
    );
    Ok(())
}

#[test]
fn route_upgrade_keeps_the_previous_revision_readable_and_resumes_after_cancel() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    for (value, source, parents) in [(1, 1000, vec![]), (2, 1001, vec![]), (3, 1000, vec![1])] {
        let _accepted = engine.append(&message(value, source, value, &parents)?)?;
    }
    let bound = binding(directory.path());
    let before = latest(&bound)?;
    {
        let storage =
            editchain_index::Storage::open(&directory.path().join("activity-timeline-v1"))?;
        let mut checkpoint: super::super::model::Checkpoint = storage.load()?;
        checkpoint
            .ready
            .as_mut()
            .ok_or_else(|| io::Error::other("ready snapshot"))?
            .routes_ready = false;
        let _saved: super::super::model::Checkpoint = storage.commit(&checkpoint)?;
    }
    let _accepted = engine.append(&message(4, 1001, 4, &[2])?)?;
    let Response::Window(during) = request(
        &bound,
        Action::Window {
            view: View::default(),
            position: Position::Latest,
            limit: 200,
        },
    )?
    else {
        return Err(io::Error::other("previous readable revision"));
    };
    equal!(
        during.rows,
        before.rows,
        "route preparation keeps the complete previous rows"
    );
    let progress = during
        .rebuilding
        .ok_or_else(|| io::Error::other("route progress"))?;
    equal!(
        progress.stage,
        "Rebuilding Activity routes",
        "upgrade stage is explicit"
    );
    let _cancelled = request(
        &bound,
        Action::Cancel {
            build: progress.build,
        },
    )?;
    let after = latest(&bound)?;
    equal!(
        after.activities,
        4,
        "source appends are applied after route preparation"
    );
    equal!(
        row(&after, 2)?.graph.lane,
        row(&after, 4)?.graph.lane,
        "independent stream keeps its column"
    );
    check!(
        row(&after, 1)?.graph.lane != row(&after, 2)?.graph.lane,
        "separate streams have separate columns"
    );
    check!(
        after
            .rows
            .iter()
            .all(|row| row.graph.transitions.is_empty()),
        "no false joins appear after migration"
    );
    Ok(())
}
