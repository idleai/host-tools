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

#[test]
fn recorded_git_context_loads_other_ancestry_only_for_the_bound_repository() -> io::Result<()> {
    use super::{id, message, row};
    use editchain_core::{
        GitOid, Payload, RepositoryId,
        activity::{Entity, ItemId, Kind, Link, Operation},
    };
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let _initialized = git(root, &["init", "--quiet"])?;
    std::fs::write(root.join("file.txt"), "root\n")?;
    let _added = git(root, &["add", "."])?;
    let _committed = git(root, &["commit", "--quiet", "-m", "root"])?;
    let first = git(root, &["rev-parse", "HEAD"])?;
    std::fs::write(root.join("file.txt"), "recorded branch\n")?;
    let _committed = git(root, &["commit", "--quiet", "-am", "recorded branch"])?;
    let branch = git(root, &["rev-parse", "HEAD"])?;
    let _checked_out = git(root, &["checkout", "--quiet", "--detach", &first])?;
    std::fs::write(root.join("file.txt"), "current branch\n")?;
    let _committed = git(root, &["commit", "--quiet", "-am", "current branch"])?;
    let head = git(root, &["rev-parse", "HEAD"])?;
    let chain = root.join(".editchain");
    let engine = Engine::open(&chain)?;
    let _appended = engine.append(&message(1, 800, 1, &[])?)?;
    let mut installed = binding(&chain);
    installed.repository_directory = Some(root.to_owned());
    equal!(
        latest(&installed)?.activities,
        3,
        "HEAD ancestry and one recorded activity"
    );
    let repository = crate::git::observe(&installed)
        .and_then(|source| source.repository)
        .ok_or_else(|| io::Error::other("fixture repository"))?;
    let link = |value, repository| {
        Operation::new(
            id(value),
            ItemId(id(value)),
            ItemId(id(900)),
            Kind::Link(Link {
                from: Entity::Operation(id(1)),
                relation: "BasedOn".into(),
                to: vec![Entity::Git {
                    repository: RepositoryId(repository),
                    oid: GitOid::from_hex(&branch)
                        .ok_or_else(|| io::Error::other("fixture commit"))?,
                }],
                content: Payload::Empty,
            }),
        )
        .into_op()
        .map_err(io::Error::other)
    };
    let _appended = engine.append(&link(100, repository.wrapping_add(1))?)?;
    equal!(
        latest(&installed)?.activities,
        3,
        "equal object hashes do not alias foreign repositories"
    );
    let _appended = engine.append(&link(101, repository)?)?;
    let revision = ChainQueries::open(&chain)?.index().revision().clone();
    let loaded = latest(&installed)?;
    equal!(
        loaded.activities,
        4,
        "a newly recorded context loads the other branch without changing HEAD"
    );
    let target = format!("git:{repository}:{branch}");
    check!(
        loaded.rows.iter().any(|row| row.occurrence == target),
        "the exact recorded commit is visible"
    );
    check!(
        row(&loaded, 1)?
            .relationships
            .iter()
            .any(|relation| relation.parent.as_ref() == Some(&target)),
        "the activity attaches to its recorded commit"
    );
    equal!(
        git(root, &["rev-parse", "HEAD"])?,
        head,
        "resolving recorded context leaves checkout state alone"
    );
    let _checked_out = git(root, &["checkout", "--quiet", "--detach", &first])?;
    let changed = latest(&installed)?;
    equal!(
        changed.activities,
        3,
        "retiring old HEAD ancestry retains the explicitly recorded branch"
    );
    check!(
        changed.rows.iter().any(|row| row.occurrence == target),
        "recorded context survives a checkout change"
    );
    equal!(
        ChainQueries::open(&chain)?.index().revision(),
        revision,
        "Git discovery never appends recorded operations"
    );
    Ok(())
}

