use super::*;
use editchain_index::Storage;

#[test]
fn compact_read_pages_resume_without_replacing_the_readable_snapshot() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let _admitted = engine.append(&message(1, 1000, 1, &[])?)?;
    let binding = binding(directory.path());
    let before = latest(&binding)?;
    {
        let storage = Storage::open(&directory.path().join("activity-timeline-v1"))?;
        let mut checkpoint: super::super::model::Checkpoint = storage.load()?;
        let ready = checkpoint
            .ready
            .as_mut()
            .ok_or_else(|| io::Error::other("ready snapshot"))?;
        ready.summaries.clear();
        ready.summaries_ready = false;
        let _saved: super::super::model::Checkpoint = storage.commit(&checkpoint)?;
    }
    let _admitted = engine.append(&message(2, 1000, 2, &[1])?)?;
    let Response::Window(during) = request(
        &binding,
        Action::Window {
            view: View::default(),
            position: Position::Latest,
            limit: 200,
        },
    )?
    else {
        return Err(io::Error::other("readable previous snapshot"));
    };
    equal!(
        during.rows,
        before.rows,
        "preparation retains complete rows and exact addresses"
    );
    let progress = during
        .rebuilding
        .ok_or_else(|| io::Error::other("read-page progress"))?;
    equal!(
        progress.stage,
        "Preparing activity read pages",
        "the resumed phase is explicit"
    );
    let _cancelled = request(
        &binding,
        Action::Cancel {
            build: progress.build,
        },
    )?;
    let after = latest(&binding)?;
    equal!(
        after.activities,
        2,
        "source changes are applied after compact-page preparation"
    );
    equal!(
        row(&after, 1)?.address,
        row(&before, 1)?.address,
        "earlier exact record addresses survive reindexing"
    );
    check!(
        !row(&after, 1)?.graph.above.is_empty(),
        "the later child updates the routing over the prepared rows"
    );
    Ok(())
}
