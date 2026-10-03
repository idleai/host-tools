use std::{collections::BTreeSet, fs};

use super::Schedule;
use crate::{Binding, Mode, Update};

struct Fixture {
    _directory: tempfile::TempDir,
    binding: Binding,
}

impl Fixture {
    fn new(count: usize) -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let binding = Binding {
            workspace: directory.path().join("repo"),
            chain: directory.path().join("chain"),
            sessions: directory.path().join("sessions"),
            helper: directory.path().join("unused-helper"),
        };
        fs::create_dir(&binding.workspace).expect("workspace");
        fs::create_dir(&binding.sessions).expect("sources");
        let header = serde_json::json!({
            "type": "session_meta", "payload": {"cwd": binding.workspace}
        });
        for index in 0..count {
            fs::write(
                binding.sessions.join(format!("rollout-{index:03}.jsonl")),
                format!("{header}\n"),
            )
            .expect("source");
        }
        Self {
            _directory: directory,
            binding,
        }
    }
}

#[test]
fn pending_batches_accept_only_the_original_stamp_after_an_append() {
    let fixture = Fixture::new(1);
    let mut schedule = Schedule::default();
    let first = schedule
        .request(&fixture.binding, Mode::Import)
        .expect("first pass");
    assert_eq!(first.paths.len(), 1);
    assert!(first.git_changed);
    let mut pending = Update {
        pending: true,
        changed: true,
        ..Update::default()
    };
    schedule.complete(&mut pending);
    let path = first.paths.first().expect("selected source");
    let mut bytes = fs::read(path).expect("source bytes");
    bytes.extend_from_slice(b"{\"new-record\":true}\n");
    fs::write(path, bytes).expect("append during capture");
    let continuation = schedule
        .request(&fixture.binding, Mode::Import)
        .expect("pending pass");
    assert_eq!(continuation.paths, first.paths);
    assert!(!continuation.git_changed);
    schedule.complete(&mut Update::default());
    let appended = schedule
        .request(&fixture.binding, Mode::Import)
        .expect("new stamp");
    assert_eq!(appended.paths, first.paths);
    schedule.complete(&mut Update::default());
    assert!(
        schedule
            .request(&fixture.binding, Mode::Import)
            .expect("unchanged pass")
            .paths
            .is_empty(),
        "the changed source is accepted after its own durable pass"
    );
}

#[test]
fn failed_sources_are_retried_but_removed_sources_do_not_block_recovery() {
    let fixture = Fixture::new(1);
    let mut schedule = Schedule::default();
    let first = schedule
        .request(&fixture.binding, Mode::Import)
        .expect("first pass");
    schedule.failed();
    let retry = schedule
        .request(&fixture.binding, Mode::Import)
        .expect("retry");
    assert_eq!(retry.paths, first.paths);
    assert!(retry.git_changed, "a failed pass did not accept Git state");
    schedule.failed();
    fs::remove_file(first.paths.first().expect("source")).expect("removed source");
    let recovered = schedule
        .request(&fixture.binding, Mode::Import)
        .expect("recovery");
    assert!(
        recovered.paths.is_empty(),
        "removed paths leave the retry set"
    );
}

#[test]
fn title_changes_revisit_every_source_in_bounded_batches() {
    let fixture = Fixture::new(70);
    let mut schedule = Schedule::default();
    for round in 0..2 {
        let mut selected = BTreeSet::new();
        for expected in [32, 32, 6, 0] {
            let request = schedule
                .request(&fixture.binding, Mode::Import)
                .expect("batch");
            assert_eq!(request.paths.len(), expected);
            selected.extend(request.paths);
            schedule.complete(&mut Update::default());
        }
        assert_eq!(
            selected.len(),
            70,
            "every source is revisited in round {round}"
        );
        fs::write(
            fixture.binding.sessions.join("session_index.jsonl"),
            format!("{{\"title-revision\":{round}}}\n"),
        )
        .expect("title update");
    }
}

#[test]
fn observation_mode_retires_pending_imports_and_resume_rediscovers_sources() {
    let fixture = Fixture::new(1);
    let mut schedule = Schedule::default();
    let first = schedule
        .request(&fixture.binding, Mode::Import)
        .expect("import");
    schedule.complete(&mut Update {
        pending: true,
        ..Update::default()
    });
    assert!(
        schedule
            .request(&fixture.binding, Mode::Observe)
            .expect("observation")
            .paths
            .is_empty(),
        "pausing import retires a pending source batch"
    );
    schedule.complete(&mut Update::default());
    assert_eq!(
        schedule
            .request(&fixture.binding, Mode::Import)
            .expect("resume")
            .paths,
        first.paths
    );
}

#[test]
fn foreign_sources_are_accepted_as_filtered_without_an_endless_pending_loop() {
    let fixture = Fixture::new(1);
    let source = fixture.binding.sessions.join("rollout-000.jsonl");
    let foreign = fixture.binding.sessions.parent().expect("fixture root");
    fs::write(
        source,
        format!(
            "{}\n",
            serde_json::json!({"type":"session_meta","payload":{"cwd":foreign}})
        ),
    )
    .expect("foreign source");
    let mut schedule = Schedule::default();
    assert!(
        schedule
            .request(&fixture.binding, Mode::Import)
            .expect("filtered pass")
            .paths
            .is_empty()
    );
    let mut update = Update::default();
    schedule.complete(&mut update);
    assert!(!update.pending, "the filtered source was examined");
    assert!(
        schedule
            .request(&fixture.binding, Mode::Import)
            .expect("unchanged pass")
            .paths
            .is_empty()
    );
}