#[test]
fn commit_file_children_page_independently_and_open_immutable_sides() -> io::Result<()> {
    use idle_history::timeline::{Action, Position, Response, View};
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let _initialized = git(root, &["init", "--quiet"])?;
    for (name, text) in [
        ("a.txt", "before\n"),
        ("old.txt", "rename me\n"),
        ("gone.txt", "delete me\n"),
        ("binary.bin", "a\0b"),
    ] {
        std::fs::write(root.join(name), text)?;
    }
    let _added = git(root, &["add", "."])?;
    let _committed = git(root, &["commit", "--quiet", "-m", "initial files"])?;
    std::fs::write(root.join("a.txt"), "after\n")?;
    std::fs::write(root.join("binary.bin"), "c\0d")?;
    let _renamed = git(root, &["mv", "old.txt", "renamed.txt"])?;
    let _removed = git(root, &["rm", "--quiet", "gone.txt"])?;
    let _committed = git(root, &["commit", "--quiet", "-am", "change files"])?;
    let chain = root.join(".editchain");
    drop(Engine::open(&chain)?);
    let mut installed = binding(&chain);
    installed.repository_directory = Some(root.to_owned());
    let baseline = latest(&installed)?;
    let parent = baseline
        .rows
        .first()
        .ok_or_else(|| io::Error::other("commit row"))?;
    let view = View {
        details: vec![parent.occurrence.clone()],
        ..View::default()
    };
    let mut position = Position::Latest;
    let mut children = Vec::new();
    let mut root_row = None;
    loop {
        let Response::Window(page) = super::request(
            &installed,
            Action::Window {
                view: view.clone(),
                position,
                limit: 2,
            },
        )?
        else {
            return Err(io::Error::other("detail page"));
        };
        equal!(
            page.activities,
            2,
            "file rows do not inflate physical activity counts"
        );
        equal!(page.visible_rows, 6, "two commits and four attached files");
        for row in page.rows {
            if row.occurrence == parent.occurrence {
                root_row = Some(row);
            } else if row.presentation.parent.is_some() {
                children.push(row);
            }
        }
        let Some(cursor) = page.older else {
            break;
        };
        position = Position::Page(cursor);
    }
    let root_row = root_row.ok_or_else(|| io::Error::other("expanded root"))?;
    check!(
        root_row
            .details
            .is_some_and(|details| details.expanded && details.count == Some(4)),
        "commit advertises its independent disclosure"
    );
    equal!(
        children.len(),
        4,
        "every child appears once across page boundaries"
    );
    for child in &children {
        equal!(
            child.presentation.parent.as_ref(),
            Some(&parent.occurrence),
            "children retain the physical parent"
        );
        equal!(
            child.timestamp,
            parent.timestamp,
            "children inherit the commit date"
        );
        equal!(
            &child.graph.above,
            &parent.graph.below,
            "a child starts at its parent's lower rails"
        );
        equal!(
            child.graph.above,
            child.graph.below,
            "detail rails pass straight through"
        );
        check!(
            child.graph.transitions.is_empty() && child.group.is_none(),
            "details add no branch or task member"
        );
    }
    std::fs::write(root.join("a.txt"), "unrelated working copy\n")?;
    for child in &children {
        let Target::CommitFile(change) = &child.address else {
            return Err(io::Error::other("file address"));
        };
        let QueryResult::CommitFile { before, after, .. } =
            crate::git::files::document(&installed, change)?
        else {
            return Err(io::Error::other("file text"));
        };
        match change.path.as_str() {
            "a.txt" => {
                equal!(before, "before\n", "exact first-parent side");
                equal!(after, "after\n", "exact commit side ignores working files");
            }
            "binary.bin" => {
                check!(change.binary, "binary classification is recorded");
                check!(
                    before.is_empty() && after.is_empty(),
                    "binary bytes are not converted to text"
                );
            }
            "gone.txt" => {
                equal!(change.status, "D", "deleted status");
                check!(after.is_empty(), "deletion has an empty new side");
            }
            "renamed.txt" => {
                equal!(change.status, "R", "rename status");
                equal!(
                    change.old_path.as_deref(),
                    Some("old.txt"),
                    "rename retains its former path"
                );
            }
            _ => return Err(io::Error::other("unexpected file")),
        }
        let mut forged = change.as_ref().clone();
        forged.old_oid = Some("a".repeat(40));
        check!(
            crate::git::files::document(&installed, &forged).is_err(),
            "a changed blob descriptor is rejected"
        );
    }
    let folded = latest(&installed)?;
    equal!(
        folded.visible_rows,
        2,
        "closing details restores the original row count"
    );
    equal!(
        &folded
            .rows
            .first()
            .ok_or_else(|| io::Error::other("folded root"))?
            .graph,
        &parent.graph,
        "disclosure never reallocates the graph"
    );
    Ok(())
}
