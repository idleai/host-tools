use std::path::Path;

use editchain_core::{
    OpId, Payload,
    activity::{ItemId, Kind, Operation, Session, SessionAction},
};
use editchain_engine::{
    Engine,
    queries::{ContentField, ContentQuery, Lookup},
};
use idle_protocol::v1::{
    identity::ProjectionCount,
    projections::{
        FreshnessStatus, ProjectionAvailability, ProjectionFreshness, ProjectionInput,
        ProjectionKind, ProjectionRow,
    },
    repository::ReadState,
};

fn session(root: &Path, ordinal: u8, label: &str) {
    let engine = Engine::open(root).expect("chain");
    let item = ItemId::derive("session", b"local-session");
    let mut operation = Operation::new(
        OpId::from_bytes([ordinal; 32]),
        item,
        ItemId::derive("recorder", b"local-editor"),
        Kind::Session(Session {
            action: if ordinal == 1 {
                SessionAction::Started
            } else {
                SessionAction::Changed
            },
            label: Payload::Inline(label.as_bytes().to_vec()),
            settings: Payload::Empty,
            participants: Vec::new(),
            parent: None,
            initiated_by: None,
        }),
    );
    operation.session = Some(item);
    let _admission = engine
        .append(&operation.into_op().expect("valid session"))
        .expect("stored session");
}

#[test]
fn recorded_session_titles_and_exact_observations_are_retained_without_live_status() {
    let directory = tempfile::tempdir().expect("temporary chain");
    session(directory.path(), 1, "Original title");
    session(directory.path(), 2, "Updated title");
    let (sessions, report) = super::records::sessions(directory.path(), 100);
    assert_eq!(
        report.state,
        ReadState::Complete,
        "complete accepted session read"
    );
    assert_eq!(sessions.len(), 1, "observations group by logical session");
    let session = sessions.first().expect("session");
    assert_eq!(
        session.labels,
        ["Original title", "Updated title"],
        "no title is selected by arbitrary ID order"
    );
    assert_eq!(
        session.actions,
        ["Started", "Changed"],
        "observed events remain explicit"
    );
    assert_eq!(
        session.records.len(),
        2,
        "every exact observation remains reachable"
    );
    assert_eq!(
        session.records.first().expect("source").item,
        session.id,
        "history uses the logical session identity"
    );
    assert_eq!(
        session.records.first().expect("source").record_hash.len(),
        64,
        "full stored hash"
    );
}

#[test]
fn github_original_bytes_round_trip_and_changed_references_are_rejected() {
    let directory = tempfile::tempdir().expect("temporary chain");
    let raw = b" [ {\"id\":42, \"title\":\"exact whitespace\"} ]\r\n";
    let source = super::records::capture(
        directory.path(),
        "repository",
        "https://api.github.com/repos/owner/repo/issues?per_page=50&page=1",
        raw,
    )
    .expect("retained source");
    let replay = super::records::capture(
        directory.path(),
        "repository",
        "https://api.github.com/repos/owner/repo/issues?per_page=50&page=1",
        raw,
    )
    .expect("exact replay");
    assert_eq!(
        source, replay,
        "conditional rereads reuse the exact original source"
    );
    let queries = Engine::open(directory.path())
        .expect("chain")
        .queries()
        .expect("queries");
    let operation = OpId::from_display_str(source.observation.as_deref().expect("observation"))
        .expect("full identity");
    let content = match queries
        .content(ContentQuery {
            operation,
            field: ContentField::Record(editchain_core::activity::Field::Content),
        })
        .expect("original content")
    {
        Lookup::Found(content) => Some(content),
        Lookup::Missing | Lookup::Conflicted(_) => None,
    }
    .expect("source must resolve");
    assert_eq!(
        content.value.bytes(),
        Some(raw.as_slice()),
        "exact source bytes are retained"
    );
    let mut inputs = vec![ProjectionInput {
        kind: ProjectionKind::Task,
        freshness: ProjectionFreshness {
            status: FreshnessStatus::Current,
            generated_at: None,
            checkpoint: None,
        },
        availability: ProjectionAvailability::Complete,
        total: Some(ProjectionCount(1)),
        rows: vec![ProjectionRow {
            key: "task".into(),
            title: "Exact task".into(),
            summary: None,
            url: Some("https://github.com/owner/repo/issues/42".into()),
            status: None,
            labels: Vec::new(),
            sources: vec![source],
            related: Vec::new(),
        }],
        gaps: Vec::new(),
    }];
    super::validate_sources(&queries, &inputs).expect("exact sources are accepted");
    inputs
        .first_mut()
        .expect("input")
        .rows
        .first_mut()
        .expect("row")
        .sources
        .first_mut()
        .expect("source")
        .record_hash = Some("00".repeat(32));
    assert!(
        super::validate_sources(&queries, &inputs).is_err(),
        "changed digests cannot select another source variant"
    );
}

