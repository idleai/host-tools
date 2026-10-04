use std::{fs, path::Path, process::Command};

use super::{
    read,
    remote::{GithubRemote, sanitize},
};
use idle_protocol::v1::repository::ReadState;

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=Recorded Author",
            "-c",
            "user.email=author@example.test",
            "-C",
        ])
        .arg(root)
        .args(args)
        .output()
        .expect("Git fixture");
    assert!(
        output.status.success(),
        "Git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn committed(root: &Path) {
    git(root, &["init", "--initial-branch=main"]);
    fs::write(root.join("tracked.txt"), b"initial\n").expect("fixture file");
    git(root, &["add", "tracked.txt"]);
    git(root, &["commit", "-m", "initial"]);
}

#[tokio::test]
async fn unborn_dirty_and_detached_checkout_reads_preserve_exact_scope() {
    let directory = tempfile::tempdir().expect("temporary repository");
    git(directory.path(), &["init", "--initial-branch=main"]);
    let empty = read(directory.path(), 100).await;
    let checkout = empty.checkout.expect("unborn checkout");
    assert_eq!(
        checkout.branch.as_deref(),
        Some("main"),
        "unborn branch remains named"
    );
    assert!(checkout.head.is_none(), "unborn branch has no commit");
    assert!(empty.authors.is_empty(), "unborn history has no authors");
    committed(directory.path());
    fs::write(directory.path().join("tracked.txt"), b"changed\n").expect("changed file");
    fs::write(directory.path().join("untracked name.txt"), b"new\n").expect("new file");
    let changed = read(directory.path(), 101).await;
    let checkout = changed.checkout.expect("checkout");
    let status = checkout.status.expect("worktree status");
    assert_eq!(
        (status.staged, status.unstaged, status.untracked),
        (0, 1, 1),
        "status counts actual changes"
    );
    assert_eq!(
        changed.authors.first().expect("author").name,
        "Recorded Author",
        "commit author is read from this repository"
    );
    git(directory.path(), &["checkout", "--detach", "HEAD"]);
    let detached = read(directory.path(), 102)
        .await
        .checkout
        .expect("detached checkout");
    assert!(
        detached.branch.is_none() && detached.head.is_some(),
        "detached and unborn remain distinct"
    );
}

#[tokio::test]
async fn linked_worktrees_and_missing_roots_never_fall_back() {
    let directory = tempfile::tempdir().expect("repository parent");
    let main = directory.path().join("main");
    let linked = directory.path().join("linked");
    fs::create_dir(&main).expect("main folder");
    committed(&main);
    git(
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "feature",
            linked.to_str().expect("UTF-8 fixture"),
        ],
    );
    let first = read(&main, 100).await.checkout.expect("main checkout");
    let second = read(&linked, 100).await.checkout.expect("linked checkout");
    assert_ne!(first.root, second.root, "checkout roots remain distinct");
    assert_ne!(
        first.git_directory, second.git_directory,
        "worktree metadata remains distinct"
    );
    assert_eq!(
        first.common_directory, second.common_directory,
        "shared object directory is recorded"
    );
    assert_eq!(
        second.branch.as_deref(),
        Some("feature"),
        "the selected worktree supplies the branch"
    );
    fs::rename(&linked, directory.path().join("moved")).expect("rename root");
    let removed = read(&linked, 101).await;
    assert!(
        removed.checkout.is_none(),
        "renamed roots do not reuse a different checkout"
    );
    assert_eq!(
        removed.reports.first().expect("read report").state,
        ReadState::Unavailable,
        "removed root is unavailable"
    );
    assert!(
        read(directory.path(), 102).await.checkout.is_none(),
        "non-Git parent does not discover a child implicitly"
    );
}

#[tokio::test]
async fn configured_remote_is_sanitized_before_it_leaves_git() {
    let directory = tempfile::tempdir().expect("repository");
    committed(directory.path());
    git(
        directory.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://private-user:private-token@github.com/owner/repository.git?private=query#private-fragment",
        ],
    );
    let result = read(directory.path(), 100).await;
    let checkout = result.checkout.expect("checkout");
    assert_eq!(
        checkout.remote.as_deref(),
        Some("https://github.com/owner/repository.git"),
        "credentials, query and fragment never reach the view"
    );
    assert_eq!(
        result.github.expect("GitHub remote").name,
        "repository",
        "normalized repository name"
    );
}

#[test]
fn ssh_remote_normalization_does_not_authorize_other_hosts() {
    for remote in [
        "git@github.com:owner/repo.git",
        "ssh://git@github.com/owner/repo.git",
        "https://github.com/owner/repo",
    ] {
        let sanitized = sanitize(remote).expect("supported remote");
        assert_eq!(
            GithubRemote::parse(&sanitized).expect("GitHub").owner,
            "owner",
            "standard GitHub remote form"
        );
    }
    for remote in [
        "https://github.com.evil.test/owner/repo",
        "https://github.com:1234/owner/repo",
        "https://github.com/owner/../repo",
        "https://elsewhere.test/owner/repo",
    ] {
        assert!(
            GithubRemote::parse(remote).is_none(),
            "remote cannot select an arbitrary API origin: {remote}"
        );
    }
}
