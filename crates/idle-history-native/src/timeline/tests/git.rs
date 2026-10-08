//! Live commits retain Git addresses and refresh without changing recorded history.

use std::{io, path::Path, process::Command};

use editchain_engine::{Engine, queries::ChainQueries};
use idle_history::{query::QueryResult, timeline::Target};

use super::{binding, latest};

fn git(root: &Path, arguments: &[&str]) -> io::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
        ])
        .args(arguments)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[test]
fn live_git_pages_refresh_and_open_exact_commits_in_the_bound_checkout() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let _init = git(root, &["init", "--quiet"])?;
    std::fs::write(root.join("file.txt"), "first\n")?;
    let _added = git(root, &["add", "file.txt"])?;
    let _committed = git(root, &["commit", "--quiet", "-m", "feat: first change"])?;
    let first = git(root, &["rev-parse", "HEAD"])?;
    let chain = root.join(".editchain");
    drop(Engine::open(&chain)?);
    let mut installed = binding(&chain);
    installed.repository_directory = Some(root.to_owned());
    let before = ChainQueries::open(&chain)?.index().revision().clone();
    let initial = latest(&installed)?;
    equal!(initial.activities, 1, "one live commit");
    let row = initial
        .rows
        .first()
        .ok_or_else(|| io::Error::other("commit row"))?;
    let Target::Commit { repository, oid } = &row.address else {
        return Err(io::Error::other("Git destination"));
    };
    equal!(oid, &first, "complete object identity");
    equal!(
        row.preview,
        "first change",
        "readable conventional commit description"
    );
    check!(
        row.records.is_empty(),
        "live Git does not invent operation records"
    );
    let QueryResult::Commit { content, .. } = crate::git::document(&installed, repository, oid)?
    else {
        return Err(io::Error::other("commit document"));
    };
    check!(
        content.contains("+first"),
        "the document includes the actual patch"
    );
    check!(
        crate::git::document(&installed, "0", oid).is_err(),
        "repository substitution is rejected"
    );
    std::fs::write(root.join("file.txt"), "second\n")?;
    let _committed = git(root, &["commit", "--quiet", "-am", "fix: second change"])?;
    let changed = latest(&installed)?;
    equal!(changed.activities, 2, "HEAD updates add the new commit");
    equal!(changed.max_lane, 0, "linear Git history retains one lane");
    check!(
        changed.revision != initial.revision,
        "Git-only updates advance the timeline revision"
    );
    let _checkout = git(root, &["checkout", "--quiet", "--detach", &first])?;
    equal!(
        latest(&installed)?.activities,
        1,
        "a divergent HEAD retires unreachable live rows"
    );
    equal!(
        ChainQueries::open(&chain)?.index().revision(),
        before,
        "Git reads never append engine operations"
    );
    Ok(())
}
