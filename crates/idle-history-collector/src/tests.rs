use std::{io, path::Path, process::Command};

use editchain_core::{
    ActorId, Clock, ImportOp, MessageOp, NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef,
    Tags,
};
use editchain_engine::{Engine, queries::ChainQueries};
use editchain_store::{BlobStorage as _, BlobStore};
use idle_history_import::FsBlobSink;

use crate::{Binding, Collector, Mode, Poll, git_links, monitor::Monitor};

#[test]
fn automatic_collection_has_one_owner_and_observation_releases_it() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let chain = directory.path().join("chain");
    drop(Engine::open(&chain).expect("empty chain"));
    let binding = Binding {
        workspace: directory.path().to_path_buf(),
        chain,
        sessions: directory.path().join("missing-sources"),
        helper: directory.path().join("unused-helper"),
    };
    let mut first = Collector::new(binding.clone()).expect("first collector");
    let mut second = Collector::new(binding).expect("second collector");
    let _initial = first.scan(Mode::Import).expect("first owner");
    assert_eq!(
        second
            .scan(Mode::Import)
            .expect_err("exclusive owner")
            .kind(),
        io::ErrorKind::WouldBlock
    );
    let _observed = second
        .poll(&Poll::default())
        .expect("other hosts can observe");
    let _paused = first
        .scan(Mode::Observe)
        .expect("release automatic collection");
    let _resumed = second.scan(Mode::Import).expect("new owner");
    drop(second);
    let _recovered = first
        .scan(Mode::Import)
        .expect("process lifetime releases ownership");
}

fn message(sequence: u64, content: Payload) -> Op {
    Op {
        id: OpId::new(NodeId(7), 0, sequence),
        source: Some(editchain_core::SourceId::new(NodeId(7), 0, sequence)),
        actor: ActorId(1),
        clock: Clock::UnixMs(sequence),
        scope: ScopeRef::None,
        parents: ParentSet::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content,
            content_type: Payload::Empty,
        }),
    }
}

#[test]
fn monitor_sees_lower_ids_conflicts_and_changes_consumed_by_other_readers() {
    let directory = tempfile::tempdir().expect("fixture succeeds");
    let engine = Engine::open(directory.path()).expect("fixture succeeds");
    let high = message(90, Payload::Inline(b"high".to_vec()));
    let _admission = engine.append(&high).expect("fixture succeeds");
    let mut monitor = Monitor::new(directory.path()).expect("fixture succeeds");
    assert!(
        !monitor.poll(directory.path()).expect("fixture succeeds"),
        "initial records are already known"
    );
    let low = message(3, Payload::Inline(b"late".to_vec()));
    let _admission = engine.append(&low).expect("fixture succeeds");
    drop(ChainQueries::open(directory.path()).expect("fixture succeeds"));
    assert!(
        monitor.poll(directory.path()).expect("fixture succeeds"),
        "another index reader cannot consume an invalidation"
    );
    assert!(
        !monitor.poll(directory.path()).expect("fixture succeeds"),
        "unchanged history does not repeat updates"
    );
    let _admission = engine
        .append(&message(3, Payload::Inline(b"conflict".to_vec())))
        .expect("fixture succeeds");
    assert!(
        monitor.poll(directory.path()).expect("fixture succeeds"),
        "same-ID conflict retracts the visible record"
    );
}

#[test]
fn monitor_sees_late_blob_without_a_new_operation() {
    let directory = tempfile::tempdir().expect("fixture succeeds");
    let engine = Engine::open(directory.path()).expect("fixture succeeds");
    let bytes = b"late content";
    let reference = editchain_core::BlobRef {
        id: editchain_core::ContentId::Hash256(*blake3::hash(bytes).as_bytes()),
        len: u32::try_from(bytes.len()).expect("small fixture"),
    };
    let _admission = engine
        .append(&message(7, Payload::Blob(reference)))
        .expect("fixture succeeds");
    let mut monitor = Monitor::new(directory.path()).expect("fixture succeeds");
    assert!(
        !monitor.poll(directory.path()).expect("fixture succeeds"),
        "content remains missing"
    );
    let _stored = BlobStore::new(directory.path().join("blobs"))
        .expect("fixture succeeds")
        .put(bytes)
        .expect("fixture succeeds");
    assert!(
        monitor.poll(directory.path()).expect("fixture succeeds"),
        "late bytes invalidate the history content"
    );
    assert!(
        !monitor.poll(directory.path()).expect("fixture succeeds"),
        "resolved bytes need no repeated invalidation"
    );
}

