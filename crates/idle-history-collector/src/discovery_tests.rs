use std::{fs, path::Path};

use super::{belongs_to_workspace, capture};
use crate::{Binding, Mode};

fn header(workspace: &Path) -> String {
    format!(
        "{}\n",
        serde_json::json!({"type": "session_meta", "payload": {"cwd": workspace}})
    )
}

#[test]
fn source_rewrites_worktree_refs_and_nested_repositories_are_observed() {
    let root = tempfile::tempdir().expect("temporary directory");
    let workspace = root.path().join("repo");
    let sessions = root.path().join("sessions");
    let common = root.path().join("git");
    let metadata = common.join("worktrees/repo");
    for path in [&workspace, &sessions, &metadata, &common.join("refs/heads")] {
        fs::create_dir_all(path).expect("fixture directory");
    }
    fs::write(
        workspace.join(".git"),
        format!("gitdir: {}\n", metadata.display()),
    )
    .expect("worktree pointer");
    fs::write(metadata.join("commondir"), "../..\n").expect("common directory pointer");
    fs::write(metadata.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    let reference = common.join("refs/heads/main");
    fs::write(&reference, "a").expect("reference");
    let source = sessions.join("rollout-one.jsonl");
    fs::write(&source, header(&workspace)).expect("source");
    let binding = Binding {
        workspace: workspace.clone(),
        chain: root.path().join("chain"),
        sessions,
        helper: root.path().join("unused-helper"),
    };
    let before = capture(&binding, Mode::Import).expect("initial discovery");
    assert_eq!(before.files.len(), 1);
    assert!(belongs_to_workspace(&source, &workspace).expect("bound source"));
    fs::write(&source, header(root.path())).expect("rewritten source");
    fs::write(&reference, "new-reference").expect("changed reference");
    let after = capture(&binding, Mode::Import).expect("updated discovery");
    assert_ne!(before.files.get(&source), after.files.get(&source));
    assert_ne!(before.git, after.git);
    assert!(!belongs_to_workspace(&source, &workspace).expect("foreign source"));

    let nested = workspace.join("nested/.git");
    fs::create_dir_all(nested.join("refs/heads")).expect("nested repository");
    fs::write(nested.join("HEAD"), "ref: refs/heads/main\n").expect("nested HEAD");
    let nested_reference = nested.join("refs/heads/main");
    fs::write(&nested_reference, "first").expect("nested reference");
    let discovered = capture(&binding, Mode::Observe).expect("nested discovery");
    assert_ne!(discovered.git, after.git);
    fs::write(&nested_reference, "second-reference").expect("new nested commit");
    let changed = capture(&binding, Mode::Observe).expect("nested update");
    assert_ne!(changed.git, discovered.git);
    assert!(
        changed.files.is_empty(),
        "observation mode never reads rollouts"
    );
}

#[test]
fn missing_child_paths_are_normalized_before_workspace_selection() {
    let root = tempfile::tempdir().expect("temporary directory");
    let workspace = root.path().join("repo");
    fs::create_dir(&workspace).expect("workspace");
    let source = root.path().join("rollout-one.jsonl");
    fs::write(
        &source,
        header(&workspace.join("not-created/../../foreign")),
    )
    .expect("source outside the workspace");
    assert!(!belongs_to_workspace(&source, &workspace).expect("selection"));
    fs::write(&source, header(&workspace.join("not-created/../child")))
        .expect("source inside the workspace");
    assert!(belongs_to_workspace(&source, &workspace).expect("selection"));
    fs::write(&source, "{\"future-format\":true}\n").expect("unknown header");
    assert!(
        belongs_to_workspace(&source, &workspace).expect("selection"),
        "the provider validates headers that the prefilter does not recognize"
    );
}

#[cfg(unix)]
#[test]
fn discovery_does_not_follow_directory_symlinks() {
    let root = tempfile::tempdir().expect("temporary directory");
    let sessions = root.path().join("sessions");
    fs::create_dir_all(sessions.join("nested")).expect("sources");
    fs::write(sessions.join("rollout-one.jsonl"), header(root.path())).expect("source");
    std::os::unix::fs::symlink(&sessions, sessions.join("nested/cycle")).expect("cycle");
    let binding = Binding {
        workspace: root.path().to_path_buf(),
        chain: root.path().join("chain"),
        sessions,
        helper: root.path().join("unused-helper"),
    };
    assert_eq!(
        capture(&binding, Mode::Import)
            .expect("bounded traversal")
            .files
            .len(),
        1
    );
}

#[test]
fn bare_repositories_and_parent_title_indexes_are_observed() {
    let root = tempfile::tempdir().expect("temporary directory");
    let sessions = root.path().join("sessions");
    for name in ["sessions", "bare/objects", "bare/refs/heads"] {
        fs::create_dir_all(root.path().join(name)).expect("fixture directory");
    }
    fs::write(root.path().join("bare/HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    fs::write(root.path().join("bare/refs/heads/main"), "one").expect("reference");
    let binding = Binding {
        workspace: root.path().to_path_buf(),
        chain: root.path().join("chain"),
        sessions,
        helper: root.path().join("unused-helper"),
    };
    let before = capture(&binding, Mode::Import).expect("initial discovery");
    fs::write(root.path().join("bare/refs/heads/main"), "second").expect("new reference");
    fs::write(root.path().join("session_index.jsonl"), "{}\n").expect("title index");
    let after = capture(&binding, Mode::Import).expect("new discovery");
    assert_ne!(before.git, after.git);
    assert_ne!(before.titles, after.titles);
}