#[test]
fn older_session_registrations_use_their_original_label_and_full_logical_item() {
    use editchain_core::{
        ActorId, Clock, Op, OpKind, ParentSet, ScopeRef, SessionId, SessionOp, Tags,
    };
    let directory = tempfile::tempdir().expect("temporary chain");
    let engine = Engine::open(directory.path()).expect("chain");
    let operation = Op {
        id: OpId::from_bytes([9; 32]),
        source: None,
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1),
        scope: ScopeRef::Session(SessionId(42)),
        tags: Tags::NONE,
        kind: OpKind::Session(SessionOp {
            id: SessionId(42),
            parent: None,
            label: Payload::Inline(b"Older captured session".to_vec()),
            metadata: Payload::Empty,
        }),
    };
    let expected = Operation::view(&operation)
        .expect("supported session")
        .item
        .to_string();
    let _admission = engine.append(&operation).expect("stored registration");
    drop(engine);
    let (sessions, report) = super::records::sessions(directory.path(), 100);
    assert_eq!(report.state, ReadState::Complete, "all candidates read");
    let session = sessions.first().expect("older registration");
    assert_eq!(
        session.id, expected,
        "legacy display conversion retains its full logical item"
    );
    assert_eq!(
        session.labels,
        ["Older captured session"],
        "resolve the original encoding's content field"
    );
}

#[test]
fn changing_one_source_removes_only_its_rows_and_marks_the_result_partial() {
    let directory = tempfile::tempdir().expect("chain");
    let source = super::records::capture(
        directory.path(),
        "repo",
        "https://api.github.com/repos/owner/repo/issues",
        b"[]",
    )
    .expect("stored response");
    let queries = Engine::open(directory.path())
        .expect("chain")
        .queries()
        .expect("queries");
    let mut changed = source.clone();
    changed.record_hash = Some("00".repeat(32));
    let row = |key: &str, source| ProjectionRow {
        key: key.into(),
        title: key.into(),
        summary: None,
        url: None,
        status: None,
        labels: Vec::new(),
        sources: vec![source],
        related: Vec::new(),
    };
    let input = ProjectionInput {
        kind: ProjectionKind::Task,
        freshness: ProjectionFreshness {
            status: FreshnessStatus::Current,
            generated_at: None,
            checkpoint: None,
        },
        availability: ProjectionAvailability::Complete,
        total: Some(ProjectionCount(2)),
        rows: vec![row("good", source), row("changed", changed)],
        gaps: Vec::new(),
    };
    let checked = super::checked_inputs(&queries, &[input]);
    let input = checked.first().expect("input");
    assert_eq!(
        input.rows.len(),
        1,
        "accepted independent rows remain readable"
    );
    assert_eq!(
        input.rows.first().expect("row").key,
        "good",
        "changed source is removed"
    );
    assert_eq!(
        input.availability,
        ProjectionAvailability::Partial,
        "source failure is never known empty"
    );
    assert!(
        input.total.is_none() && input.gaps.len() == 1,
        "lost coverage is explicit"
    );
}