#[test]
fn empty_poll_creates_no_chain_and_rejects_sources_outside_its_binding() {
    let directory = tempfile::tempdir().expect("fixture succeeds");
    let chain = directory.path().join("chain");
    let sessions = directory.path().join("sessions");
    std::fs::create_dir(&sessions).expect("fixture succeeds");
    let outside = directory.path().join("rollout-outside.jsonl");
    std::fs::write(&outside, b"{}\n").expect("fixture succeeds");
    let mut collector = Collector::new(Binding {
        workspace: directory.path().to_owned(),
        chain: chain.clone(),
        sessions,
        helper: directory.path().join("absent-exporter"),
    })
    .expect("fixture succeeds");
    assert!(
        !collector
            .poll(&Poll::default())
            .expect("fixture succeeds")
            .changed,
        "empty polling is read-only"
    );
    assert!(
        !chain.exists(),
        "disabled collection must not create a history chain"
    );
    let error = collector
        .poll(&Poll {
            paths: vec![outside],
            ..Poll::default()
        })
        .expect_err("foreign source");
    assert_eq!(
        error.kind(),
        io::ErrorKind::InvalidInput,
        "source binding is enforced before execution"
    );
    assert!(
        !chain.exists(),
        "invalid sources have no storage side effects"
    );
}

fn git(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .output()
        .expect("Git fixture");
    assert!(
        output.status.success(),
        "Git fixture command failed: {arguments:?}"
    );
    String::from_utf8(output.stdout)
        .expect("Git text")
        .trim()
        .to_owned()
}

#[test]
fn schema_three_originals_reconcile_git_commits_once_and_exclude_conflicts() {
    let directory = tempfile::tempdir().expect("fixture succeeds");
    drop(git(directory.path(), &["init", "-q"]));
    std::fs::write(directory.path().join("file.txt"), b"content").expect("fixture succeeds");
    drop(git(directory.path(), &["add", "file.txt"]));
    drop(git(
        directory.path(),
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-qm",
            "initial",
        ],
    ));
    let oid = git(directory.path(), &["rev-parse", "HEAD"]);
    let chain = directory.path().join("chain");
    let engine = Engine::open(&chain).expect("fixture succeeds");
    let bytes = serde_json::to_vec(&serde_json::json!({
        "type": "event_msg", "payload": { "type": "item_completed", "item": {
            "type": "CommandExecution", "command": "git commit -m initial", "status": "completed", "exit_code": 0,
            "aggregated_output": format!("[main {}] initial\n", oid.get(..8).expect("Git prefix")),
        } }
    })).expect("fixture succeeds");
    let mut original = message(4, Payload::Empty);
    original.kind = OpKind::Import(ImportOp {
        raw_ref: Payload::Inline(bytes),
        raw_hash: None,
    });
    original.tags = Tags::IMPORT;
    let mut blobs = FsBlobSink::new(chain.join("blobs")).expect("fixture succeeds");
    let records = idle_history_import::activity::convert(&[original], &mut blobs)
        .map_err(io::Error::other)
        .expect("fixture succeeds");
    for record in &records {
        let _admission = engine.append(record).expect("fixture succeeds");
    }
    let links = git_links::derive(directory.path(), &chain, &[], true).expect("fixture succeeds");
    assert_eq!(
        links.len(),
        1,
        "a retained schema-three Original can supply the commit output"
    );
    for link in &links {
        let _admission = engine.append(link).expect("fixture succeeds");
    }
    assert!(
        git_links::derive(directory.path(), &chain, &[], true)
            .expect("fixture succeeds")
            .is_empty(),
        "reconciliation is idempotent"
    );
    let mut conflict =
        editchain_core::activity::Operation::view(records.first().expect("converted Original"))
            .expect("activity");
    conflict.time_ms = Some(999);
    let conflicted = conflict
        .into_op()
        .map_err(io::Error::other)
        .expect("fixture succeeds");
    let _admission = engine.append(&conflicted).expect("fixture succeeds");
    assert!(
        git_links::derive(directory.path(), &chain, &[], true)
            .expect("fixture succeeds")
            .is_empty(),
        "quarantined sources cannot establish new relationships"
    );
}
